use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;
use maki_tool_macro::Tool;

use super::{NO_FILES_FOUND, SEARCH_RESULT_LIMIT, mtime, resolve_search_path};

#[derive(Tool, Debug, Clone)]
pub struct Glob {
    #[param(description = "Glob pattern (e.g. **/*.rs, src/**/*.ts)")]
    pattern: String,
    #[param(description = "Directory to search in (default: cwd)")]
    path: Option<String>,
}

impl Glob {
    pub const NAME: &str = "glob";
    pub const DESCRIPTION: &str = include_str!("glob.md");

    pub fn execute(&self, _ctx: &super::ToolContext) -> Result<String, String> {
        let search_path = resolve_search_path(self.path.as_deref())?;

        let mut overrides = OverrideBuilder::new(&search_path);
        overrides
            .add(&self.pattern)
            .map_err(|e| format!("invalid glob pattern: {e}"))?;
        let overrides = overrides
            .build()
            .map_err(|e| format!("glob build error: {e}"))?;

        let mut entries: Vec<_> = WalkBuilder::new(&search_path)
            .hidden(false)
            .overrides(overrides)
            .build()
            .flatten()
            .filter(|e| e.file_type().is_some_and(|ft| ft.is_file()))
            .map(|e| {
                let p = e.into_path();
                (mtime(&p), p.to_string_lossy().into_owned())
            })
            .collect();

        if entries.is_empty() {
            return Ok(NO_FILES_FOUND.to_string());
        }

        entries.sort_unstable_by(|a, b| b.0.cmp(&a.0));
        entries.truncate(SEARCH_RESULT_LIMIT);

        Ok(entries
            .into_iter()
            .map(|(_, p)| p)
            .collect::<Vec<_>>()
            .join("\n"))
    }

    pub fn start_summary(&self) -> String {
        self.pattern.clone()
    }

    pub fn mutable_path(&self) -> Option<&str> {
        None
    }
}
