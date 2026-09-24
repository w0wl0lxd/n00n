use std::path::Path;
use std::str::FromStr;

use mlua::{Lua, Result as LuaResult, Table};
use n00n_git::conflicts::{ConflictsOptions, FindingKind, OutputMode};
use n00n_git::git;
use serde::Serialize;
use serde_json::json;

use crate::docs::{DocKind, FnDoc, ModuleDoc, ParamDoc};
use crate::plugin_permissions::{Permission, PluginPermissions};

const DEFAULT_LOG_COUNT: usize = 10;

fn encode(value: &impl Serialize) -> Result<String, mlua::Error> {
    serde_json::to_string(value).map_err(mlua::Error::external)
}

fn run(command: &str, repo: &Path, options: &Table) -> Result<String, mlua::Error> {
    let output = match command {
        "status" => encode(&git::status(repo).map_err(mlua::Error::external)?)?,
        "log" => {
            let count = match options.get::<Option<usize>>("count")? {
                Some(count) => count,
                None => DEFAULT_LOG_COUNT,
            };
            encode(&git::log(repo, count).map_err(mlua::Error::external)?)?
        }
        "diff" => encode(
            &git::diff(
                repo,
                &options.get::<String>("ref_a")?,
                &options.get::<String>("ref_b")?,
            )
            .map_err(mlua::Error::external)?,
        )?,
        "branches" => encode(&git::branches(repo).map_err(mlua::Error::external)?)?,
        "blame" => encode(
            &git::blame(repo, &options.get::<String>("file")?).map_err(mlua::Error::external)?,
        )?,
        "conflicts" => {
            let defaults = ConflictsOptions::default();
            let kinds = match options.get::<Option<Vec<String>>>("kinds")? {
                Some(kinds) => kinds
                    .into_iter()
                    .map(|kind| FindingKind::from_str(&kind).map_err(mlua::Error::external))
                    .collect::<Result<Vec<_>, _>>()?,
                None => defaults.kinds,
            };
            let output = match options.get::<Option<String>>("output")? {
                Some(output) => OutputMode::from_str(&output).map_err(mlua::Error::external)?,
                None => defaults.output,
            };
            let max_hunk_lines = match options.get::<Option<usize>>("max_hunk_lines")? {
                Some(max_hunk_lines) => max_hunk_lines,
                None => defaults.max_hunk_lines,
            };
            let max_file_bytes = match options.get::<Option<usize>>("max_file_bytes")? {
                Some(max_file_bytes) => max_file_bytes,
                None => defaults.max_file_bytes,
            };
            let include_untracked = match options.get::<Option<bool>>("include_untracked")? {
                Some(include_untracked) => include_untracked,
                None => defaults.include_untracked,
            };
            let include_ignored = match options.get::<Option<bool>>("include_ignored")? {
                Some(include_ignored) => include_ignored,
                None => defaults.include_ignored,
            };
            encode(
                &n00n_git::conflicts::find(
                    repo,
                    &ConflictsOptions {
                        kinds,
                        output,
                        max_hunk_lines,
                        max_file_bytes,
                        include_untracked,
                        include_ignored,
                    },
                )
                .map_err(mlua::Error::external)?,
            )?
        }
        "add" => {
            git::add(repo, &options.get::<Vec<String>>("files")?).map_err(mlua::Error::external)?;
            encode(&json!({ "ok": true }))?
        }
        "commit" => {
            let commit_id = git::commit(repo, &options.get::<String>("message")?)
                .map_err(mlua::Error::external)?;
            encode(&json!({ "commit_id": commit_id }))?
        }
        "checkout" => {
            git::checkout(repo, &options.get::<String>("target")?)
                .map_err(mlua::Error::external)?;
            encode(&json!({ "ok": true }))?
        }
        _ => {
            return Err(mlua::Error::external(format!(
                "unknown git command: {command}"
            )));
        }
    };
    Ok(output)
}

