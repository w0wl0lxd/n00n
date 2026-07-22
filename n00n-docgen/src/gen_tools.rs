use n00n_agent::template::Vars;
use n00n_agent::tools::{DescriptionContext, ToolAudience, ToolFilter, ToolRegistry, ToolSource};
use n00n_config::{PluginFileConfig, PluginsConfig};
use regex::Regex;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::sync::Arc;

use n00n_lua::{OptionType, PluginHost};

type GenResult<T> = Result<T, String>;

const DATE_PLACEHOLDER: &str = "YYYY-MM-DD";

const SECTIONS: &[(&str, &[&str])] = &[
    (
        "File Operations",
        &[
            "bash",
            "read",
            "write",
            "edit",
            "multiedit",
            "edit_lines",
            "insert_lines",
            "glob",
            "grep",
            "index",
            "view_image",
        ],
    ),
    (
        "Execution & Control",
        &["batch", "code_execution", "question"],
    ),
    (
        "Agent & Knowledge",
        &[
            "agent_control",
            "team",
            "task",
            "workflow",
            "todo_write",
            "memory",
            "skill",
        ],
    ),
    ("Web", &["webfetch", "websearch"]),
];

struct ToolInfo {
    def: Value,
    source: ToolSource,
}

struct Param {
    name: String,
    ty: String,
    required: bool,
    default: String,
    description: String,
}

fn extract_default(desc: &str) -> (String, String) {
    for pattern in ["(default: ", "(default "] {
        if let Some(start) = desc.find(pattern) {
            let after = &desc[start + pattern.len()..];
            if let Some(end) = after.find(')') {
                let default_val = after[..end].to_string();
                let cleaned = format!(
                    "{}{}",
                    desc[..start].trim_end(),
                    &desc[start + pattern.len() + end + 1..]
                )
                .trim()
                .to_string();
                return (default_val, cleaned);
            }
        }
    }
    (String::new(), desc.to_string())
}

fn first_paragraph(desc: &str) -> &str {
    desc.split("\n\n").next().unwrap_or_else(|| desc)
}

fn schema_type(schema: &Value) -> String {
    match schema.get("type") {
        Some(Value::String(ty)) => ty.clone(),
        Some(Value::Array(types)) => types
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("/"),
        _ => schema
            .get("anyOf")
            .and_then(Value::as_array)
            .map(|variants| {
                variants
                    .iter()
                    .map(schema_type)
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .filter(|types| !types.is_empty())
            .unwrap_or_else(|| "string".to_string()),
    }
}

fn extract_params(schema: &Value) -> Vec<Param> {
    let Some(properties) = schema.get("properties").and_then(|p| p.as_object()) else {
        return Vec::new();
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map_or_else(Vec::new, |arr| {
            arr.iter().filter_map(|v| v.as_str()).collect()
        });

    let mut params = Vec::new();
    for (name, prop) in properties {
        let raw_type = schema_type(prop);
        let raw_desc = prop
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or_else(|| "");
        let is_required = required.contains(&name.as_str());
        let (default, description) = extract_default(raw_desc);
        params.push(Param {
            name: name.clone(),
            ty: raw_type,
            required: is_required,
            default,
            description,
        });
    }
    params
}

fn write_param_table(out: &mut String, params: &[Param]) {
    let has_defaults = params.iter().any(|p| !p.default.is_empty());
    let header = if has_defaults {
        "| Parameter | Type | Required | Default | Description |\n|-----------|------|----------|---------|-------------|"
    } else {
        "| Parameter | Type | Required | Description |\n|-----------|------|----------|-------------|"
    };
    let _ = writeln!(out, "{header}");
    for p in params {
        let desc = p.description.replace('\n', "<br>");
        let required = if p.required { "yes" } else { "no" };
        if has_defaults {
            let _ = writeln!(
                out,
                "| `{}` | {} | {} | {} | {} |",
                p.name, p.ty, required, p.default, desc
            );
        } else {
            let _ = writeln!(out, "| `{}` | {} | {} | {} |", p.name, p.ty, required, desc);
        }
    }
}

fn source_label(source: &ToolSource) -> &'static str {
    match source {
        ToolSource::Lua { .. } => "lua plugin",
        ToolSource::Mcp { .. } => "mcp",
    }
}

fn write_tool_entry(out: &mut String, name: &str, info: &ToolInfo, opt_in: &HashSet<String>) {
    let description = info
        .def
        .get("description")
        .and_then(|d| d.as_str())
        .unwrap_or_else(|| "");
    let schema = info
        .def
        .get("input_schema")
        .cloned()
        .unwrap_or_else(|| Value::Null);
    let params = extract_params(&schema);
    let summary = first_paragraph(description);

    let _ = writeln!(out);
    let mut badge_text = source_label(&info.source).to_string();
    if opt_in.contains(name) {
        badge_text.push_str(", opt-in");
    }
    let _ = writeln!(out, "### `{name}` *({badge_text})*");
    let _ = writeln!(out);
    let _ = writeln!(out, "{summary}");
    let _ = writeln!(out);
    write_param_table(out, &params);
}

/// Replace `target` with `placeholder`. Empty `target` is a no-op.
fn redact_path(input: &str, target: &str, placeholder: &str) -> String {
    if target.is_empty() {
        input.to_string()
    } else {
        input.replace(target, placeholder)
    }
}

/// Plugins bake env-specific values into their `description` at registration:
/// `bash` interpolates `n00n.uv.cwd()` and `websearch` interpolates
/// `os.date("%Y-%m-%d")`. Scrub both so `gen-docs-check` is stable across
/// machines and days. CWD is replaced before HOME so a cwd nested under ~
/// doesn't get partially mangled.
fn redact_env_and_dates(input: &str) -> GenResult<String> {
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|c| c.to_str().map(str::to_owned))
        .unwrap_or_else(String::new);
    let mut out = redact_path(input, &cwd, "<cwd>");
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        out = redact_path(&out, &home, "~");
    }
    let re = get_date_re()?;
    Ok(re.replace_all(&out, DATE_PLACEHOLDER).into_owned())
}

