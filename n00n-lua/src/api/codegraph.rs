use mlua::{Lua, Result as LuaResult, Table};
use n00n_codegraph::{Client, CodegraphError};

use crate::docs::{DocKind, FnDoc, ModuleDoc, ParamDoc};

fn map_err(error: CodegraphError) -> mlua::Error {
    mlua::Error::external(format!("{error:#}"))
}

pub(crate) fn create_codegraph_table(lua: &Lua) -> LuaResult<Table> {
    let table = lua.create_table()?;

    let check = lua.create_function(|_, ()| Client::check_binary().map_err(map_err))?;
    table.set("check_binary", check)?;

    let available = lua.create_function(|_, ()| Ok(Client::available()))?;
    table.set("available", available)?;

    let has_index = lua.create_function(|_, project: String| {
        Ok(Client::has_index(std::path::Path::new(&project)))
    })?;
    table.set("has_index", has_index)?;

    let explore = lua.create_function(
        |_, (query, project, timeout_secs): (String, String, Option<u64>)| {
            Client::explore(&query, std::path::Path::new(&project), timeout_secs).map_err(map_err)
        },
    )?;
    table.set("explore", explore)?;

    Ok(table)
}

pub(crate) const DOCS: ModuleDoc = ModuleDoc {
    name: "n00n.codegraph",
    kind: DocKind::Table,
    desc: "Cross-file structural exploration via the codegraph CLI. Wraps `codegraph explore` with timeout and index checks.",
    fns: &[
        FnDoc {
            name: "check_binary",
            args: "",
            desc: "Check that the `codegraph` CLI is installed and working.",
            params: &[],
            returns: "(nil) nil on success, or error on failure.",
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
            desc: "Run `codegraph explore` for a natural-language or symbol query.",
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
            returns: "(string) Explore output text.",
            example: "",
        },
    ],
};
