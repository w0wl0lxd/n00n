use mlua::{Lua, LuaSerdeExt, Result as LuaResult, Table};
use n00n_arbor::{ArborError, Client};

fn map_err(e: ArborError) -> mlua::Error {
    mlua::Error::external(format!("{e:#}"))
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

    let avaiable = lua.create_function(|_, ()| Ok(Client::check_binary().is_ok()))?;
    t.set("avaiable", avaiable)?;

    let callers = lua.create_function(|lua, (symbol, project): (String, String)| {
        let results = Client::callers(&symbol, std::path::Path::new(&project)).map_err(map_err)?;
        let val =
            serde_json::to_value(&results).map_err(|e| mlua::Error::external(format!("{e:#}")))?;
        lua.to_value(&val)
    })?;
    t.set("callers", callers)?;

    let callees = lua.create_function(|lua, (symbol, project): (String, String)| {
        let results = Client::callees(&symbol, std::path::Path::new(&project)).map_err(map_err)?;
        let val =
            serde_json::to_value(&results).map_err(|e| mlua::Error::external(format!("{e:#}")))?;
        lua.to_value(&val)
    })?;
    t.set("callees", callees)?;

    let map_fn = lua.create_function(|lua, (project, token_budget): (String, Option<u64>)| {
        let results = Client::map(std::path::Path::new(&project), token_budget).map_err(map_err)?;
        let val =
            serde_json::to_value(&results).map_err(|e| mlua::Error::external(format!("{e:#}")))?;
        lua.to_value(&val)
    })?;
    t.set("map", map_fn)?;

    let diff = lua.create_function(|lua, project: String| {
        let results = Client::diff(std::path::Path::new(&project)).map_err(map_err)?;
        let val =
            serde_json::to_value(&results).map_err(|e| mlua::Error::external(format!("{e:#}")))?;
        lua.to_value(&val)
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

pub(crate) const DOCS: crate::docs::ModuleDoc = crate::docs::ModuleDoc {
    name: "n00n.arbor",
    kind: crate::docs::DocKind::Table,
    desc: "Graph-based code analysis via Arbor CLI. Wraps `arbor callers`, `arbor callees`, `arbor map`, `arbor diff`, `arbor query`, and `arbor status`. Each method shells out to the `arbor` binary (from Anandb71/arbor, `cargo install arbor-graph-cli`) and parses its JSON output into Lua tables.",
    fns: &[
        crate::docs::FnDoc {
            name: "check_binary",
            args: "",
            desc: "Check that the `arbor` CLI is installed and working.",
            params: &[],
            returns: "(nil|string) nil on success, or an error message string.",
            example: "",
        },
        crate::docs::FnDoc {
            name: "avaiable",
            args: "",
            desc: "Returns true if the `arbor` CLI is on PATH.",
            params: &[],
            returns: "(boolean) true when arbor is available.",
            example: "",
        },
        crate::docs::FnDoc {
            name: "callers",
            args: "{symbol}, {project}",
            desc: "Show who calls a symbol.",
            params: &[
                crate::docs::ParamDoc {
                    name: "{symbol}",
                    ty: "string",
                    desc: "Symbol name (function, class, etc.)",
                },
                crate::docs::ParamDoc {
                    name: "{project}",
                    ty: "string",
                    desc: "Path to the project root.",
                },
            ],
            returns: "(table) Array of caller objects with `name`, `path`, `kind`, `line` fields.",
            example: "",
        },
        crate::docs::FnDoc {
            name: "callees",
            args: "{symbol}, {project}",
            desc: "Show what a symbol calls.",
            params: &[
                crate::docs::ParamDoc {
                    name: "{symbol}",
                    ty: "string",
                    desc: "Symbol name.",
                },
                crate::docs::ParamDoc {
                    name: "{project}",
                    ty: "string",
                    desc: "Path to the project root.",
                },
            ],
            returns: "(table) Array of callee objects with `name`, `path`, `kind`, `line` fields.",
            example: "",
        },
        crate::docs::FnDoc {
            name: "map",
            args: "{project}, {token_budget?}",
            desc: "Ranked project skeleton with symbols.",
            params: &[
                crate::docs::ParamDoc {
                    name: "{project}",
                    ty: "string",
                    desc: "Path to the project root.",
                },
                crate::docs::ParamDoc {
                    name: "{token_budget}",
                    ty: "integer",
                    desc: "Optional token budget (default 1024).",
                },
            ],
            returns: "(table) Array of map entries with `path`, `rank`, `symbols` fields.",
            example: "",
        },
        crate::docs::FnDoc {
            name: "diff",
            args: "{project}",
            desc: "Blast radius of unpushed git changes.",
            params: &[crate::docs::ParamDoc {
                name: "{project}",
                ty: "string",
                desc: "Path to the project root.",
            }],
            returns: "(table) Array of impacted symbols with `name`, `path`, `distance`, `kind` fields.",
            example: "",
        },
        crate::docs::FnDoc {
            name: "query",
            args: "{query}, {project}",
            desc: "Free-text search of the code graph.",
            params: &[
                crate::docs::ParamDoc {
                    name: "{query}",
                    ty: "string",
                    desc: "Search query text.",
                },
                crate::docs::ParamDoc {
                    name: "{project}",
                    ty: "string",
                    desc: "Path to the project root.",
                },
            ],
            returns: "(string) Raw query results as text.",
            example: "",
        },
        crate::docs::FnDoc {
            name: "status",
            args: "{project}",
            desc: "Show Arbor index status for a project.",
            params: &[crate::docs::ParamDoc {
                name: "{project}",
                ty: "string",
                desc: "Path to the project root.",
            }],
            returns: "(string) Status text.",
            example: "",
        },
        crate::docs::FnDoc {
            name: "ensure_indexed",
            args: "{project}",
            desc: "Run `arbor index` if the project is not yet indexed.",
            params: &[crate::docs::ParamDoc {
                name: "{project}",
                ty: "string",
                desc: "Path to the project root.",
            }],
            returns: "(nil) nil on success, or error on failure.",
            example: "",
        },
    ],
};
