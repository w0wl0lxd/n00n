use mlua::{Lua, Result as LuaResult, Table};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::env;

use crate::api::util::convert::json_to_lua;
use crate::docs::{DocKind, FnDoc, ModuleDoc, ParamDoc};
use crate::plugin_permissions::PluginPermissions;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubIssue {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub user: GitHubUser,
    pub body: Option<String>,
    pub html_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubUser {
    pub login: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubPullRequest {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub user: GitHubUser,
    pub head: GitHubRef,
    pub base: GitHubRef,
    pub html_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubRef {
    #[serde(rename = "ref")]
    pub ref_field: String,
    pub sha: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubRepository {
    pub name: String,
    pub full_name: String,
    pub description: Option<String>,
    pub language: Option<String>,
    pub stargazers_count: u64,
    pub forks_count: u64,
    pub html_url: String,
}

fn get_token() -> Option<String> {
    env::var("GITHUB_TOKEN").ok()
}

fn create_client() -> Result<Client, mlua::Error> {
    let mut builder = Client::builder();
    if let Ok(timeout) = env::var("GITHUB_TIMEOUT")
        && let Ok(secs) = timeout.parse::<u64>()
    {
        builder = builder.timeout(std::time::Duration::from_secs(secs));
    }
    builder
        .build()
        .map_err(|e| mlua::Error::external(format!("failed to create HTTP client: {e}")))
}

fn map_err(e: reqwest::Error) -> mlua::Error {
    mlua::Error::external(format!("GitHub API error: {e}"))
}

fn validate_repo_segment(segment: &str) -> Result<(), mlua::Error> {
    if segment.is_empty() {
        return Err(mlua::Error::external(
            "repository owner and name must be non-empty",
        ));
    }
    if segment.contains('/') || segment.contains('\\') {
        return Err(mlua::Error::external(
            "repository owner and name must not contain path separators",
        ));
    }
    if segment == "." || segment == ".." {
        return Err(mlua::Error::external(
            "repository owner and name must not be '.' or '..'",
        ));
    }
    Ok(())
}

fn value_or_err<T: serde::Serialize>(
    lua: &Lua,
    result: Result<T, reqwest::Error>,
) -> LuaResult<mlua::Value> {
    let val = result.map_err(map_err)?;
    let json = serde_json::to_value(&val).map_err(|e| mlua::Error::external(format!("{e:#}")))?;
    json_to_lua(lua, &json)
}

pub(crate) fn create_github_table(lua: &Lua, permissions: &PluginPermissions) -> LuaResult<Table> {
    let t = lua.create_table()?;

    let list_issues_fn = lua.create_function(|lua, (owner, repo): (String, String)| {
        validate_repo_segment(&owner)?;
        validate_repo_segment(&repo)?;
        let client = create_client()?;
        let token = get_token();

        let url = format!("https://api.github.com/repos/{owner}/{repo}/issues");
        let mut request = client.get(&url);

        if let Some(t) = token {
            request = request.header("Authorization", format!("Bearer {t}"));
        }

        let response = request.send().map_err(map_err)?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_else(|_| "no body".to_string());
            return Err(mlua::Error::external(format!(
                "GitHub API error {status}: {body}"
            )));
        }

        let issues: Vec<GitHubIssue> = response.json().map_err(map_err)?;
        value_or_err(lua, Ok(issues))
    })?;
    t.set(
        "list_issues",
        permissions.guard(
            crate::plugin_permissions::Permission::GitHubRead,
            lua,
            list_issues_fn,
        )?,
    )?;

    let create_issue_fn = lua.create_function(
        |lua, (owner, repo, title, body): (String, String, String, String)| {
            validate_repo_segment(&owner)?;
            validate_repo_segment(&repo)?;
            let client = create_client()?;
            let token = get_token().ok_or_else(|| {
                mlua::Error::external("GITHUB_TOKEN environment variable not set")
            })?;

            let url = format!("https://api.github.com/repos/{owner}/{repo}/issues");
            let payload = serde_json::json!({
                "title": title,
                "body": body,
            });

            let response = client
                .post(&url)
                .header("Authorization", format!("Bearer {token}"))
                .header("User-Agent", "n00n")
                .json(&payload)
                .send()
                .map_err(map_err)?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().unwrap_or_else(|_| "no body".to_string());
                return Err(mlua::Error::external(format!(
                    "GitHub API error {status}: {body}"
                )));
            }

            let issue: GitHubIssue = response.json().map_err(map_err)?;
            value_or_err(lua, Ok(issue))
        },
    )?;
    t.set(
        "create_issue",
        permissions.guard(
            crate::plugin_permissions::Permission::GitHubWrite,
            lua,
            create_issue_fn,
        )?,
    )?;

    let list_prs_fn = lua.create_function(|lua, (owner, repo): (String, String)| {
        validate_repo_segment(&owner)?;
        validate_repo_segment(&repo)?;
        let client = create_client()?;
        let token = get_token();

        let url = format!("https://api.github.com/repos/{owner}/{repo}/pulls");
        let mut request = client.get(&url);

        if let Some(t) = token {
            request = request.header("Authorization", format!("Bearer {t}"));
        }

        let response = request.send().map_err(map_err)?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_else(|_| "no body".to_string());
            return Err(mlua::Error::external(format!(
                "GitHub API error {status}: {body}"
            )));
        }

        let prs: Vec<GitHubPullRequest> = response.json().map_err(map_err)?;
        value_or_err(lua, Ok(prs))
    })?;
    t.set(
        "list_prs",
        permissions.guard(
            crate::plugin_permissions::Permission::GitHubRead,
            lua,
            list_prs_fn,
        )?,
    )?;

    let get_repo_fn = lua.create_function(|lua, (owner, repo): (String, String)| {
        validate_repo_segment(&owner)?;
        validate_repo_segment(&repo)?;
        let client = create_client()?;
        let token = get_token();

        let url = format!("https://api.github.com/repos/{owner}/{repo}");
        let mut request = client.get(&url);

        if let Some(t) = token {
            request = request.header("Authorization", format!("Bearer {t}"));
        }

        let response = request.send().map_err(map_err)?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_else(|_| "no body".to_string());
            return Err(mlua::Error::external(format!(
                "GitHub API error {status}: {body}"
            )));
        }

        let repo_info: GitHubRepository = response.json().map_err(map_err)?;
        value_or_err(lua, Ok(repo_info))
    })?;
    t.set(
        "get_repo",
        permissions.guard(
            crate::plugin_permissions::Permission::GitHubRead,
            lua,
            get_repo_fn,
        )?,
    )?;

    Ok(t)
}