fn redact_def(def: &Value) -> GenResult<Value> {
    let Some(d) = def.get("description").and_then(|v| v.as_str()) else {
        return Ok(def.clone());
    };
    let redacted = redact_env_and_dates(d)?;
    if redacted == d {
        Ok(def.clone())
    } else {
        let mut out = def.clone();
        out["description"] = Value::String(redacted);
        Ok(out)
    }
}

fn write_front_matter(out: &mut String) {
    let _ = writeln!(out, "+++");
    let _ = writeln!(out, "title = \"Tools\"");
    let _ = writeln!(out, "weight = 3");
    let _ = writeln!(out, "[extra]");
    let _ = writeln!(out, "group = \"Reference\"");
    let _ = writeln!(out, "+++");
}

fn collect_tool_info(
    def_map: &HashMap<String, &Value>,
    entry: &n00n_agent::tools::RegisteredTool,
) -> GenResult<Option<ToolInfo>> {
    let name = entry.name();
    let Some(def) = def_map.get(name) else {
        return Ok(None);
    };
    Ok(Some(ToolInfo {
        def: redact_def(def)?,
        source: entry.source.clone(),
    }))
}

/// Loads every builtin with all sub-tools on, so the reference documents
/// opt-in tools too. "Opt-in" means the plugin declares the tool as a boolean
/// option defaulting to false, so the badge cannot drift from the defaults.
fn load_registry_with_builtins() -> GenResult<(Arc<ToolRegistry>, HashSet<String>)> {
    let registry = Arc::new(ToolRegistry::new());
    let mut host = PluginHost::new(Arc::clone(&registry))
        .map_err(|e| format!("failed to create plugin host: {e}"))?;

    let mut plugins = HashMap::new();
    let mut edit = PluginFileConfig::default();
    for &sub in n00n_config::EDIT_SUB_TOOLS {
        edit.opts.insert(sub.to_owned(), Value::Bool(true));
    }
    plugins.insert("edit".to_owned(), edit);
    host.load_builtins(&PluginsConfig::from_plugins(&plugins))
        .map_err(|e| format!("failed to load builtin plugins: {e}"))?;

    let opt_in = host
        .plugin_options()
        .map_err(|e| format!("failed to collect plugin options: {e}"))?
        .into_values()
        .flatten()
        .filter(|o| o.ty == OptionType::Boolean && o.default == Some(Value::Bool(false)))
        .map(|o| o.name)
        .collect();
    Ok((registry, opt_in))
}

#[allow(clippy::too_many_lines)]
pub fn generate() -> GenResult<String> {
    let vars = Vars::new()
        .set("{cwd}", "<cwd>")
        .set("{platform}", "linux")
        .set("{date}", "YYYY-MM-DD");

    let (registry, opt_in) = load_registry_with_builtins()?;
    let defs = registry.definitions(
        &vars,
        &DescriptionContext {
            filter: &ToolFilter::All,
            audience: ToolAudience::MAIN,
            workflow: false,
        },
        false,
    );
    let Some(defs_array) = defs.as_array() else {
        return Err("definitions should be an array".to_string());
    };
    let def_map: HashMap<String, &Value> = defs_array
        .iter()
        .filter_map(|t| {
            t.get("name")
                .and_then(|n| n.as_str())
                .map(|n| (n.to_string(), t))
        })
        .collect();

    let snapshot = registry.iter();
    let mut tools: HashMap<&str, ToolInfo> = HashMap::new();
    for entry in &snapshot {
        if let Some(info) = collect_tool_info(&def_map, entry)? {
            tools.insert(entry.name(), info);
        }
    }

    let total = tools.len();
    let mut out = String::new();
    write_front_matter(&mut out);
    let _ = writeln!(out);
    let _ = writeln!(out, "# Tools");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "N00n ships with {total} built-in tools. This is the full reference."
    );

    let mut rendered: HashSet<&str> = HashSet::new();

    for (section_name, tool_names) in SECTIONS {
        let present: Vec<&str> = tool_names
            .iter()
            .copied()
            .filter(|n| tools.contains_key(*n))
            .collect();
        if present.is_empty() {
            continue;
        }
        let _ = writeln!(out);
        let _ = writeln!(out, "## {section_name}");
        for name in present {
            let Some(info) = tools.get(name) else {
                return Err(format!("tool '{name}' not found in tools map"));
            };
            write_tool_entry(&mut out, name, info, &opt_in);
            rendered.insert(name);
        }
    }

    let mut leftovers: Vec<&str> = tools
        .keys()
        .filter(|n| !rendered.contains(*n))
        .copied()
        .collect();
    leftovers.sort_unstable();
    if !leftovers.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "## Additional tools");
        for name in leftovers {
            let Some(info) = tools.get(name) else {
                return Err(format!("tool '{name}' not found in tools map"));
            };
            write_tool_entry(&mut out, name, info, &opt_in);
        }
    }

    if out.ends_with('\n') {
        out.pop();
    }
    Ok(out)
}

