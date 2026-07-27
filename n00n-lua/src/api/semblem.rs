use mlua::{Lua, Result as LuaResult, Table};
use n00n_semble::{Client, FindRelatedRequest, Mode, SearchRequest, SembleError};

use crate::docs::{DocKind, FnDoc, ModuleDoc, ParamDoc};

fn map_err(error: SembleError) -> mlua::Error {
    mlua::Error::external(format!("{error:#}"))
}

pub(crate) fn create_semblem_table(lua: &Lua) -> LuaResult<Table> {
    let table = lua.create_table()?;

    let has_index = lua.create_function(|_, project: String| {
        Ok(Client::has_index(std::path::Path::new(&project)))
    })?;
    table.set("has_index", has_index)?;

    let search = lua.create_function(
        |_, (repo, query, mode, top_k): (String, String, Option<String>, Option<usize>)| {
            let mode = match mode.as_deref() {
                Some(raw) => Mode::parse(raw).map_err(map_err)?,
                None => Mode::Bm25,
            };
            Client::search(&SearchRequest {
                repo: std::path::Path::new(&repo),
                query: &query,
                mode,
                top_k,
            })
            .map_err(map_err)
        },
    )?;
    table.set("search", search)?;

    let find_related = lua.create_function(
        |_, (repo, file_path, line, top_k): (String, String, usize, Option<usize>)| {
            Client::find_related(&FindRelatedRequest {
                repo: std::path::Path::new(&repo),
                file_path: &file_path,
                line,
                top_k,
            })
            .map_err(map_err)
        },
    )?;
    table.set("find_related", find_related)?;

    Ok(table)
}

pub(crate) const DOCS: ModuleDoc = ModuleDoc {
    name: "n00n.semblem",
    kind: DocKind::Table,
    desc: "BM25 code search and related-chunk lookup via the native `.n00n/search/` index.",
    fns: &[
        FnDoc {
            name: "has_index",
            args: "{project}",
            desc: "Returns true when `.n00n/search/metadata.json` exists in the project root.",
            params: &[ParamDoc {
                name: "{project}",
                ty: "string",
                desc: "Path to the project root.",
            }],
            returns: "(boolean) true when a search index is present.",
            example: "",
        },
        FnDoc {
            name: "search",
            args: "{repo}, {query}, {mode?}, {top_k?}",
            desc: "Search indexed source chunks. Defaults to BM25; hybrid/semantic modes nag when no embedder is configured.",
            params: &[
                ParamDoc {
                    name: "{repo}",
                    ty: "string",
                    desc: "Path to the project root.",
                },
                ParamDoc {
                    name: "{query}",
                    ty: "string",
                    desc: "Natural-language or keyword query.",
                },
                ParamDoc {
                    name: "{mode}",
                    ty: "string",
                    desc: "One of bm25, hybrid, or semantic.",
                },
                ParamDoc {
                    name: "{top_k}",
                    ty: "integer",
                    desc: "Maximum number of results.",
                },
            ],
            returns: "(string) Ranked snippet output.",
            example: "",
        },
        FnDoc {
            name: "find_related",
            args: "{repo}, {file_path}, {line}, {top_k?}",
            desc: "Find chunks related to a file location using BM25 over the anchor chunk.",
            params: &[
                ParamDoc {
                    name: "{repo}",
                    ty: "string",
                    desc: "Path to the project root.",
                },
                ParamDoc {
                    name: "{file_path}",
                    ty: "string",
                    desc: "Relative or absolute file path.",
                },
                ParamDoc {
                    name: "{line}",
                    ty: "integer",
                    desc: "1-based line number inside the file.",
                },
                ParamDoc {
                    name: "{top_k}",
                    ty: "integer",
                    desc: "Maximum number of results.",
                },
            ],
            returns: "(string) Ranked snippet output.",
            example: "",
        },
    ],
};
