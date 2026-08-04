use std::{collections::HashSet, ffi::c_void};

use mlua::{Lua, LuaSerdeExt, Table, Value};
use n00n_storage::sessions::MAX_PLUGIN_STATE_BYTES;
use serde_json::Value as JsonValue;

pub(crate) const MAX_STATE_DEPTH: usize = 64;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum StateConvertError {
    #[error("state contains unsupported Lua value type '{0}'")]
    UnsupportedValue(&'static str),
    #[error("state contains a non-finite number")]
    NonFiniteNumber,
    #[error("state contains a string that is not valid UTF-8")]
    InvalidUtf8String,
    #[error("state object keys must be UTF-8 strings")]
    NonStringObjectKey,
    #[error("state array keys must be positive integers")]
    InvalidArrayKey,
    #[error("state arrays must have contiguous keys starting at 1")]
    SparseArray,
    #[error("state contains a cycle")]
    Cycle,
    #[error("state exceeds the maximum size of {maximum} bytes")]
    MaximumBytesExceeded { maximum: usize },
    #[error("state exceeds the maximum nesting depth of {maximum}")]
    MaximumDepthExceeded { maximum: usize },
    #[error("JSON number cannot be represented as a Lua number")]
    UnrepresentableNumber,
    #[error("Lua operation failed during state conversion: {0}")]
    Lua(String),
}

struct StateBudget {
    used: usize,
}

impl StateBudget {
    fn consume(&mut self, bytes: usize) -> Result<(), StateConvertError> {
        self.used = self.used.saturating_add(bytes);
        if self.used > MAX_PLUGIN_STATE_BYTES {
            Err(StateConvertError::MaximumBytesExceeded {
                maximum: MAX_PLUGIN_STATE_BYTES,
            })
        } else {
            Ok(())
        }
    }
}

impl From<mlua::Error> for StateConvertError {
    fn from(error: mlua::Error) -> Self {
        Self::Lua(error.to_string())
    }
}

pub(crate) fn json_to_lua(lua: &Lua, value: &JsonValue) -> Result<Value, StateConvertError> {
    json_to_lua_at_depth(lua, value, 0)
}

fn json_to_lua_at_depth(
    lua: &Lua,
    value: &JsonValue,
    depth: usize,
) -> Result<Value, StateConvertError> {
    check_depth(depth)?;
    match value {
        JsonValue::Null => Ok(lua.null()),
        JsonValue::Bool(value) => Ok(Value::Boolean(*value)),
        JsonValue::Number(value) => {
            if let Some(integer) = value.as_i64() {
                return Ok(Value::Integer(integer));
            }
            if value.as_u64().is_some() {
                return Err(StateConvertError::UnrepresentableNumber);
            }
            let number = value
                .as_f64()
                .filter(|number| number.is_finite())
                .ok_or(StateConvertError::UnrepresentableNumber)?;
            let round_trip = serde_json::Number::from_f64(number)
                .ok_or(StateConvertError::UnrepresentableNumber)?;
            if &round_trip != value {
                return Err(StateConvertError::UnrepresentableNumber);
            }
            Ok(Value::Number(number))
        }
        JsonValue::String(value) => Ok(Value::String(lua.create_string(value)?)),
        JsonValue::Array(values) => {
            let table = lua.create_table_with_capacity(values.len(), 0)?;
            table.set_metatable(Some(lua.array_metatable()))?;
            for (offset, value) in values.iter().enumerate() {
                table.raw_set(offset + 1, json_to_lua_at_depth(lua, value, depth + 1)?)?;
            }
            Ok(Value::Table(table))
        }
        JsonValue::Object(values) => {
            let table = lua.create_table_with_capacity(0, values.len())?;
            for (key, value) in values {
                table.raw_set(key.as_str(), json_to_lua_at_depth(lua, value, depth + 1)?)?;
            }
            Ok(Value::Table(table))
        }
    }
}

pub(crate) fn lua_to_json(lua: &Lua, value: &Value) -> Result<JsonValue, StateConvertError> {
    lua_to_json_at_depth(
        lua,
        value,
        0,
        &mut HashSet::new(),
        &mut StateBudget { used: 0 },
    )
}

fn lua_to_json_at_depth(
    lua: &Lua,
    value: &Value,
    depth: usize,
    active_tables: &mut HashSet<*const c_void>,
    budget: &mut StateBudget,
) -> Result<JsonValue, StateConvertError> {
    check_depth(depth)?;
    match value {
        value if value.is_null() => {
            budget.consume(4)?;
            Ok(JsonValue::Null)
        }
        Value::Boolean(value) => {
            budget.consume(if *value { 4 } else { 5 })?;
            Ok(JsonValue::Bool(*value))
        }
        Value::Integer(value) => {
            budget.consume(value.to_string().len())?;
            Ok(JsonValue::Number((*value).into()))
        }
        Value::Number(value) => {
            let number =
                serde_json::Number::from_f64(*value).ok_or(StateConvertError::NonFiniteNumber)?;
            budget.consume(number.to_string().len())?;
            Ok(JsonValue::Number(number))
        }
        Value::String(value) => {
            let value = value
                .to_str()
                .map_err(|_| StateConvertError::InvalidUtf8String)?;
            budget.consume(serialized_string_len(&value))?;
            Ok(JsonValue::String(value.to_owned()))
        }
        Value::Table(table) => table_to_json(lua, table, depth, active_tables, budget),
        value => Err(StateConvertError::UnsupportedValue(value.type_name())),
    }
}

fn table_to_json(
    lua: &Lua,
    table: &Table,
    depth: usize,
    active_tables: &mut HashSet<*const c_void>,
    budget: &mut StateBudget,
) -> Result<JsonValue, StateConvertError> {
    budget.consume(2)?;
    let pointer = table.to_pointer();
    if !active_tables.insert(pointer) {
        return Err(StateConvertError::Cycle);
    }

    let result = if is_array_table(lua, table)? {
        array_to_json(lua, table, depth, active_tables, budget)
    } else {
        object_to_json(lua, table, depth, active_tables, budget)
    };
    active_tables.remove(&pointer);
    result
}

fn is_array_table(lua: &Lua, table: &Table) -> Result<bool, StateConvertError> {
    if table
        .metatable()
        .is_some_and(|metatable| metatable == lua.array_metatable())
    {
        return Ok(true);
    }

    let length = table.raw_len();
    if length == 0 {
        return Ok(false);
    }
    let mut entries = 0;
    for pair in table.clone().pairs::<Value, Value>() {
        let (key, _) = pair?;
        let Value::Integer(key) = key else {
            return Ok(false);
        };
        let Ok(index) = usize::try_from(key) else {
            return Ok(false);
        };
        if !(1..=length).contains(&index) {
            return Ok(false);
        }
        entries += 1;
    }
    Ok(entries == length)
}

fn array_to_json(
    lua: &Lua,
    table: &Table,
    depth: usize,
    active_tables: &mut HashSet<*const c_void>,
    budget: &mut StateBudget,
) -> Result<JsonValue, StateConvertError> {
    let mut entries = Vec::new();
    for pair in table.pairs::<Value, Value>() {
        let (key, value) = pair?;
        let Value::Integer(key) = key else {
            return Err(StateConvertError::InvalidArrayKey);
        };
        let index = usize::try_from(key).map_err(|_| StateConvertError::InvalidArrayKey)?;
        if index == 0 {
            return Err(StateConvertError::InvalidArrayKey);
        }
        if !entries.is_empty() {
            budget.consume(1)?;
        }
        entries.push((index, value));
    }
    entries.sort_unstable_by_key(|(index, _)| *index);

    let mut values = Vec::with_capacity(entries.len());
    for (offset, (index, value)) in entries.into_iter().enumerate() {
        if index != offset + 1 {
            return Err(StateConvertError::SparseArray);
        }
        values.push(lua_to_json_at_depth(
            lua,
            &value,
            depth + 1,
            active_tables,
            budget,
        )?);
    }
    Ok(JsonValue::Array(values))
}

fn object_to_json(
    lua: &Lua,
    table: &Table,
    depth: usize,
    active_tables: &mut HashSet<*const c_void>,
    budget: &mut StateBudget,
) -> Result<JsonValue, StateConvertError> {
    let mut values = serde_json::Map::new();
    for pair in table.pairs::<Value, Value>() {
        let (key, value) = pair?;
        let Value::String(key) = key else {
            return Err(StateConvertError::NonStringObjectKey);
        };
        let key = key
            .to_str()
            .map_err(|_| StateConvertError::InvalidUtf8String)?;
        if !values.is_empty() {
            budget.consume(1)?;
        }
        budget.consume(serialized_string_len(&key).saturating_add(1))?;
        values.insert(
            key.to_owned(),
            lua_to_json_at_depth(lua, &value, depth + 1, active_tables, budget)?,
        );
    }
    Ok(JsonValue::Object(values))
}
fn serialized_string_len(value: &str) -> usize {
    value.chars().fold(2, |bytes, character| {
        bytes.saturating_add(match character {
            '"' | '\\' | '\u{0008}' | '\u{0009}' | '\u{000a}' | '\u{000c}' | '\u{000d}' => 2,
            '\u{0000}'..='\u{001f}' => 6,
            character => character.len_utf8(),
        })
    })
}

const fn check_depth(depth: usize) -> Result<(), StateConvertError> {
    if depth > MAX_STATE_DEPTH {
        Err(StateConvertError::MaximumDepthExceeded {
            maximum: MAX_STATE_DEPTH,
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use mlua::{Lua, LuaSerdeExt, Value};
    use n00n_storage::sessions::MAX_PLUGIN_STATE_BYTES;
    use serde_json::json;
    use test_case::test_case;

    use super::{
        MAX_STATE_DEPTH, StateConvertError, json_to_lua, lua_to_json, serialized_string_len,
    };

    #[test]
    fn round_trip_preserves_nested_null_and_array_shape() {
        let lua = Lua::new();
        let input = json!({"items": [null, {"enabled": true}], "name": "state"});

        let lua_value = json_to_lua(&lua, &input).unwrap();
        let items: Value = lua_value.as_table().unwrap().raw_get("items").unwrap();
        let first: Value = items.as_table().unwrap().raw_get(1).unwrap();

        assert!(first.is_null());
        assert_eq!(lua_to_json(&lua, &lua_value).unwrap(), input);
    }

    #[test]
    fn ordinary_lua_sequence_converts_to_json_array() {
        let lua = Lua::new();
        let value = lua.load("return { items = { 'a', 'b' } }").eval().unwrap();

        assert_eq!(
            lua_to_json(&lua, &value).unwrap(),
            json!({"items": ["a", "b"]})
        );
    }

    #[test]
    fn round_trip_preserves_empty_array_shape() {
        let lua = Lua::new();
        let input = json!({"items": []});

        let value = json_to_lua(&lua, &input).unwrap();

        assert_eq!(lua_to_json(&lua, &value).unwrap(), input);
    }

    #[test_case(f64::NAN ; "nan")]
    #[test_case(f64::INFINITY ; "positive_infinity")]
    #[test_case(f64::NEG_INFINITY ; "negative_infinity")]
    fn rejects_non_finite_numbers(number: f64) {
        let lua = Lua::new();

        assert_eq!(
            lua_to_json(&lua, &Value::Number(number)).unwrap_err(),
            StateConvertError::NonFiniteNumber
        );
    }

    #[test_case("18446744073709551617" ; "above_u64")]
    #[test_case("-9223372036854775809" ; "below_i64")]
    #[test_case("0.12345678901234567890123456789" ; "precision_loss")]
    fn rejects_json_numbers_that_cannot_round_trip_exactly(raw: &str) {
        let lua = Lua::new();
        let value: serde_json::Value = serde_json::from_str(raw).unwrap();

        assert_eq!(
            json_to_lua(&lua, &value).unwrap_err(),
            StateConvertError::UnrepresentableNumber
        );
    }

    #[test]
    fn rejects_non_utf8_strings() {
        let lua = Lua::new();
        let value = Value::String(lua.create_string([0xff]).unwrap());

        assert_eq!(
            lua_to_json(&lua, &value).unwrap_err(),
            StateConvertError::InvalidUtf8String
        );
    }

    #[test]
    fn rejects_unsupported_values() {
        let lua = Lua::new();
        let value = Value::Function(lua.create_function(|_, ()| Ok(())).unwrap());

        assert_eq!(
            lua_to_json(&lua, &value).unwrap_err(),
            StateConvertError::UnsupportedValue("function")
        );
    }

    #[test]
    fn rejects_nil_but_accepts_mlua_null() {
        let lua = Lua::new();

        assert_eq!(
            lua_to_json(&lua, &Value::Nil).unwrap_err(),
            StateConvertError::UnsupportedValue("nil")
        );
        assert_eq!(lua_to_json(&lua, &lua.null()).unwrap(), json!(null));
    }

    #[test]
    fn unmarked_tables_are_objects_with_string_keys_only() {
        let lua = Lua::new();
        let object = lua.create_table().unwrap();
        object.raw_set("name", "plugin").unwrap();
        assert_eq!(
            lua_to_json(&lua, &Value::Table(object)).unwrap(),
            json!({"name": "plugin"})
        );

        let invalid = lua.create_table().unwrap();
        invalid.raw_set(1, "item").unwrap();
        assert_eq!(
            lua_to_json(&lua, &Value::Table(invalid)).unwrap_err(),
            StateConvertError::NonStringObjectKey
        );
    }

    #[test]
    fn marked_arrays_require_only_contiguous_integer_keys() {
        let lua = Lua::new();
        let array = lua.create_table().unwrap();
        array.set_metatable(Some(lua.array_metatable())).unwrap();
        array.raw_set(1, "a").unwrap();
        array.raw_set(2, "b").unwrap();
        assert_eq!(
            lua_to_json(&lua, &Value::Table(array)).unwrap(),
            json!(["a", "b"])
        );

        let mixed = lua.create_table().unwrap();
        mixed.set_metatable(Some(lua.array_metatable())).unwrap();
        mixed.raw_set(1, "a").unwrap();
        mixed.raw_set("name", "state").unwrap();
        assert_eq!(
            lua_to_json(&lua, &Value::Table(mixed)).unwrap_err(),
            StateConvertError::InvalidArrayKey
        );

        let sparse = lua.create_table().unwrap();
        sparse.set_metatable(Some(lua.array_metatable())).unwrap();
        sparse.raw_set(1, "a").unwrap();
        sparse.raw_set(3, "c").unwrap();
        assert_eq!(
            lua_to_json(&lua, &Value::Table(sparse)).unwrap_err(),
            StateConvertError::SparseArray
        );
    }

    #[test]
    fn rejects_direct_and_indirect_cycles() {
        let lua = Lua::new();
        let direct = lua.create_table().unwrap();
        direct.raw_set("self", direct.clone()).unwrap();
        assert_eq!(
            lua_to_json(&lua, &Value::Table(direct)).unwrap_err(),
            StateConvertError::Cycle
        );

        let first = lua.create_table().unwrap();
        let second = lua.create_table().unwrap();
        first.raw_set("second", second.clone()).unwrap();
        second.raw_set("first", first.clone()).unwrap();
        assert_eq!(
            lua_to_json(&lua, &Value::Table(first)).unwrap_err(),
            StateConvertError::Cycle
        );
    }

    #[test]
    fn allows_repeated_acyclic_table_references() {
        let lua = Lua::new();
        let shared = lua.create_table().unwrap();
        shared.raw_set("value", 7).unwrap();
        let root = lua.create_table().unwrap();
        root.raw_set("left", shared.clone()).unwrap();
        root.raw_set("right", shared).unwrap();

        assert_eq!(
            lua_to_json(&lua, &Value::Table(root)).unwrap(),
            json!({"left": {"value": 7}, "right": {"value": 7}})
        );
    }

    #[test]
    fn enforces_serialized_size_during_lua_conversion() {
        let lua = Lua::new();
        let exact = Value::String(
            lua.create_string("x".repeat(MAX_PLUGIN_STATE_BYTES - 2))
                .unwrap(),
        );
        assert!(lua_to_json(&lua, &exact).is_ok());

        let oversized = Value::String(
            lua.create_string("x".repeat(MAX_PLUGIN_STATE_BYTES - 1))
                .unwrap(),
        );
        assert_eq!(
            lua_to_json(&lua, &oversized).unwrap_err(),
            StateConvertError::MaximumBytesExceeded {
                maximum: MAX_PLUGIN_STATE_BYTES
            }
        );
    }

    #[test_case("plain" ; "plain")]
    #[test_case("quote\"slash\\" ; "escapes")]
    #[test_case("line\ncontrol\u{0001}" ; "controls")]
    #[test_case("héllo" ; "utf8")]
    fn serialized_string_size_matches_serde_json(value: &str) {
        assert_eq!(
            serialized_string_len(value),
            serde_json::to_vec(value).unwrap().len()
        );
    }

    #[test]
    fn enforces_maximum_depth_in_both_directions() {
        let lua = Lua::new();
        let mut json_value = json!(true);
        for _ in 0..=MAX_STATE_DEPTH {
            json_value = json!({"child": json_value});
        }
        assert_eq!(
            json_to_lua(&lua, &json_value).unwrap_err(),
            StateConvertError::MaximumDepthExceeded {
                maximum: MAX_STATE_DEPTH
            }
        );

        let root = lua.create_table().unwrap();
        let mut current = root.clone();
        for _ in 0..=MAX_STATE_DEPTH {
            let child = lua.create_table().unwrap();
            current.raw_set("child", child.clone()).unwrap();
            current = child;
        }
        assert_eq!(
            lua_to_json(&lua, &Value::Table(root)).unwrap_err(),
            StateConvertError::MaximumDepthExceeded {
                maximum: MAX_STATE_DEPTH
            }
        );
    }
}
