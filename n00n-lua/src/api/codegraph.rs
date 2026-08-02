use std::path::Path;

use mlua::{Lua, Result as LuaResult, Table};
use n00n_codegraph::Client;

use crate::docs::{DocKind, FnDoc, ModuleDoc, ParamDoc};

pub(crate) fn create_codegraph_table(lua: &Lua) -> LuaResult<Table> {
    let table = lua.create_table()?;

    let check = lua.create_function(|_, ()| match Client::check_binary() {
        Ok(()) => Ok((true, None::<String>)),
        Err(e) => Ok((false, Some(format!("{e:#}")))),
    })?;
    table.set("check_binary", check)?;

    let available = lua.create_function(|_, ()| Ok(Client::available()))?;
    table.set("available", available)?;

    let has_index =
        lua.create_function(|_, project: String| Ok(Client::has_index(Path::new(&project))))?;
    table.set("has_index", has_index)?;

    let has_database = lua.create_function(|_, project: String| {
        Ok(Client::has_database(std::path::Path::new(&project)))
    })?;
    table.set("has_database", has_database)?;

    let explore = lua.create_function(
        |_, (query, project, timeout_secs): (String, String, Option<u64>)| match Client::explore(
            &query,
            Path::new(&project),
            timeout_secs,
        ) {
            Ok(output) => Ok((Some(output), None::<String>)),
            Err(e) => Ok((None::<String>, Some(format!("{e:#}")))),
        },
    )?;
    table.set("explore", explore)?;

    let callers =
        lua.create_function(
            |_, (symbol, project, timeout_secs): (String, String, Option<u64>)| {
                match Client::callers(&symbol, Path::new(&project), timeout_secs) {
                    Ok(output) => Ok((Some(output), None::<String>)),
                    Err(e) => Ok((None::<String>, Some(format!("{e:#}")))),
                }
            },
        )?;
    table.set("callers", callers)?;

    let callees_fn =
        lua.create_function(
            |_, (symbol, project, timeout_secs): (String, String, Option<u64>)| {
                match Client::callees(&symbol, Path::new(&project), timeout_secs) {
                    Ok(output) => Ok((Some(output), None::<String>)),
                    Err(e) => Ok((None::<String>, Some(format!("{e:#}")))),
                }
            },
        )?;
    table.set("callees", callees_fn)?;

    let impact = lua.create_function(
        |_, (symbol, project, timeout_secs): (String, String, Option<u64>)| match Client::impact(
            &symbol,
            Path::new(&project),
            timeout_secs,
        ) {
            Ok(output) => Ok((Some(output), None::<String>)),
            Err(e) => Ok((None::<String>, Some(format!("{e:#}")))),
        },
    )?;
    table.set("impact", impact)?;

    let affected = lua.create_function(
        |_, (files, project, timeout_secs): (Vec<String>, String, Option<u64>)| {
            let files_refs: Vec<&str> = files.iter().map(String::as_str).collect();
            match Client::affected(&files_refs, Path::new(&project), timeout_secs) {
                Ok(output) => Ok((Some(output), None::<String>)),
                Err(e) => Ok((None::<String>, Some(format!("{e:#}")))),
            }
        },
    )?;
    table.set("affected", affected)?;

    let node = lua.create_function(
        |_, (name, project, timeout_secs): (String, String, Option<u64>)| match Client::node(
            &name,
            Path::new(&project),
            timeout_secs,
        ) {
            Ok(output) => Ok((Some(output), None::<String>)),
            Err(e) => Ok((None::<String>, Some(format!("{e:#}")))),
        },
    )?;
    table.set("node", node)?;

    let query = lua.create_function(
        |_, (search, project, timeout_secs): (String, String, Option<u64>)| match Client::query(
            &search,
            Path::new(&project),
            timeout_secs,
        ) {
            Ok(output) => Ok((Some(output), None::<String>)),
            Err(e) => Ok((None::<String>, Some(format!("{e:#}")))),
        },
    )?;
    table.set("query", query)?;

    let sync =
        lua.create_function(
            |_, (project, timeout_secs): (String, Option<u64>)| match Client::sync(
                Path::new(&project),
                timeout_secs,
            ) {
                Ok(output) => Ok((Some(output), None::<String>)),
                Err(e) => Ok((None::<String>, Some(format!("{e:#}")))),
            },
        )?;
    table.set("sync", sync)?;

    let files =
        lua.create_function(
            |_, (project, timeout_secs): (String, Option<u64>)| match Client::files(
                Path::new(&project),
                timeout_secs,
            ) {
                Ok(output) => Ok((Some(output), None::<String>)),
                Err(e) => Ok((None::<String>, Some(format!("{e:#}")))),
            },
        )?;
    table.set("files", files)?;

    Ok(table)
}

