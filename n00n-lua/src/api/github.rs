use mlua::{Lua, Result as LuaResult, Table};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::env;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::api::util::convert::json_to_lua;
use crate::docs::{DocKind, FnDoc, ModuleDoc, ParamDoc};

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
    pub body: Option<String>,
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

#[derive(Debug)]
enum GitHubError {
    RateLimited { retry_after: Option<u64> },
}

fn gh_path() -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("gh"))
            .find(|candidate| candidate.is_file())
    })
}

fn resolve_token(
    provided_token: Option<String>,
    gh_tried: Arc<AtomicBool>,
    env_token: Option<String>,
) -> Option<String> {
    if let Some(t) = env_token {
        return Some(t);
    }
    if let Some(t) = provided_token {
        return Some(t);
    }
    if !gh_tried.swap(true, Ordering::SeqCst)
        && let Some(gh) = gh_path()
        && let Ok(output) = std::process::Command::new(gh)
            .args(["auth", "token"])
            .output()
        && output.status.success()
        && let Ok(token) = String::from_utf8(output.stdout)
    {
        let token = token.trim().to_string();
        if !token.is_empty() {
            return Some(token);
        }
    }
    None
}

fn get_token(provided_token: Option<String>, gh_tried: Arc<AtomicBool>) -> Option<String> {
    resolve_token(provided_token, gh_tried, env::var("GITHUB_TOKEN").ok())
}

fn check_rate_limit(response: &reqwest::blocking::Response) -> Result<(), GitHubError> {
    if let Some(remaining) = response.headers().get("X-RateLimit-Remaining")
        && let Ok(remaining_str) = remaining.to_str()
        && remaining_str == "0"
    {
        let retry_after = response
            .headers()
            .get("X-RateLimit-Reset")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok());
        return Err(GitHubError::RateLimited { retry_after });
    }
    Ok(())
}

