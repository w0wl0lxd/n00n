use std::fmt::Write;
use std::fs;
use std::path::Path;

use crate::agent;
use crate::{InstructionBlock, ToolOutput};
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
            if Path::new(&path).is_dir() {
                return Self::list_dir(&path);
            }

            let raw = fs::read_to_string(&path).map_err(|e| format!("read error: {e}"))?;
            let total_lines = raw.lines().count();

            let start = offset.unwrap_or(1).saturating_sub(1);
            let limit = limit.unwrap_or(MAX_OUTPUT_LINES);

            let lines: Vec<String> = raw
                .lines()
                .skip(start)
                .take(limit)
                .map(truncate_bytes)
                .collect();

            let instructions = if let Ok(cwd) = std::env::current_dir() {
                let found = agent::find_subdirectory_instructions(Path::new(&path), &cwd, &loaded);
                let blocks: Vec<InstructionBlock> = found
                    .into_iter()
                    .map(|(display, content)| InstructionBlock {
                        path: display,
                        content,
                    })
                    .collect();
                if blocks.is_empty() {
                    None
                } else {
                    Some(blocks)
                }
            } else {
                None
            };

            Ok(ToolOutput::ReadCode {
                path,
                start_line: start + 1,
                lines,
                total_lines,
                instructions,
            })
        })
        .await
    }

    fn list_dir(path: &str) -> Result<ToolOutput, String> {
        let entries = fs::read_dir(path).map_err(|e| format!("read error: {e}"))?;

        let mut dirs = Vec::new();
        let mut files = Vec::new();

        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                dirs.push(format!("{name}/"));
            } else {
                files.push(name);
            }
        }

        dirs.sort_unstable();
        files.sort_unstable();
        dirs.append(&mut files);

        Ok(ToolOutput::Plain(dirs.join("\n")))
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

    #[test]
    fn list_dir_returns_sorted_entries_dirs_first() {
        let dir = tempfile::TempDir::new().unwrap();
        let dir_path = dir.path().to_string_lossy().to_string();

        std::fs::write(dir.path().join("b.txt"), "").unwrap();
        std::fs::write(dir.path().join("a.rs"), "").unwrap();
        std::fs::create_dir(dir.path().join("zdir")).unwrap();
        std::fs::create_dir(dir.path().join("adir")).unwrap();

        let result = Read::list_dir(&dir_path).unwrap();
        let text = result.as_text();
        let entries: Vec<&str> = text.lines().collect();
        assert_eq!(entries, vec!["adir/", "zdir/", "a.rs", "b.txt"]);
    }
}
