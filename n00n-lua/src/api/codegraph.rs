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
    ],
};
