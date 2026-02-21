use std::path::Path;
use std::process::{Command, Stdio};

use maki_providers::{ToolInput, ToolOutput};
use maki_tool_macro::Tool;

use super::{NO_FILES_FOUND, SEARCH_RESULT_LIMIT, mtime, resolve_search_path};

const MAX_GREP_LINE_LENGTH: usize = 2000;

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

    pub fn execute(&self, _ctx: &super::ToolContext) -> Result<ToolOutput, String> {
        let search_path = resolve_search_path(self.path.as_deref())?;

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
        let stdout = String::from_utf8_lossy(&output.stdout);

        let prefix = search_path.strip_suffix('/').unwrap_or(&search_path);

        let mut files: Vec<(String, Vec<String>)> = Vec::new();
        for line in stdout.lines() {
            let Some((file, rest)) = line.split_once('|') else {
                continue;
            };
            let Some((line_num, text)) = rest.split_once('|') else {
                continue;
            };
            let mut text = text.to_string();
            if text.len() > MAX_GREP_LINE_LENGTH {
                let boundary = text.floor_char_boundary(MAX_GREP_LINE_LENGTH);
                text.truncate(boundary);
                text.push_str("...");
            }
            let rel = file
                .strip_prefix(prefix)
                .and_then(|p| p.strip_prefix('/'))
                .unwrap_or(file);
            let formatted = format!("  {line_num}: {text}");
            match files.last_mut().filter(|(f, _)| f == rel) {
                Some((_, lines)) => lines.push(formatted),
                None => files.push((rel.to_string(), vec![formatted])),
            }
        }

        if files.is_empty() {
            return Ok(ToolOutput::Plain(NO_FILES_FOUND.to_string()));
        }

        files.sort_by(|a, b| {
            let a_abs = Path::new(prefix).join(&a.0);
            let b_abs = Path::new(prefix).join(&b.0);
            mtime(&b_abs).cmp(&mtime(&a_abs))
        });

        let mut result = String::new();
        let mut total = 0;
        for (file, lines) in &files {
            if total >= SEARCH_RESULT_LIMIT {
                break;
            }
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(file);
            result.push(':');
            for line in lines {
                if total >= SEARCH_RESULT_LIMIT {
                    break;
                }
                result.push('\n');
                result.push_str(line);
                total += 1;
            }
        }

        Ok(ToolOutput::Plain(result))
    }

    pub fn start_summary(&self) -> String {
        let mut s = self.pattern.clone();
        if let Some(inc) = &self.include {
            s.push_str(" [");
            s.push_str(inc);
            s.push(']');
        }
        if let Some(p) = &self.path {
            s.push_str(" in ");
            s.push_str(p);
        }
        s
    }

    pub fn start_input(&self) -> Option<ToolInput> {
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

    #[test_case("fn main", None,          None,          "fn main"                ; "pattern_only")]
    #[test_case("TODO",    Some("*.rs"),  None,          "TODO [*.rs]"            ; "with_include")]
    #[test_case("TODO",    None,          Some("src/"),  "TODO in src/"           ; "with_path")]
    #[test_case("TODO",    Some("*.rs"),  Some("src/"),  "TODO [*.rs] in src/"    ; "with_both")]
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
