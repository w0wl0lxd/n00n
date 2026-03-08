use std::fmt::Write;
use std::fs;
use std::path::Path;

use crate::ToolOutput;
use crate::agent;
use maki_tool_macro::Tool;

use super::{MAX_OUTPUT_LINES, relative_path, truncate_bytes};

#[derive(Tool, Debug, Clone)]
pub struct Read {
    #[param(description = "Absolute path to the file or directory")]
    path: String,
    #[param(description = "Line number to start from (1-indexed)")]
    offset: Option<usize>,
    #[param(description = "Max number of lines to read")]
    limit: Option<usize>,
}

impl Read {
    pub const NAME: &str = "read";
    pub const DESCRIPTION: &str = include_str!("read.md");
    pub const EXAMPLES: Option<&str> = Some(
        r#"[
  {"path": "/home/user/project/src/main.rs"},
  {"path": "/home/user/project/src/lib.rs", "offset": 50, "limit": 30},
  {"path": "/home/user/project/src"}
]"#,
    );

    pub async fn execute(&self, ctx: &super::ToolContext) -> Result<ToolOutput, String> {
        let path = self.path.clone();
        let offset = self.offset;
        let limit = self.limit;
        let loaded = ctx.loaded_instructions.clone();
        smol::unblock(move || {
            let raw = fs::read_to_string(&path).map_err(|e| format!("read error: {e}"))?;

            let start = offset.unwrap_or(1).saturating_sub(1);
            let limit = limit.unwrap_or(MAX_OUTPUT_LINES);

            let mut lines: Vec<String> = raw
                .lines()
                .skip(start)
                .take(limit)
                .map(truncate_bytes)
                .collect();

            if let Ok(cwd) = std::env::current_dir() {
                let instructions =
                    agent::find_subdirectory_instructions(Path::new(&path), &cwd, &loaded);
                for (display, content) in instructions {
                    lines.push(String::new());
                    lines.push(format!("---\nInstructions from: {display}"));
                    lines.extend(content.lines().map(String::from));
                }
            }

            Ok(ToolOutput::ReadCode {
                path,
                start_line: start + 1,
                lines,
            })
        })
        .await
    }

    pub fn start_summary(&self) -> String {
        let mut s = relative_path(&self.path);
        let start = self.offset.unwrap_or(1);
        match (self.offset.is_some(), self.limit) {
            (_, Some(l)) => {
                let _ = write!(s, ":{start}-{}", start + l - 1);
            }
            (true, None) => {
                let _ = write!(s, ":{start}");
            }
            _ => {}
        }
        s
    }
}

impl super::ToolDefaults for Read {}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;

    #[test_case(None,      None,      "/a/b.rs"       ; "path_only")]
    #[test_case(Some(10),  None,      "/a/b.rs:10"    ; "offset_only")]
    #[test_case(None,      Some(25),  "/a/b.rs:1-25"  ; "limit_only")]
    #[test_case(Some(50),  Some(51),  "/a/b.rs:50-100" ; "offset_and_limit")]
    fn start_summary_cases(offset: Option<usize>, limit: Option<usize>, expected: &str) {
        let r = Read {
            path: "/a/b.rs".into(),
            offset,
            limit,
        };
        assert_eq!(r.start_summary(), expected);
    }
}
