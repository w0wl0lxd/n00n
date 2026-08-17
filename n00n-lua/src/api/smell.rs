use std::path::{Path, PathBuf};
use std::process::Command;

use mlua::{Lua, Result as LuaResult, Table};
use n00n_smell::{Query, SearchConfig, SmellIndex};

use crate::docs::{DocKind, FnDoc, ModuleDoc, ParamDoc};
use crate::plugin_permissions::{Permission, PluginPermissions};

fn smell_override() -> Option<PathBuf> {
    std::env::var("N00N_SMELL")
        .ok()
        .map(PathBuf::from)
        .filter(|candidate| candidate.is_file())
}

#[allow(clippy::manual_unwrap_or)]
fn top_k_or_default(top_k: Option<usize>) -> usize {
    match top_k {
        Some(top_k) => top_k,
        None => 5,
    }
}

fn resolve_project(project: &str) -> Result<PathBuf, mlua::Error> {
    let path = Path::new(project);
    if !path.is_dir() {
        return Err(mlua::Error::external(format!(
            "project path is not a directory: {project}"
        )));
    }
    path.canonicalize()
        .map_err(|err| mlua::Error::external(format!("failed to resolve {project}: {err}")))
}

fn run_override(binary: &Path, args: &[&str]) -> Result<String, mlua::Error> {
    let output = Command::new(binary)
        .args(args)
        .output()
        .map_err(|err| mlua::Error::external(format!("failed to run n00n smell: {err}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if output.status.success() {
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Err(mlua::Error::external(format!(
            "n00n smell failed: {stderr}{stdout}",
        )))
    }
}

fn index_project(project: &Path) -> Result<(), mlua::Error> {
    let index_dir = SmellIndex::index_dir(project);
    let mut index = SmellIndex::open_or_create(&index_dir, &SearchConfig::default())
        .map_err(mlua::Error::external)?;
    index.update(project, |_| {}).map_err(mlua::Error::external)
}

fn search_project(
    project: &Path,
    query: String,
    kind: Option<String>,
    top_k: usize,
) -> Result<String, mlua::Error> {
    if !SmellIndex::has_index(project) {
        return Err(mlua::Error::external(format!(
            "no smell index for {}; run `n00n smell index`",
            project.display()
        )));
    }
    let index =
        SmellIndex::open_or_create(&SmellIndex::index_dir(project), &SearchConfig::default())
            .map_err(mlua::Error::external)?;
    let results = index
        .search(&Query {
            text: query,
            kind,
            top_k,
        })
        .map_err(mlua::Error::external)?;
    Ok(n00n_smell::format_results(&results))
}

pub(crate) fn create_smell_table(lua: &Lua, permissions: &PluginPermissions) -> LuaResult<Table> {
    let table = lua.create_table()?;
    let can_read = permissions.is_allowed(Permission::FsRead);
    let can_write = permissions.is_allowed(Permission::FsWrite);
    let can_run = permissions.is_allowed(Permission::Run);

    let has_index = lua.create_function(move |_, project: String| {
        if !can_read {
            return Err(mlua::Error::external(
                "permission denied: smell has_index requires fs_read",
            ));
        }
        let path = Path::new(&project);
        Ok(path.is_dir() && SmellIndex::has_index(path))
    })?;
    table.set("has_index", has_index)?;

    let index = lua.create_function(
        move |_, project: String| -> LuaResult<(bool, Option<String>)> {
            if !can_read || !can_write {
                return Ok((
                    false,
                    Some("permission denied: smell index requires fs_read and fs_write".to_owned()),
                ));
            }
            let outcome = resolve_project(&project).and_then(|path| {
                if smell_override().is_some() && !can_run {
                    return Err(mlua::Error::external(
                        "permission denied: N00N_SMELL override requires run",
                    ));
                }
                if let Some(binary) = smell_override() {
                    run_override(&binary, &["index", &path.to_string_lossy()]).map(|_| ())
                } else {
                    index_project(&path)
                }
            });
            match outcome {
                Ok(()) => Ok((true, None)),
                Err(err) => Ok((false, Some(err.to_string()))),
            }
        },
    )?;
    table.set("index", index)?;

    let search = lua.create_function(
        move |_,
              (project, query, kind, top_k): (String, String, Option<String>, Option<usize>)|
              -> LuaResult<(Option<String>, Option<String>)> {
            if !can_read {
                return Ok((
                    None,
                    Some("permission denied: smell search requires fs_read".to_owned()),
                ));
            }
            let outcome = resolve_project(&project).and_then(|path| {
                if smell_override().is_some() && !can_run {
                    return Err(mlua::Error::external(
                        "permission denied: N00N_SMELL override requires run",
                    ));
                }
                if let Some(binary) = smell_override() {
                    let mut owned = vec![
                        "search".to_owned(),
                        path.to_string_lossy().into_owned(),
                        query,
                    ];
                    if let Some(kind) = kind {
                        owned.push("--kind".to_owned());
                        owned.push(kind);
                    }
                    if let Some(top_k) = top_k {
                        owned.push("--top-k".to_owned());
                        owned.push(top_k.to_string());
                    }
                    let args: Vec<&str> = owned.iter().map(String::as_str).collect();
                    run_override(&binary, &args)
                } else {
                    search_project(&path, query, kind, top_k_or_default(top_k))
                }
            });
            match outcome {
                Ok(output) => Ok((Some(output), None)),
                Err(err) => Ok((None, Some(err.to_string()))),
            }
        },
    )?;
    table.set("search", search)?;

    Ok(table)
}

pub(crate) const DOCS: ModuleDoc = ModuleDoc {
    name: "n00n.smell",
    kind: DocKind::Table,
    desc: "Persistent code-smell and comment index built into n00n. Stores TODO/FIXME/HACK comments and placeholder phrases in a local `.n00n/smells` Tantivy index.",
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
            returns: "(boolean, string|nil) true on success, or false and the error message.",
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
                    desc: "Optional kind filter: todo, fixme, hack, placeholder.",
                },
                ParamDoc {
                    name: "{top_k}",
                    ty: "integer",
                    desc: "Maximum number of results (default 5).",
                },
            ],
            returns: "(string|nil, string|nil) Ranked smell output, or nil and the error message.",
            example: "",
        },
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_and_search_run_in_process() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join("sample.rs"),
            "// TODO: test direct smell search\n",
        )
        .unwrap();

        index_project(temp.path()).unwrap();
        let output = search_project(temp.path(), "direct smell".to_owned(), None, 5).unwrap();

        assert!(output.contains("sample.rs"));
    }
}