pub(crate) fn create_git_table(lua: &Lua, permissions: &PluginPermissions) -> LuaResult<Table> {
    let table = lua.create_table()?;
    let permissions = permissions.clone();
    table.set(
        "run",
        lua.create_function(
            move |lua, (command, repo, options): (String, String, Option<Table>)| {
                let writes = matches!(command.as_str(), "add" | "commit" | "checkout");
                let requires_run = command == "checkout";
                let permission = if writes {
                    Permission::FsWrite
                } else {
                    Permission::FsRead
                };
                if !permissions.is_allowed(permission)
                    || (requires_run && !permissions.is_allowed(Permission::Run))
                {
                    return Ok((
                        None,
                        Some(format!(
                            "permission denied: git {command} requires {permission}{}",
                            if requires_run { " and run" } else { "" }
                        )),
                    ));
                }
                let options = match options {
                    Some(options) => options,
                    None => lua.create_table()?,
                };
                match run(&command, Path::new(&repo), &options) {
                    Ok(output) => Ok((Some(output), None::<String>)),
                    Err(error) => Ok((None, Some(error.to_string()))),
                }
            },
        )?,
    )?;
    Ok(table)
}

pub(crate) const DOCS: ModuleDoc = ModuleDoc {
    name: "n00n.git",
    kind: DocKind::Table,
    desc: "In-process access to the git operations linked into n00n.",
    fns: &[FnDoc {
        name: "run",
        args: "{command}, {repo}, {options?}",
        desc: "Run a bundled git operation and return its JSON result.",
        params: &[
            ParamDoc {
                name: "{command}",
                ty: "string",
                desc: "Operation name.",
            },
            ParamDoc {
                name: "{repo}",
                ty: "string",
                desc: "Path to the repository.",
            },
            ParamDoc {
                name: "{options}",
                ty: "table",
                desc: "Operation-specific arguments.",
            },
        ],
        returns: "(string|nil, string|nil) JSON result, or nil and the error message.",
        example: "",
    }],
};

#[cfg(test)]
mod tests {
    use std::process::Command;

    use mlua::Function;

    use super::*;

    fn test_repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        let init = Command::new("git")
            .args(["init", "--initial-branch=main"])
            .arg(repo.path())
            .output()
            .unwrap();
        assert!(init.status.success());
        repo
    }

    #[test]
    fn conflicts_uses_library_defaults_when_options_are_absent() {
        let repo = test_repo();
        std::fs::write(repo.path().join("sample.rs"), "// TODO: inspect\n").unwrap();
        let add = Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["add", "sample.rs"])
            .output()
            .unwrap();
        assert!(add.status.success());
        let lua = Lua::new();
        let options = lua.create_table().unwrap();

        let output = run("conflicts", repo.path(), &options).unwrap();
        let result: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(result["files"][0]["path"], "sample.rs");
    }

    #[test]
    fn native_commit_does_not_require_run_permission() {
        let lua = Lua::new();
        let mut permissions = PluginPermissions::denied();
        permissions.set(Permission::FsWrite, true);
        let table = create_git_table(&lua, &permissions).unwrap();
        let run: Function = table.get("run").unwrap();

        let commit_options = lua.create_table().unwrap();
        commit_options.set("message", "test").unwrap();
        let (_, commit_error): (Option<String>, Option<String>) =
            run.call(("commit", "/missing", commit_options)).unwrap();
        assert!(!commit_error.unwrap().contains("permission denied"));

        let add_options = lua.create_table().unwrap();
        add_options.set("files", vec!["file.txt"]).unwrap();
        let (_, add_error): (Option<String>, Option<String>) =
            run.call(("add", "/missing", add_options)).unwrap();
        assert!(!add_error.unwrap().contains("permission denied"));

        let (_, checkout_error): (Option<String>, Option<String>) =
            run.call(("checkout", "/missing", None::<Table>)).unwrap();
        assert!(checkout_error.unwrap().contains("permission denied"));
    }

    #[test]
    fn status_runs_in_process() {
        let repo = test_repo();
        let lua = Lua::new();
        let table = create_git_table(&lua, &PluginPermissions::trusted()).unwrap();
        let run: Function = table.get("run").unwrap();
        let (output, error): (Option<String>, Option<String>) = run
            .call((
                "status",
                repo.path().to_string_lossy().as_ref(),
                None::<Table>,
            ))
            .unwrap();
        assert!(error.is_none(), "{error:?}");
        let value: serde_json::Value = serde_json::from_str(&output.unwrap()).unwrap();
        assert!(value.get("files").is_some());
    }
}