static DATE_RE: std::sync::LazyLock<Result<Regex, regex::Error>> =
    std::sync::LazyLock::new(|| Regex::new(r"\d{4}-\d{2}-\d{2}"));

fn get_date_re() -> GenResult<&'static Regex> {
    DATE_RE
        .as_ref()
        .map_err(|e| format!("invalid date regex: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case("2026-07-05", "YYYY-MM-DD"; "simple date")]
    #[test_case("today is 2026-07-05 here", "today is YYYY-MM-DD here"; "embedded date")]
    #[test_case("v2024-01-15", "vYYYY-MM-DD"; "prefix embedded")]
    #[test_case("2026-7-5", "2026-7-5"; "single digit not matched")]
    #[test_case("no date here", "no date here"; "no date")]
    #[test_case("2026-07-05 and 2025-12-31", "YYYY-MM-DD and YYYY-MM-DD"; "two dates")]
    fn redacts_date(input: &str, expected: &str) {
        // redact_env_and_dates also scrubs HOME/cwd; isolate date logic by
        // calling the regex replacement directly.
        assert_eq!(
            get_date_re()
                .unwrap()
                .replace_all(input, DATE_PLACEHOLDER)
                .as_ref(),
            expected
        );
    }

    #[test_case("/home/user/repo", "/home/user", "~", "~/repo"; "path under home")]
    #[test_case("/home/user", "/home/user", "~", "~"; "exact home")]
    #[test_case("/elsewhere", "/home/user", "~", "/elsewhere"; "unrelated path")]
    #[test_case("any", "", "<cwd>", "any"; "empty target no-op")]
    #[test_case("", "/home/user", "~", ""; "empty input")]
    fn redacts_path(input: &str, target: &str, placeholder: &str, expected: &str) {
        assert_eq!(redact_path(input, target, placeholder), expected);
    }

    #[test_case("$(default: foo)", "foo", "$"; "colon form")]
    #[test_case("desc (default bar) suffix", "bar", "desc suffix"; "space form")]
    #[test_case("prefix (default: baz) extra", "baz", "prefix extra"; "in middle")]
    #[test_case("plain description", "", "plain description"; "no default")]
    fn extracts_default(input: &str, expected_default: &str, remaining_prefix: &str) {
        let (default, cleaned) = extract_default(input);
        assert_eq!(default, expected_default);
        assert!(cleaned.starts_with(remaining_prefix), "cleaned: {cleaned}");
    }

    #[test_case(&serde_json::json!({ "type": "string" }), "string"; "single")]
    #[test_case(&serde_json::json!({ "type": ["string", "integer"] }), "string/integer"; "type union")]
    #[test_case(
        &serde_json::json!({ "anyOf": [{ "type": "string" }, { "type": "integer" }] }),
        "string/integer";
        "any_of_union"
    )]
    #[test_case(&serde_json::json!({}), "string"; "missing")]
    fn formats_schema_type(input: &Value, expected: &str) {
        assert_eq!(schema_type(input), expected);
    }

    #[test]
    fn sections_partition_registered_tools() {
        let (registry, _) = load_registry_with_builtins().unwrap();
        let snapshot = registry.iter();
        let registered: HashSet<&str> = snapshot
            .iter()
            .map(n00n_agent::tools::RegisteredTool::name)
            .collect();

        let mut sectioned: HashSet<&str> = HashSet::new();
        for (_, names) in SECTIONS {
            for &n in *names {
                assert!(
                    registered.contains(n),
                    "SECTIONS references \"{n}\" which isn't a registered tool"
                );
                assert!(sectioned.insert(n), "\"{n}\" appears in multiple sections");
            }
        }

        let unsectioned: Vec<&str> = registered.difference(&sectioned).copied().collect();
        assert!(
            unsectioned.is_empty(),
            "registered tools missing from SECTIONS: {unsectioned:?}"
        );
    }
}
