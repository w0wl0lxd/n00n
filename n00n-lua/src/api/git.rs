use std::path::Path;
use std::str::FromStr;

use mlua::{Lua, Result as LuaResult, Table};
use n00n_git::conflicts::{ConflictsOptions, FindingKind, OutputMode};
use n00n_git::git;
use serde::Serialize;
use serde_json::json;

use crate::docs::{DocKind, FnDoc, ModuleDoc, ParamDoc};
use crate::plugin_permissions::{Permission, PluginPermissions};

fn encode(value: &impl Serialize) -> Result<String, mlua::Error> {
    serde_json::to_string(value).map_err(mlua::Error::external)
}

#[allow(clippy::manual_unwrap_or, clippy::manual_unwrap_or_default)]
fn value_or<T>(value: Option<T>, default: T) -> T {
    match value {
        Some(value) => value,
        None => default,
    }
}

fn run(command: &str, repo: &Path, options: &Table) -> Result<String, mlua::Error> {
    let output = match command {
        "status" => encode(&git::status(repo).map_err(mlua::Error::external)?)?,
        "log" => {
            let count = value_or(options.get::<Option<usize>>("count")?, 10);
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
            let kinds = value_or(options.get::<Option<Vec<String>>>("kinds")?, Vec::new())
                .into_iter()
                .map(|kind| FindingKind::from_str(&kind).map_err(mlua::Error::external))
                .collect::<Result<Vec<_>, _>>()?;
            let output = match options.get::<Option<String>>("output")? {
                Some(output) => OutputMode::from_str(&output).map_err(mlua::Error::external)?,
                None => OutputMode::default(),
            };
            let max_hunk_lines = value_or(options.get::<Option<usize>>("max_hunk_lines")?, 200);
            let max_file_bytes = value_or(
                options.get::<Option<usize>>("max_file_bytes")?,
                2 * 1024 * 1024,
            );
            let include_untracked =
                value_or(options.get::<Option<bool>>("include_untracked")?, true);
            let include_ignored = value_or(options.get::<Option<bool>>("include_ignored")?, false);
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
                let requires_run = matches!(command.as_str(), "add" | "checkout");
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
    use mlua::Function;

    use super::*;

    #[test]
    fn native_commit_does_not_require_run_permission() {
        let lua = Lua::new();
        let mut permissions = PluginPermissions::denied();
        permissions.set(Permission::FsWrite, true);
        let table = create_git_table(&lua, &permissions).unwrap();
        let run: Function = table.get("run").unwrap();

        let (_, commit_error): (Option<String>, Option<String>) =
            run.call(("commit", "/missing", None::<Table>)).unwrap();
        assert!(!commit_error.unwrap().contains("permission denied"));

        for command in ["add", "checkout"] {
            let (_, error): (Option<String>, Option<String>) =
                run.call((command, "/missing", None::<Table>)).unwrap();
            assert!(error.unwrap().contains("permission denied"));
        }
    }

    #[test]
    fn status_runs_in_process() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let lua = Lua::new();
        let table = create_git_table(&lua, &PluginPermissions::trusted()).unwrap();
        let run: Function = table.get("run").unwrap();
        let (output, error): (Option<String>, Option<String>) = run
            .call(("status", repo.to_string_lossy().as_ref(), None::<Table>))
            .unwrap();
        assert!(error.is_none(), "{error:?}");
        let value: serde_json::Value = serde_json::from_str(&output.unwrap()).unwrap();
        assert!(value.get("files").is_some());
    }
}
