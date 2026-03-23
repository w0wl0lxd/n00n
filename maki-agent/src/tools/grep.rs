use std::path::Path;

use grep_regex::RegexMatcher;
use grep_searcher::SearcherBuilder;
use grep_searcher::sinks::UTF8;
use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;

use crate::{GrepFileEntry, GrepMatch, ToolOutput};
use maki_tool_macro::Tool;
use tracing::debug;

use super::{NO_FILES_FOUND, mtime, relative_path, resolve_search_path, truncate_bytes};

pub(super) const INVALID_REGEX: &str = "invalid regex pattern";

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
    pub const EXAMPLES: Option<&str> = Some(r#"[{"pattern": "fn main", "include": "*.rs"}]"#);

    pub async fn execute(&self, ctx: &super::ToolContext) -> Result<ToolOutput, String> {
        let pattern = self.pattern.clone();
        let include = self.include.clone();
        let path = self.path.clone();
        let search_limit = ctx.config.search_result_limit;
        let max_line_bytes = ctx.config.max_line_bytes;

        smol::unblock(move || {
            let search_path = resolve_search_path(path.as_deref())?;
            debug!(
                pattern = %pattern,
                include = ?include,
                path = %search_path,
                "grep executing"
            );

            let matcher = RegexMatcher::new_line_matcher(&pattern)
                .or_else(|_| RegexMatcher::new(&pattern))
                .map_err(|e| format!("{INVALID_REGEX}: {e}"))?;

            let mut walker = WalkBuilder::new(&search_path);
            walker.hidden(false);

            if let Some(glob) = &include {
                let mut overrides = OverrideBuilder::new(&search_path);
                overrides
                    .add(glob)
                    .map_err(|e| format!("invalid glob pattern: {e}"))?;
                walker.overrides(
                    overrides
                        .build()
                        .map_err(|e| format!("invalid glob pattern: {e}"))?,
                );
            }

            let mut searcher = SearcherBuilder::new()
                .binary_detection(grep_searcher::BinaryDetection::quit(b'\x00'))
                .line_number(true)
                .build();

            let search = Path::new(&search_path);
            let base = if search.is_file() {
                search.parent().unwrap_or(search)
            } else {
                search
            };
            let mut entries: Vec<GrepFileEntry> = Vec::new();

            for entry in walker.build().flatten() {
                if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                    continue;
                }
                let path = entry.into_path();
                let mut file_matches = Vec::new();

                let _ = searcher.search_path(
                    &matcher,
                    &path,
                    UTF8(|line_nr, text| {
                        let text = text.strip_suffix('\n').unwrap_or(text);
                        let text = text.strip_suffix('\r').unwrap_or(text);
                        file_matches.push(GrepMatch {
                            line_nr: line_nr as usize,
                            text: truncate_bytes(text, max_line_bytes),
                        });
                        Ok(true)
                    }),
                );

                if !file_matches.is_empty() {
                    let rel = path
                        .strip_prefix(base)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .into_owned();
                    entries.push(GrepFileEntry {
                        path: rel,
                        matches: file_matches,
                    });
                }
            }

            if entries.is_empty() {
                return Ok(ToolOutput::Plain(NO_FILES_FOUND.to_string()));
            }

            entries.sort_by(|a, b| {
                let a_abs = base.join(&a.path);
                let b_abs = base.join(&b.path);
                mtime(&b_abs).cmp(&mtime(&a_abs))
            });

            let mut total = 0;
            for entry in &mut entries {
                let remaining = search_limit.saturating_sub(total);
                entry.matches.truncate(remaining);
                total += entry.matches.len();
            }
            entries.retain(|e| !e.matches.is_empty());

            Ok(ToolOutput::GrepResult { entries })
        })
        .await
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
}

impl super::ToolDefaults for Grep {}

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