fn create_client() -> Result<Client, mlua::Error> {
    let mut builder = Client::builder().user_agent("n00n");
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

fn map_github_err(e: GitHubError) -> mlua::Error {
    match e {
        GitHubError::RateLimited { retry_after } => {
            let msg = if let Some(secs) = retry_after {
                format!(
                    "GitHub API rate limit exceeded. Retry after {secs} seconds (Unix timestamp)."
                )
            } else {
                "GitHub API rate limit exceeded. Retry time unknown.".to_string()
            };
            mlua::Error::external(msg)
        }
    }
}

fn value_or_err<T: serde::Serialize>(
    lua: &Lua,
    result: Result<T, reqwest::Error>,
) -> LuaResult<mlua::Value> {
    let val = result.map_err(map_err)?;
    let json = serde_json::to_value(&val).map_err(|e| mlua::Error::external(format!("{e:#}")))?;
    json_to_lua(lua, &json)
}

pub(crate) fn create_github_table(lua: &Lua) -> LuaResult<Table> {
    let t = lua.create_table()?;
    let gh_tried = Arc::new(AtomicBool::new(false));

    let list_issues_fn = lua.create_function({
        let gh_tried = Arc::clone(&gh_tried);
        move |lua, (owner, repo, token): (String, String, Option<String>)| {
            let client = create_client()?;
            let token = get_token(token, Arc::clone(&gh_tried));

            let url = format!("https://api.github.com/repos/{owner}/{repo}/issues");
            let mut request = client.get(&url);

            if let Some(t) = token {
                request = request.header("Authorization", format!("Bearer {t}"));
            }

            let response = request.send().map_err(map_err)?;
            check_rate_limit(&response).map_err(map_github_err)?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().unwrap_or_else(|_| "no body".to_string());
                return Err(mlua::Error::external(format!(
                    "GitHub API error {status}: {body}"
                )));
            }

            let issues: Vec<GitHubIssue> = response.json().map_err(map_err)?;
            value_or_err(lua, Ok(issues))
        }
    })?;
    t.set("list_issues", list_issues_fn)?;

    let create_issue_fn = lua.create_function(
        {
            let gh_tried = Arc::clone(&gh_tried);
            move |lua,
         (owner, repo, title, body, token): (
            String,
            String,
            String,
            String,
            Option<String>,
        )| {
            let client = create_client()?;
            let token = get_token(token, Arc::clone(&gh_tried)).ok_or_else(|| {
                mlua::Error::external(
                    "GitHub token not found. Set GITHUB_TOKEN, pass token parameter, or install gh CLI.",
                )
            })?;

            let url = format!("https://api.github.com/repos/{owner}/{repo}/issues");
            let payload = serde_json::json!({
                "title": title,
                "body": body,
            });

            let response = client
                .post(&url)
                .header("Authorization", format!("Bearer {token}"))
                .json(&payload)
                .send()
                .map_err(map_err)?;

            check_rate_limit(&response).map_err(map_github_err)?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().unwrap_or_else(|_| "no body".to_string());
                return Err(mlua::Error::external(format!(
                    "GitHub API error {status}: {body}"
                )));
            }

            let issue: GitHubIssue = response.json().map_err(map_err)?;
            value_or_err(lua, Ok(issue))
        }
    })?;
    t.set("create_issue", create_issue_fn)?;

    let list_prs_fn = lua.create_function({
        let gh_tried = Arc::clone(&gh_tried);
        move |lua, (owner, repo, token): (String, String, Option<String>)| {
            let client = create_client()?;
            let token = get_token(token, Arc::clone(&gh_tried));

            let url = format!("https://api.github.com/repos/{owner}/{repo}/pulls");
            let mut request = client.get(&url);

            if let Some(t) = token {
                request = request.header("Authorization", format!("Bearer {t}"));
            }

            let response = request.send().map_err(map_err)?;
            check_rate_limit(&response).map_err(map_github_err)?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().unwrap_or_else(|_| "no body".to_string());
                return Err(mlua::Error::external(format!(
                    "GitHub API error {status}: {body}"
                )));
            }

            let prs: Vec<GitHubPullRequest> = response.json().map_err(map_err)?;
            value_or_err(lua, Ok(prs))
        }
    })?;
    t.set("list_prs", list_prs_fn)?;

    let get_repo_fn = lua.create_function({
        let gh_tried = Arc::clone(&gh_tried);
        move |lua, (owner, repo, token): (String, String, Option<String>)| {
            let client = create_client()?;
            let token = get_token(token, Arc::clone(&gh_tried));

            let url = format!("https://api.github.com/repos/{owner}/{repo}");
            let mut request = client.get(&url);

            if let Some(t) = token {
                request = request.header("Authorization", format!("Bearer {t}"));
            }

            let response = request.send().map_err(map_err)?;
            check_rate_limit(&response).map_err(map_github_err)?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().unwrap_or_else(|_| "no body".to_string());
                return Err(mlua::Error::external(format!(
                    "GitHub API error {status}: {body}"
                )));
            }

            let repo_info: GitHubRepository = response.json().map_err(map_err)?;
            value_or_err(lua, Ok(repo_info))
        }
    })?;
    t.set("get_repo", get_repo_fn)?;

    let get_issue_fn = lua.create_function({
        let gh_tried = Arc::clone(&gh_tried);
        move |lua, (owner, repo, issue_number, token): (String, String, u64, Option<String>)| {
            let client = create_client()?;
            let token = get_token(token, Arc::clone(&gh_tried));

            let url = format!("https://api.github.com/repos/{owner}/{repo}/issues/{issue_number}");
            let mut request = client.get(&url);

            if let Some(t) = token {
                request = request.header("Authorization", format!("Bearer {t}"));
            }

            let response = request.send().map_err(map_err)?;
            check_rate_limit(&response).map_err(map_github_err)?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().unwrap_or_else(|_| "no body".to_string());
                return Err(mlua::Error::external(format!(
                    "GitHub API error {status}: {body}"
                )));
            }

            let issue: GitHubIssue = response.json().map_err(map_err)?;
            value_or_err(lua, Ok(issue))
        }
    })?;
    t.set("get_issue", get_issue_fn)?;

    let get_pr_fn = lua.create_function({
        let gh_tried = Arc::clone(&gh_tried);
        move |lua, (owner, repo, pr_number, token): (String, String, u64, Option<String>)| {
            let client = create_client()?;
            let token = get_token(token, Arc::clone(&gh_tried));

            let url = format!("https://api.github.com/repos/{owner}/{repo}/pulls/{pr_number}");
            let mut request = client.get(&url);

            if let Some(t) = token {
                request = request.header("Authorization", format!("Bearer {t}"));
            }

            let response = request.send().map_err(map_err)?;
            check_rate_limit(&response).map_err(map_github_err)?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().unwrap_or_else(|_| "no body".to_string());
                return Err(mlua::Error::external(format!(
                    "GitHub API error {status}: {body}"
                )));
            }

            let pr: GitHubPullRequest = response.json().map_err(map_err)?;
            value_or_err(lua, Ok(pr))
        }
    })?;
    t.set("get_pr", get_pr_fn)?;

    Ok(t)
}

