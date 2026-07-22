use mlua::{Lua, LuaSerdeExt, Result as LuaResult, Table};
use n00n_arbor::{ArborError, Client};

use crate::docs::{DocKind, FnDoc, ModuleDoc, ParamDoc};

fn map_err(e: ArborError) -> mlua::Error {
    mlua::Error::external(format!("{e:#}"))
}

fn value_or_err<T: serde::Serialize>(
    lua: &Lua,
    result: Result<T, ArborError>,
) -> LuaResult<mlua::Value> {
    let val = result.map_err(map_err)?;
    let json = serde_json::to_value(&val).map_err(|e| mlua::Error::external(format!("{e:#}")))?;
    lua.to_value(&json)
}

pub(crate) fn create_arbor_table(lua: &Lua) -> LuaResult<Table> {
    let t = lua.create_table()?;

    let check = lua.create_function(|_, ()| match Client::check_binary() {
        Ok(()) => Ok(()),
        Err(ArborError::Exec { .. }) => {
            Err(mlua::Error::external("arbor binary not found on PATH"))
        }
        Err(e) => Err(mlua::Error::external(format!("{e:#}"))),
    })?;
    t.set("check_binary", check)?;

    let available = lua.create_function(|_, ()| Ok(Client::check_binary().is_ok()))?;
    t.set("available", available)?;

    let callers = lua.create_function(|lua, (symbol, project): (String, String)| {
        value_or_err(
            lua,
            Client::callers(&symbol, std::path::Path::new(&project)),
        )
    })?;
    t.set("callers", callers)?;

    let callees_fn = lua.create_function(|lua, (symbol, project): (String, String)| {
        value_or_err(
            lua,
            Client::callees(&symbol, std::path::Path::new(&project)),
        )
    })?;
    t.set("callees", callees_fn)?;

    let map_fn = lua.create_function(|lua, (project, token_budget): (String, Option<u64>)| {
        value_or_err(
            lua,
            Client::map(std::path::Path::new(&project), token_budget),
        )
    })?;
    t.set("map", map_fn)?;

    let diff = lua.create_function(|lua, project: String| {
        value_or_err(lua, Client::diff(std::path::Path::new(&project)))
    })?;
    t.set("diff", diff)?;

    let query_fn = lua.create_function(|_, (query, project): (String, String)| {
        Client::query(&query, std::path::Path::new(&project)).map_err(map_err)
    })?;
    t.set("query", query_fn)?;

    let status_fn = lua.create_function(|_, project: String| {
        Client::status(std::path::Path::new(&project)).map_err(map_err)
    })?;
    t.set("status", status_fn)?;

    let ensure_indexed = lua.create_function(|_, project: String| {
        Client::ensure_indexed(std::path::Path::new(&project)).map_err(map_err)?;
        Ok(())
    })?;
    t.set("ensure_indexed", ensure_indexed)?;

    Ok(t)
}

pub(crate) const DOCS: ModuleDoc = ModuleDoc {
    name: "n00n.arbor",
    kind: DocKind::Table,
    desc: "Graph-based code analysis via Arbor CLI. Wraps `arbor callers`, `arbor callees`, `arbor map`, `arbor diff`, `arbor query`, and `arbor status`. Each method shells out to the `arbor` binary (Anandb71/arbor, `cargo install arbor-graph-cli`) and parses its JSON output into Lua tables.",
    fns: &[
        FnDoc {
            name: "check_binary",
            args: "",
            desc: "Check that the `arbor` CLI is installed and working.",
            params: &[],
            returns: "(nil|string) nil on success, or an error message string.",
            example: "",
        },
        FnDoc {
            name: "available",
            args: "",
            desc: "Returns true if the `arbor` CLI is on PATH.",
            params: &[],
            returns: "(boolean) true when arbor is available.",
            example: "",
        },
        FnDoc {
            name: "callers",
            args: "{symbol}, {project}",
            desc: "Show who calls a symbol.",
            params: &[
                ParamDoc {
                    name: "{symbol}",
                    ty: "string",
                    desc: "Symbol name (function, class, etc.)",
                },
                ParamDoc {
                    name: "{project}",
                    ty: "string",
                    desc: "Path to the project root.",
                },
            ],
            returns: "(table) Array of caller objects with `name`, `path`, `kind`, `line` fields.",
            example: "",
        },
        FnDoc {
            name: "callees",
            args: "{symbol}, {project}",
            desc: "Show what a symbol calls.",
            params: &[
                ParamDoc {
                    name: "{symbol}",
                    ty: "string",
                    desc: "Symbol name.",
                },
                ParamDoc {
                    name: "{project}",
                    ty: "string",
                    desc: "Path to the project root.",
                },
            ],
            returns: "(table) Array of callee objects with `name`, `path`, `kind`, `line` fields.",
            example: "",
        },
        FnDoc {
            name: "map",
            args: "{project}, {token_budget?}",
            desc: "Ranked project skeleton with symbols.",
            params: &[
                ParamDoc {
                    name: "{project}",
                    ty: "string",
                    desc: "Path to the project root.",
                },
                ParamDoc {
                    name: "{token_budget}",
                    ty: "integer",
                    desc: "Optional token budget (default 1024).",
                },
            ],
            returns: "(table) Array of map entries with `file`, `symbols` (each with `name`, `kind`, `line`, `centrality`, `callers`).",
            example: "",
        },
        FnDoc {
            name: "diff",
            args: "{project}",
            desc: "Blast radius of unpushed git changes.",
            params: &[ParamDoc {
                name: "{project}",
                ty: "string",
                desc: "Path to the project root.",
            }],
            returns: "(table) Impact object with `direct_callers`, `indirect_callers`, `blast_radius_nodes`, `api_entrypoints_affected`, `files_likely_require_updates`.",
            example: "",
        },
        FnDoc {
            name: "query",
            args: "{query}, {project}",
            desc: "Free-text search of the code graph.",
            params: &[
                ParamDoc {
                    name: "{query}",
                    ty: "string",
                    desc: "Search query text.",
                },
                ParamDoc {
                    name: "{project}",
                    ty: "string",
                    desc: "Path to the project root.",
                },
            ],
            returns: "(string) Raw query results as text.",
            example: "",
        },
        FnDoc {
            name: "status",
            args: "{project}",
            desc: "Show Arbor index status for a project.",
            params: &[ParamDoc {
                name: "{project}",
                ty: "string",
                desc: "Path to the project root.",
            }],
            returns: "(string) Status text.",
            example: "",
        },
        FnDoc {
            name: "ensure_indexed",
            args: "{project}",
            desc: "Run `arbor index` if the project is not yet indexed.",
            params: &[ParamDoc {
                name: "{project}",
                ty: "string",
                desc: "Path to the project root.",
            }],
            returns: "(nil) nil on success, or error on failure.",
            example: "",
        },
    ],
};
