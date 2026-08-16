use std::path::{Path, PathBuf};
use std::process::Command;

use mlua::{Lua, Result as LuaResult, Table};

use crate::docs::{DocKind, FnDoc, ModuleDoc, ParamDoc};

fn smell_binary() -> Result<(PathBuf, bool), mlua::Error> {
    if let Ok(path) = std::env::var("N00N_SMELL") {
        let candidate = PathBuf::from(path);
        if candidate.is_file() {
            return Ok((candidate, false));
        }
    }

    if let Ok(exe) = std::env::current_exe()
        && exe.file_stem().is_some_and(|name| name == "n00n")
    {
        return Ok((exe, true));
    }

    if let Some(path) = which("n00n") {
        return Ok((path, true));
    }

    Err(mlua::Error::external(
        "n00n executable not found; set N00N_SMELL to a compatible executable path",
    ))
}

fn which(name: &str) -> Option<PathBuf> {
    let executable = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(&executable))
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
    let (binary, bundled) = smell_binary()?;
    let mut command = Command::new(binary);
    if bundled {
        command.arg("smell");
    }
    let output = command
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

    let index = lua.create_function(|_, project: String| -> LuaResult<(bool, Option<String>)> {
        let outcome = resolve_project(&project)
            .and_then(|path| run_smell(&["index", &path.to_string_lossy()]).map(|_| ()));
        match outcome {
            Ok(()) => Ok((true, None)),
            Err(err) => Ok((false, Some(err.to_string()))),
        }
    })?;
    table.set("index", index)?;

    let search = lua.create_function(
        |_,
         (project, query, kind, top_k): (String, String, Option<String>, Option<usize>)|
         -> LuaResult<(Option<String>, Option<String>)> {
            let outcome = resolve_project(&project).and_then(|path| {
                let mut owned = vec![
                    "search".to_owned(),
                    path.to_string_lossy().into_owned(),
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
