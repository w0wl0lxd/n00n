use mlua::{Lua, Result as LuaResult, Table};
use n00n_git::{GitError, git};
use std::path::Path;

use crate::api::util::convert::json_to_lua;
use crate::docs::{DocKind, FnDoc, ModuleDoc, ParamDoc};

fn map_err(e: GitError) -> mlua::Error {
    mlua::Error::external(format!("{e:#}"))
}

fn value_or_err<T: serde::Serialize>(
    lua: &Lua,
    result: Result<T, GitError>,
) -> LuaResult<mlua::Value> {
    let val = result.map_err(map_err)?;
    let json = serde_json::to_value(&val).map_err(|e| mlua::Error::external(format!("{e:#}")))?;
    json_to_lua(lua, &json)
}

pub(crate) fn create_git_table(lua: &Lua) -> LuaResult<Table> {
    let t = lua.create_table()?;

    let status_fn =
        lua.create_function(|lua, path: String| value_or_err(lua, git::status(Path::new(&path))))?;
    t.set("status", status_fn)?;

    let log_fn = lua.create_function(|lua, (path, count): (String, Option<usize>)| {
        value_or_err(lua, git::log(Path::new(&path), count.unwrap_or(10)))
    })?;
    t.set("log", log_fn)?;

    let diff_fn = lua.create_function(|lua, (path, ref_a, ref_b): (String, String, String)| {
        value_or_err(lua, git::diff(Path::new(&path), &ref_a, &ref_b))
    })?;
    t.set("diff", diff_fn)?;

    let branches_fn = lua
        .create_function(|lua, path: String| value_or_err(lua, git::branches(Path::new(&path))))?;
    t.set("branches", branches_fn)?;

    let blame_fn = lua.create_function(|lua, (path, file): (String, String)| {
        value_or_err(lua, git::blame(Path::new(&path), &file))
    })?;
    t.set("blame", blame_fn)?;

    Ok(t)
}

pub(crate) const DOCS: ModuleDoc = ModuleDoc {
    name: "n00n.git",
    kind: DocKind::Table,
    desc: "Native git operations using gix/gitoxide. Provides structured access to git status, log, diff, branches, and blame without shelling out to the git CLI.",
    fns: &[
        FnDoc {
            name: "status",
            args: "{path}",
            desc: "Get the current git status of a repository.",
            params: &[ParamDoc {
                name: "{path}",
                ty: "string",
                desc: "Path to the git repository.",
            }],
            returns: "(table) Status object with branch and files array containing path, status, and staged fields.",
            example: "",
        },
        FnDoc {
            name: "log",
            args: "{path}, {count?}",
            desc: "Get commit history for a repository.",
            params: &[
                ParamDoc {
                    name: "{path}",
                    ty: "string",
                    desc: "Path to the git repository.",
                },
                ParamDoc {
                    name: "{count}",
                    ty: "integer",
                    desc: "Optional number of commits to return (default 10).",
                },
            ],
            returns: "(table) Array of commit objects with id, author, email, time, and message.",
            example: "",
        },
        FnDoc {
            name: "diff",
            args: "{path}, {ref_a}, {ref_b}",
            desc: "Get diff between two references.",
            params: &[
                ParamDoc {
                    name: "{path}",
                    ty: "string",
                    desc: "Path to the git repository.",
                },
                ParamDoc {
                    name: "{ref_a}",
                    ty: "string",
                    desc: "First reference (commit SHA, branch, tag).",
                },
                ParamDoc {
                    name: "{ref_b}",
                    ty: "string",
                    desc: "Second reference (commit SHA, branch, tag).",
                },
            ],
            returns: "(table) Diff object with files array containing path, additions, deletions, and changes.",
            example: "",
        },
        FnDoc {
            name: "branches",
            args: "{path}",
            desc: "List branches in a repository.",
            params: &[ParamDoc {
                name: "{path}",
                ty: "string",
                desc: "Path to the git repository.",
            }],
            returns: "(table) Array of branch objects with name, head, and is_current fields.",
            example: "",
        },
        FnDoc {
            name: "blame",
            args: "{path}, {file}",
            desc: "Get blame information for a file.",
            params: &[
                ParamDoc {
                    name: "{path}",
                    ty: "string",
                    desc: "Path to the git repository.",
                },
                ParamDoc {
                    name: "{file}",
                    ty: "string",
                    desc: "Relative path to the file within the repository.",
                },
            ],
            returns: "(table) Blame object with lines array containing line_number, content, commit_id, author, and time.",
            example: "",
        },
    ],
};
