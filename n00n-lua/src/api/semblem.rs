use mlua::{Lua, Result as LuaResult, Table};
use n00n_semble::{Client, Mode, SembleError};

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

    // T082-T083: Use hybrid search with CLI fallback
    let search = lua.create_function(
        |_,
         (repo, query, mode, top_k, content): (
            String,
            String,
            Option<String>,
            Option<usize>,
            Option<String>,
        )| {
            let mode = match mode.as_deref() {
                Some(raw) => Mode::parse(raw).map_err(map_err)?,
                None => Mode::Bm25,
            };
            Client::search_hybrid(&repo, &query, mode, top_k, content.as_deref()).map_err(map_err)
        },
    )?;
    table.set("search", search)?;

    // T082-T083: Use hybrid find_related with CLI fallback
    let find_related = lua.create_function(
        |_, (repo, file_path, line, top_k): (String, String, usize, Option<usize>)| {
            Client::find_related_hybrid(&repo, &file_path, line, top_k).map_err(map_err)
        },
    )?;
    table.set("find_related", find_related)?;

    // T080: Add savings command
    let savings = lua.create_function(|_, repo: String| match Client::savings(&repo) {
        Ok(output) => Ok((Some(output), None::<String>)),
        Err(e) => Ok((None::<String>, Some(format!("{e:#}")))),
    })?;
    table.set("savings", savings)?;

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
            args: "{repo}, {query}, {mode?}, {top_k?}, {content?}",
            desc: "Search indexed source chunks. BM25 is native; hybrid/semantic try the upstream semble CLI and fall back to BM25 with an embedder nag if the CLI is unavailable.",
            params: &[
                ParamDoc {
                    name: "{repo}",
                    ty: "string",
                    desc: "Local project root path, or an HTTPS git URL allowed by N00N_SEMBLE_ALLOWED_REMOTE_REPOS.",
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
                ParamDoc {
                    name: "{content}",
                    ty: "string",
                    desc: "Content filter: docs, config, code, or all.",
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
                    desc: "Local project root path, or an HTTPS git URL allowed by N00N_SEMBLE_ALLOWED_REMOTE_REPOS.",
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
        FnDoc {
            name: "savings",
            args: "{repo}",
            desc: "Estimate token savings from using a hybrid/semantic embedder. Requires the semble CLI and has no native fallback.",
            params: &[ParamDoc {
                name: "{repo}",
                ty: "string",
                desc: "Local project root path, or an HTTPS git URL allowed by N00N_SEMBLE_ALLOWED_REMOTE_REPOS.",
            }],
            returns: "(string?, string?) savings summary and optional error message.",
            example: "",
        },
    ],
};
