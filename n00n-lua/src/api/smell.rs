use std::path::Path;

use mlua::{Lua, Result as LuaResult, Table};
use n00n_smell::{Query, SearchConfig, SmellError, SmellIndex};

use crate::docs::{DocKind, FnDoc, ModuleDoc, ParamDoc};

fn map_err(error: SmellError) -> mlua::Error {
    mlua::Error::external(format!("{error:#}"))
}

pub(crate) fn create_smell_table(lua: &Lua) -> LuaResult<Table> {
    let table = lua.create_table()?;

    let has_index =
        lua.create_function(|_, project: String| Ok(SmellIndex::has_index(Path::new(&project))))?;
    table.set("has_index", has_index)?;

    let index = lua.create_function(|_, project: String| {
        let index_dir = SmellIndex::index_dir(Path::new(&project));
        let mut smell_index =
            SmellIndex::open_or_create(&index_dir, &SearchConfig::default()).map_err(map_err)?;
        smell_index
            .update(Path::new(&project), |_| {})
            .map_err(map_err)?;
        Ok(())
    })?;
    table.set("index", index)?;

    let search = lua.create_function(
        |_, (project, query, kind, top_k): (String, String, Option<String>, Option<usize>)| {
            let index_dir = SmellIndex::index_dir(Path::new(&project));
            let smell_index = SmellIndex::open_or_create(&index_dir, &SearchConfig::default())
                .map_err(map_err)?;
            let results = smell_index
                .search(&Query {
                    text: query,
                    kind,
                    top_k: top_k.map_or(5, std::convert::identity),
                })
                .map_err(map_err)?;
            Ok(n00n_smell::format_results(&results))
        },
    )?;
    table.set("search", search)?;

    Ok(table)
}

pub(crate) const DOCS: ModuleDoc = ModuleDoc {
    name: "n00n.smell",
    kind: DocKind::Table,
    desc: "Persistent code-smell and comment index. Stores conflict markers, TODO/FIXME/HACK comments, and placeholder phrases in a local `.n00n/smells` Tantivy index.",
    fns: &[
        FnDoc {
            name: "has_index",
            args: "{project}",
            desc: "Returns true when `.n00n/smells/metadata.json` exists in the project root.",
            params: &[ParamDoc {
                name: "{project}",
                ty: "string",
                desc: "Path to the project root.",
            }],
            returns: "(boolean) true when a smell index is present.",
            example: "",
        },
        FnDoc {
            name: "index",
            args: "{project}",
            desc: "Build or rebuild the smell index for a repository.",
            params: &[ParamDoc {
                name: "{project}",
                ty: "string",
                desc: "Path to the project root.",
            }],
            returns: "(nil) or raises an error.",
            example: "",
        },
        FnDoc {
            name: "search",
            args: "{project}, {query}, {kind?}, {top_k?}",
            desc: "Search the smell index by keyword and optional kind.",
            params: &[
                ParamDoc {
                    name: "{project}",
                    ty: "string",
                    desc: "Path to the project root.",
                },
                ParamDoc {
                    name: "{query}",
                    ty: "string",
                    desc: "Keyword or phrase.",
                },
                ParamDoc {
                    name: "{kind}",
                    ty: "string",
                    desc: "Optional kind filter: conflict, todo, fixme, hack, placeholder.",
                },
                ParamDoc {
                    name: "{top_k}",
                    ty: "integer",
                    desc: "Maximum number of results (default 5).",
                },
            ],
            returns: "(string) Ranked smell output.",
            example: "",
        },
    ],
};
