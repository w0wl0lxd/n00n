//! `noon.workflow` compiles sandboxed workflow scripts. Plugins cannot call
//! Lua's `load` (the runtime sandbox strips it), so the workflow plugin hands
//! the script and a capability table here; Rust compiles the chunk with that
//! table as its environment. The plugin owns the policy (which primitives the
//! script sees); Rust owns compilation. Execution stays in Lua.

use mlua::{Function, Lua, Result as LuaResult, Table};
use noon_lua_macro::{lua_fn, lua_table};

type Pair = (Option<Function>, Option<String>);

/// Compile {source} into a function whose global environment is exactly {env}.
/// The chunk sees only the keys you put in {env}: anything else (noon, os, io,
/// require, print) reads as nil, so a workflow script stays inside the
/// primitives the plugin injects. Returns (function, nil) on success, or
/// (nil, error) when the source fails to compile.
///
/// @param source string Lua source to compile.
/// @param env table The chunk's global environment.
/// @return (function|nil, string|nil) The compiled chunk, or the compile error.
/// @example
/// local fn, err = noon.workflow.compile("return agent({ prompt = 'hi' })", { agent = agent })
/// if fn then print(fn()) end
#[lua_fn]
fn compile(lua: &Lua, source: String, env: Table) -> LuaResult<Pair> {
    match lua.load(&source).set_environment(env).into_function() {
        Ok(f) => Ok((Some(f), None)),
        Err(e) => Ok((None, Some(e.to_string()))),
    }
}

lua_table! {
    /// Sandboxed workflow script compilation.
    ///
    /// Plugins cannot reach Lua's `load`, so this compiles a workflow script
    /// with a caller-supplied environment table, keeping the script inside the
    /// primitives the plugin injects.
    ///
    /// ```lua
    /// local fn, err = noon.workflow.compile("return 1 + 1", {})
    /// ```
    "noon.workflow" => pub(crate) fn create_workflow_table(), DOCS [
        compile,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Value;

    #[test]
    fn compile_sandbox_blocks_unlisted_globals() {
        let lua = Lua::new();
        let t = create_workflow_table(&lua).unwrap();
        let compile: Function = t.get("compile").unwrap();
        let env = lua.create_table().unwrap();
        let (func, err): (Option<Function>, Option<String>) = compile
            .call((r#"return noon, os, io, require, print"#, env))
            .unwrap();
        assert!(err.is_none(), "compile failed: {err:?}");
        let (n, o, i, r, p): (Value, Value, Value, Value, Value) = func.unwrap().call(()).unwrap();
        for (name, v) in [
            ("noon", n),
            ("os", o),
            ("io", i),
            ("require", r),
            ("print", p),
        ] {
            assert!(v.is_nil(), "{name} leaked into the sandbox");
        }
    }

    #[test]
    fn compile_exposes_env_capabilities() {
        let lua = Lua::new();
        let t = create_workflow_table(&lua).unwrap();
        let compile: Function = t.get("compile").unwrap();
        let env = lua.create_table().unwrap();
        env.set("answer", 42).unwrap();
        let (func, err): (Option<Function>, Option<String>) =
            compile.call(("return answer", env)).unwrap();
        assert!(err.is_none(), "compile failed: {err:?}");
        let out: i64 = func.unwrap().call(()).unwrap();
        assert_eq!(out, 42);
    }

    #[test]
    fn compile_reports_syntax_error_as_pair() {
        let lua = Lua::new();
        let t = create_workflow_table(&lua).unwrap();
        let compile: Function = t.get("compile").unwrap();
        let env = lua.create_table().unwrap();
        let (func, err): (Option<Function>, Option<String>) =
            compile.call(("return (", env)).unwrap();
        assert!(func.is_none());
        assert!(err.is_some());
    }
}