pub(crate) const DOCS: ModuleDoc = ModuleDoc {
    name: "n00n.github",
    kind: DocKind::Table,
    desc: "GitHub REST API client using reqwest. Provides structured access to GitHub issues, pull requests, and repository metadata. Reads GITHUB_TOKEN from environment for authentication.",
    fns: &[
        FnDoc {
            name: "list_issues",
            args: "{owner}, {repo}",
            desc: "List issues in a GitHub repository.",
            params: &[
                ParamDoc {
                    name: "{owner}",
                    ty: "string",
                    desc: "Repository owner (username or organization).",
                },
                ParamDoc {
                    name: "{repo}",
                    ty: "string",
                    desc: "Repository name.",
                },
            ],
            returns: "(table) Array of issue objects with number, title, state, user, body, and html_url.",
            example: "",
        },
        FnDoc {
            name: "create_issue",
            args: "{owner}, {repo}, {title}, {body}",
            desc: "Create a new issue in a GitHub repository. Requires GITHUB_TOKEN.",
            params: &[
                ParamDoc {
                    name: "{owner}",
                    ty: "string",
                    desc: "Repository owner (username or organization).",
                },
                ParamDoc {
                    name: "{repo}",
                    ty: "string",
                    desc: "Repository name.",
                },
                ParamDoc {
                    name: "{title}",
                    ty: "string",
                    desc: "Issue title.",
                },
                ParamDoc {
                    name: "{body}",
                    ty: "string",
                    desc: "Issue body (markdown).",
                },
            ],
            returns: "(table) Created issue object with number, title, state, user, body, and html_url.",
            example: "",
        },
        FnDoc {
            name: "list_prs",
            args: "{owner}, {repo}",
            desc: "List pull requests in a GitHub repository.",
            params: &[
                ParamDoc {
                    name: "{owner}",
                    ty: "string",
                    desc: "Repository owner (username or organization).",
                },
                ParamDoc {
                    name: "{repo}",
                    ty: "string",
                    desc: "Repository name.",
                },
            ],
            returns: "(table) Array of pull request objects with number, title, state, user, head, base, and html_url.",
            example: "",
        },
        FnDoc {
            name: "get_repo",
            args: "{owner}, {repo}",
            desc: "Get repository metadata from GitHub.",
            params: &[
                ParamDoc {
                    name: "{owner}",
                    ty: "string",
                    desc: "Repository owner (username or organization).",
                },
                ParamDoc {
                    name: "{repo}",
                    ty: "string",
                    desc: "Repository name.",
                },
            ],
            returns: "(table) Repository object with name, full_name, description, language, stargazers_count, forks_count, and html_url.",
            example: "",
        },
    ],
};