pub(crate) const DOCS: ModuleDoc = ModuleDoc {
    name: "n00n.codegraph",
    kind: DocKind::Table,
    desc: "Cross-file structural exploration via native `.codegraph/codegraph.db` queries with CLI fallback.",
    fns: &[
        FnDoc {
            name: "check_binary",
            args: "",
            desc: "Check that the `codegraph` CLI is installed and working.",
            params: &[],
            returns: "(boolean, string?) ok and optional error message.",
            example: "",
        },
        FnDoc {
            name: "available",
            args: "",
            desc: "Returns true if the `codegraph` CLI is on PATH.",
            params: &[],
            returns: "(boolean) true when codegraph is available.",
            example: "",
        },
        FnDoc {
            name: "has_index",
            args: "{project}",
            desc: "Returns true when `.codegraph/` exists in the project root.",
            params: &[ParamDoc {
                name: "{project}",
                ty: "string",
                desc: "Path to the project root.",
            }],
            returns: "(boolean) true when a codegraph index is present.",
            example: "",
        },
        FnDoc {
            name: "has_database",
            args: "{project}",
            desc: "Returns true when `.codegraph/codegraph.db` exists in the project root.",
            params: &[ParamDoc {
                name: "{project}",
                ty: "string",
                desc: "Path to the project root.",
            }],
            returns: "(boolean) true when the native SQLite index is present.",
            example: "",
        },
        FnDoc {
            name: "explore",
            args: "{query}, {project}, {timeout_secs?}",
            desc: "Run an explore query using the native SQLite index when available, otherwise `codegraph explore`.",
            params: &[
                ParamDoc {
                    name: "{query}",
                    ty: "string",
                    desc: "Natural language question or symbol names to explore.",
                },
                ParamDoc {
                    name: "{project}",
                    ty: "string",
                    desc: "Path to the project root.",
                },
                ParamDoc {
                    name: "{timeout_secs}",
                    ty: "integer",
                    desc: "Optional timeout in seconds (default 30).",
                },
            ],
            returns: "(string?, string?) output and optional error message.",
            example: "",
        },
        FnDoc {
            name: "callers",
            args: "{symbol}, {project}, {timeout_secs?}",
            desc: "Find all functions/methods that call a specific symbol using native SQLite when available, otherwise `codegraph callers`.",
            params: &[
                ParamDoc {
                    name: "{symbol}",
                    ty: "string",
                    desc: "Symbol name to find callers for.",
                },
                ParamDoc {
                    name: "{project}",
                    ty: "string",
                    desc: "Path to the project root.",
                },
                ParamDoc {
                    name: "{timeout_secs}",
                    ty: "integer",
                    desc: "Optional timeout in seconds (default 30).",
                },
            ],
            returns: "(string?, string?) output and optional error message.",
            example: "",
        },
        FnDoc {
            name: "callees",
            args: "{symbol}, {project}, {timeout_secs?}",
            desc: "Find all functions/methods that a specific symbol calls using native SQLite when available, otherwise `codegraph callees`.",
            params: &[
                ParamDoc {
                    name: "{symbol}",
                    ty: "string",
                    desc: "Symbol name to find callees for.",
                },
                ParamDoc {
                    name: "{project}",
                    ty: "string",
                    desc: "Path to the project root.",
                },
                ParamDoc {
                    name: "{timeout_secs}",
                    ty: "integer",
                    desc: "Optional timeout in seconds (default 30).",
                },
            ],
            returns: "(string?, string?) output and optional error message.",
            example: "",
        },
        FnDoc {
            name: "impact",
            args: "{symbol}, {project}, {timeout_secs?}",
            desc: "Analyze what code is affected by changing a symbol using native SQLite when available, otherwise `codegraph impact`.",
            params: &[
                ParamDoc {
                    name: "{symbol}",
                    ty: "string",
                    desc: "Symbol name to analyze impact for.",
                },
                ParamDoc {
                    name: "{project}",
                    ty: "string",
                    desc: "Path to the project root.",
                },
                ParamDoc {
                    name: "{timeout_secs}",
                    ty: "integer",
                    desc: "Optional timeout in seconds (default 30).",
                },
            ],
            returns: "(string?, string?) output and optional error message.",
            example: "",
        },
        FnDoc {
            name: "affected",
            args: "{files}, {project}, {timeout_secs?}",
            desc: "Accept an array of file paths and compute the affected file set using `codegraph affected`.",
            params: &[
                ParamDoc {
                    name: "{files}",
                    ty: "table<string>",
                    desc: "Array of file paths that changed.",
                },
                ParamDoc {
                    name: "{project}",
                    ty: "string",
                    desc: "Path to the project root.",
                },
                ParamDoc {
                    name: "{timeout_secs}",
                    ty: "integer",
                    desc: "Optional timeout in seconds (default 30).",
                },
            ],
            returns: "(string?, string?) output and optional error message.",
            example: "",
        },
        FnDoc {
            name: "node",
            args: "{name}, {project}, {timeout_secs?}",
            desc: "Get one symbol's source + caller/callee trail using native SQLite when available, otherwise `codegraph node`.",
            params: &[
                ParamDoc {
                    name: "{name}",
                    ty: "string",
                    desc: "Symbol name to look up.",
                },
                ParamDoc {
                    name: "{project}",
                    ty: "string",
                    desc: "Path to the project root.",
                },
                ParamDoc {
                    name: "{timeout_secs}",
                    ty: "integer",
                    desc: "Optional timeout in seconds (default 30).",
                },
            ],
            returns: "(string?, string?) output and optional error message.",
            example: "",
        },
        FnDoc {
            name: "query",
            args: "{search}, {project}, {timeout_secs?}",
            desc: "Search for symbols in the codebase using native SQLite when available, otherwise `codegraph query`.",
            params: &[
                ParamDoc {
                    name: "{search}",
                    ty: "string",
                    desc: "Search query for symbols.",
                },
                ParamDoc {
                    name: "{project}",
                    ty: "string",
                    desc: "Path to the project root.",
                },
                ParamDoc {
                    name: "{timeout_secs}",
                    ty: "integer",
                    desc: "Optional timeout in seconds (default 30).",
                },
            ],
            returns: "(string?, string?) output and optional error message.",
            example: "",
        },
        FnDoc {
            name: "sync",
            args: "{project}, {timeout_secs?}",
            desc: "Sync changes since last index using `codegraph sync`.",
            params: &[
                ParamDoc {
                    name: "{project}",
                    ty: "string",
                    desc: "Path to the project root.",
                },
                ParamDoc {
                    name: "{timeout_secs}",
                    ty: "integer",
                    desc: "Optional timeout in seconds (default 30).",
                },
            ],
            returns: "(string?, string?) output and optional error message.",
            example: "",
        },
        FnDoc {
            name: "files",
            args: "{project}, {timeout_secs?}",
            desc: "Show project file structure from the index using native SQLite when available, otherwise `codegraph files`.",
            params: &[
                ParamDoc {
                    name: "{project}",
                    ty: "string",
                    desc: "Path to the project root.",
                },
                ParamDoc {
                    name: "{timeout_secs}",
                    ty: "integer",
                    desc: "Optional timeout in seconds (default 30).",
                },
            ],
            returns: "(string?, string?) output and optional error message.",
            example: "",
        },
    ],
};
