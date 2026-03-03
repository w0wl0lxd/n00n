use std::path::Path;
use std::process::{Command, Stdio};

use maki_providers::{GrepFileEntry, GrepMatch, ToolInput, ToolOutput};
use maki_tool_macro::Tool;
use tracing::debug;

use super::{
    NO_FILES_FOUND, SEARCH_RESULT_LIMIT, mtime, relative_path, resolve_search_path, truncate_bytes,
};

#[derive(Tool, Debug, Clone)]
pub struct Grep {
    #[param(description = "Regex pattern to search for")]
    pattern: String,
    #[param(description = "Directory to search in (default: cwd)")]
    path: Option<String>,
    #[param(description = "File glob filter (e.g. *.rs)")]
    include: Option<String>,
}

impl Grep {
    pub const NAME: &str = "grep";
    pub const DESCRIPTION: &str = include_str!("grep.md");
    pub const EXAMPLES: Option<&str> = Some(
        r#"[
  {"pattern": "fn main", "include": "*.rs"},
  {"pattern": "TODO|FIXME", "path": "src/"},
  {"pattern": "impl\\s+\\w+", "include": "*.rs", "path": "src/models"}
]"#,
    );

    pub fn execute(&self, _ctx: &super::ToolContext) -> Result<ToolOutput, String> {
        let search_path = resolve_search_path(self.path.as_deref())?;

        debug!(
            pattern = %self.pattern,
            pattern_debug = ?self.pattern,
            include = ?self.include,
            path = %search_path,
            "grep executing"
        );

        let mut cmd = Command::new("rg");
        cmd.args([
            "-nH",
            "--hidden",
            "--no-messages",
            "--field-match-separator",
            "|",
            "--regexp",
            &self.pattern,
        ]);
        if let Some(glob) = &self.include {
            cmd.args(["--glob", glob]);
        }
        cmd.arg(&search_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = cmd.output().map_err(|e| format!("failed to run rg: {e}"))?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.is_empty() {
            debug!(stderr = %stderr, status = ?output.status, "rg stderr");
        }
        let stdout = String::from_utf8_lossy(&output.stdout);

        let prefix = search_path.strip_suffix('/').unwrap_or(&search_path);

        let mut entries: Vec<GrepFileEntry> = Vec::new();
        for line in stdout.lines() {
            let Some((file, rest)) = line.split_once('|') else {
                continue;
            };
            let Some((line_num, text)) = rest.split_once('|') else {
                continue;
            };
            let text = truncate_bytes(text);
            let rel = file
                .strip_prefix(prefix)
                .and_then(|p| p.strip_prefix('/'))
                .unwrap_or(file);
            let m = GrepMatch {
                line_nr: line_num.parse().unwrap_or(0),
                text,
            };
            match entries.last_mut().filter(|e| e.path == rel) {
                Some(entry) => entry.matches.push(m),
                None => entries.push(GrepFileEntry {
                    path: rel.to_string(),
                    matches: vec![m],
                }),
            }
        }

        if entries.is_empty() {
            return Ok(ToolOutput::Plain(NO_FILES_FOUND.to_string()));
        }

        entries.sort_by(|a, b| {
            let a_abs = Path::new(prefix).join(&a.path);
            let b_abs = Path::new(prefix).join(&b.path);
            mtime(&b_abs).cmp(&mtime(&a_abs))
        });

        let mut total = 0;
        for entry in &mut entries {
            let remaining = SEARCH_RESULT_LIMIT.saturating_sub(total);
            entry.matches.truncate(remaining);
            total += entry.matches.len();
        }
        entries.retain(|e| !e.matches.is_empty());

        Ok(ToolOutput::GrepResult { entries })
    }

    pub fn start_summary(&self) -> String {
        let mut s = self.pattern.clone();
        if let Some(inc) = &self.include {
            s.push_str(" [");
            s.push_str(inc);
            s.push(']');
        }
        if let Some(dir) = &self.path {
            s.push_str(" in ");
            s.push_str(&relative_path(dir));
        }
        s
    }

    pub fn start_input(&self) -> Option<ToolInput> {
        None
    }

    pub fn start_output(&self) -> Option<ToolOutput> {
        None
    }

    pub fn mutable_path(&self) -> Option<&str> {
        None
    }
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;

    #[test_case("fn main", None,        None,           "fn main"              ; "pattern_only")]
    #[test_case("TODO",    Some("*.rs"), None,           "TODO [*.rs]"          ; "with_include")]
    #[test_case("TODO",    None,         Some("src/"),   "TODO in src/"         ; "with_path")]
    #[test_case("TODO",    Some("*.rs"), Some("src/"),   "TODO [*.rs] in src/" ; "with_include_and_path")]
    fn start_summary_cases(
        pattern: &str,
        include: Option<&str>,
        path: Option<&str>,
        expected: &str,
    ) {
        let g = Grep {
            pattern: pattern.into(),
            include: include.map(Into::into),
            path: path.map(Into::into),
        };
        assert_eq!(g.start_summary(), expected);
    }
}
