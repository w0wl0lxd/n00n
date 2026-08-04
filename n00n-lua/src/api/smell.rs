use std::path::{Path, PathBuf};
use std::process::Command;

use mlua::{Lua, Result as LuaResult, Table};

use crate::docs::{DocKind, FnDoc, ModuleDoc, ParamDoc};

fn smell_binary_path() -> Result<PathBuf, mlua::Error> {
    if let Ok(path) = std::env::var("N00N_SMELL") {
        let candidate = PathBuf::from(path);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        let same = exe.with_file_name("n00n-smell");
        if same.is_file() {
            return Ok(same);
        }
        if let Some(parent) = exe.parent() {
            let sibling = parent.join("n00n-smell");
            if sibling.is_file() {
                return Ok(sibling);
            }
            if let Some(grandparent) = parent.parent() {
                let cousin = grandparent.join("n00n-smell");
                if cousin.is_file() {
                    return Ok(cousin);
                }
            }
        }
    }

    if let Some(path) = which("n00n-smell") {
        return Ok(path);
    }

    Err(mlua::Error::external(
        "n00n-smell binary not found; set N00N_SMELL or build the workspace",
    ))
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
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

fn run_smell(args: &[&str]) -> Result<String, mlua::Error> {
    let binary = smell_binary_path()?;
    let output = Command::new(binary)
        .args(args)
        .output()
        .map_err(|err| mlua::Error::external(format!("failed to run n00n-smell: {err}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    if output.status.success() {
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Err(mlua::Error::external(format!(
            "n00n-smell failed: {stderr}{stdout}",
        )))
    }
}

pub(crate) fn create_smell_table(lua: &Lua) -> LuaResult<Table> {
    let table = lua.create_table()?;

    let has_index = lua.create_function(|_, project: String| {
        let path = Path::new(&project);
        if !path.is_dir() {
            return Ok(false);
        }
        Ok(path.join(".n00n/smells/metadata.json").is_file())
    })?;
    table.set("has_index", has_index)?;

    let index = lua.create_function(|_, project: String| {
        let project = resolve_project(&project)?;
        run_smell(&["index", &project.to_string_lossy()])?;
        Ok(())
    })?;
    table.set("index", index)?;

    let search = lua.create_function(
        |_, (project, query, kind, top_k): (String, String, Option<String>, Option<usize>)| {
            let project = resolve_project(&project)?;
            let mut owned = vec![
                "search".to_owned(),
                project.to_string_lossy().into_owned(),
                query,
            ];
            if let Some(k) = kind {
                owned.push("--kind".to_owned());
                owned.push(k);
            }
            if let Some(n) = top_k {
                owned.push("--top-k".to_owned());
                owned.push(n.to_string());
            }
            let args: Vec<&str> = owned.iter().map(String::as_str).collect();
            run_smell(&args)
        },
    )?;
    table.set("search", search)?;

    Ok(table)
}

pub(crate) const DOCS: ModuleDoc = ModuleDoc {
    name: "n00n.smell",
    kind: DocKind::Table,
    desc: "Persistent code-smell and comment index. Stores conflict markers, TODO/FIXME/HACK comments, and placeholder phrases in a local `.n00n/smells` Tantivy index. The n00n-smell binary does the actual indexing and searching.",
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
            desc: "Build or rebuild the smell index for a repository by invoking n00n-smell.",
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