pub(crate) const DOCS: ModuleDoc = ModuleDoc {
    name: "n00n.github",
    kind: DocKind::Table,
    desc: "GitHub REST API client using reqwest. Provides structured access to GitHub issues, pull requests, and repository metadata. Token sources: GITHUB_TOKEN env var, optional token parameter, or gh CLI fallback.",
    fns: &[
        FnDoc {
            name: "list_issues",
            args: "{owner}, {repo}[, {token}]",
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
                ParamDoc {
                    name: "{token}",
                    ty: "string?",
                    desc: "Optional GitHub token. Falls back to GITHUB_TOKEN env var or gh CLI.",
                },
            ],
            returns: "(table) Array of issue objects with number, title, state, user, body, and html_url.",
            example: "",
        },
        FnDoc {
            name: "create_issue",
            args: "{owner}, {repo}, {title}, {body}[, {token}]",
            desc: "Create a new issue in a GitHub repository. Requires authentication.",
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
                ParamDoc {
                    name: "{token}",
                    ty: "string?",
                    desc: "Optional GitHub token. Falls back to GITHUB_TOKEN env var or gh CLI.",
                },
            ],
            returns: "(table) Created issue object with number, title, state, user, body, and html_url.",
            example: "",
        },
        FnDoc {
            name: "list_prs",
            args: "{owner}, {repo}[, {token}]",
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
                ParamDoc {
                    name: "{token}",
                    ty: "string?",
                    desc: "Optional GitHub token. Falls back to GITHUB_TOKEN env var or gh CLI.",
                },
            ],
            returns: "(table) Array of pull request objects with number, title, state, user, head, base, body, and html_url.",
            example: "",
        },
        FnDoc {
            name: "get_repo",
            args: "{owner}, {repo}[, {token}]",
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
                ParamDoc {
                    name: "{token}",
                    ty: "string?",
                    desc: "Optional GitHub token. Falls back to GITHUB_TOKEN env var or gh CLI.",
                },
            ],
            returns: "(table) Repository object with name, full_name, description, language, stargazers_count, forks_count, and html_url.",
            example: "",
        },
        FnDoc {
            name: "get_issue",
            args: "{owner}, {repo}, {issue_number}[, {token}]",
            desc: "Get a single issue from GitHub.",
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
                    name: "{issue_number}",
                    ty: "integer",
                    desc: "Issue number.",
                },
                ParamDoc {
                    name: "{token}",
                    ty: "string?",
                    desc: "Optional GitHub token. Falls back to GITHUB_TOKEN env var or gh CLI.",
                },
            ],
            returns: "(table) Issue object with number, title, state, user, body, and html_url.",
            example: "",
        },
        FnDoc {
            name: "get_pr",
            args: "{owner}, {repo}, {pr_number}[, {token}]",
            desc: "Get a single pull request from GitHub.",
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
                    name: "{pr_number}",
                    ty: "integer",
                    desc: "Pull request number.",
                },
                ParamDoc {
                    name: "{token}",
                    ty: "string?",
                    desc: "Optional GitHub token. Falls back to GITHUB_TOKEN env var or gh CLI.",
                },
            ],
            returns: "(table) Pull request object with number, title, state, user, head, base, body, and html_url.",
            example: "",
        },
    ],
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::AtomicBool;

    fn parse_rate_limit_headers(headers: &HashMap<String, String>) -> Result<(), GitHubError> {
        if let Some(remaining) = headers.get("X-RateLimit-Remaining")
            && remaining == "0"
        {
            let retry_after = headers
                .get("X-RateLimit-Reset")
                .and_then(|s| s.parse::<u64>().ok());
            return Err(GitHubError::RateLimited { retry_after });
        }
        Ok(())
    }

    #[test]
    fn test_rate_limit_not_exceeded() {
        let mut headers = HashMap::new();
        headers.insert("X-RateLimit-Remaining".to_string(), "42".to_string());
        let result = parse_rate_limit_headers(&headers);
        assert!(result.is_ok());
    }

    #[test]
    fn test_rate_limit_exceeded_with_reset() {
        let mut headers = HashMap::new();
        headers.insert("X-RateLimit-Remaining".to_string(), "0".to_string());
        headers.insert("X-RateLimit-Reset".to_string(), "1234567890".to_string());
        let result = parse_rate_limit_headers(&headers);
        assert!(result.is_err());
        match result {
            Err(GitHubError::RateLimited { retry_after }) => {
                assert_eq!(retry_after, Some(1_234_567_890));
            }
            _ => panic!("Expected RateLimited error"),
        }
    }

    #[test]
    fn test_rate_limit_exceeded_without_reset() {
        let mut headers = HashMap::new();
        headers.insert("X-RateLimit-Remaining".to_string(), "0".to_string());
        let result = parse_rate_limit_headers(&headers);
        assert!(result.is_err());
        match result {
            Err(GitHubError::RateLimited { retry_after }) => {
                assert_eq!(retry_after, None);
            }
            _ => panic!("Expected RateLimited error"),
        }
    }

    #[test]
    fn test_rate_limit_malformed_reset() {
        let mut headers = HashMap::new();
        headers.insert("X-RateLimit-Remaining".to_string(), "0".to_string());
        headers.insert("X-RateLimit-Reset".to_string(), "invalid".to_string());
        let result = parse_rate_limit_headers(&headers);
        assert!(result.is_err());
        match result {
            Err(GitHubError::RateLimited { retry_after }) => {
                assert_eq!(retry_after, None);
            }
            _ => panic!("Expected RateLimited error"),
        }
    }

    #[test]
    fn test_rate_limit_no_headers() {
        let headers = HashMap::new();
        let result = parse_rate_limit_headers(&headers);
        assert!(result.is_ok());
    }

    #[test]
    fn resolve_token_prefers_env_over_provided() {
        let gh_tried = Arc::new(AtomicBool::new(false));
        let token = resolve_token(
            Some("provided_token".to_string()),
            Arc::clone(&gh_tried),
            Some("env_token".to_string()),
        );
        assert_eq!(token, Some("env_token".to_string()));
        assert!(!gh_tried.load(Ordering::SeqCst));
    }

    #[test]
    fn resolve_token_falls_back_to_provided() {
        let gh_tried = Arc::new(AtomicBool::new(false));
        let token = resolve_token(
            Some("provided_token".to_string()),
            Arc::clone(&gh_tried),
            None,
        );
        assert_eq!(token, Some("provided_token".to_string()));
        assert!(!gh_tried.load(Ordering::SeqCst));
    }

    #[test]
    fn resolve_token_tries_gh_only_once() {
        let gh_tried = Arc::new(AtomicBool::new(false));
        // gh may or may not be installed, so we only assert the state change
        // and that the second call does not retry the gh fallback.
        let _first = resolve_token(None, Arc::clone(&gh_tried), None);
        assert!(gh_tried.load(Ordering::SeqCst));

        let second = resolve_token(None, Arc::clone(&gh_tried), None);
        assert_eq!(second, None);
        assert!(gh_tried.load(Ordering::SeqCst));
    }
}
