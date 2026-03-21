use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::AgentMode;
use crate::template::Vars;

const INSTRUCTION_FILES: &[&str] = &[
    "AGENTS.md",
    "CLAUDE.md",
    ".github/copilot-instructions.md",
    "COPILOT.md",
    ".cursorrules",
    ".windsurfrules",
    ".clinerules",
    "CONVENTIONS.md",
    "GEMINI.md",
    "CODING_AGENT.md",
];

#[derive(Clone, Default)]
pub struct LoadedInstructions(Arc<Mutex<HashSet<PathBuf>>>);

impl LoadedInstructions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn contains_or_insert(&self, path: PathBuf) -> bool {
        let mut set = self.0.lock().unwrap_or_else(|e| e.into_inner());
        !set.insert(path)
    }
}

pub(crate) fn is_instruction_file(name: &str) -> bool {
    INSTRUCTION_FILES
        .iter()
        .any(|f| *f == name || Path::new(f).file_name().is_some_and(|n| n == name))
}

pub fn build_system_prompt(vars: &Vars, mode: &AgentMode, instructions: &str) -> String {
    let mut out = crate::prompt::SYSTEM_PROMPT.to_string();

    out.push_str(&vars.apply(
        "\n\nEnvironment:\n- Working directory: {cwd}\n- Platform: {platform}\n- Date: {date}",
    ));

    out.push_str(instructions);

    if let AgentMode::Plan(plan_path) = mode {
        let plan_vars = Vars::new().set("{plan_path}", plan_path.display().to_string());
        out.push_str(&plan_vars.apply(crate::prompt::PLAN_PROMPT));
    }

    out
}

pub fn load_instruction_files(cwd: &str) -> (String, LoadedInstructions) {
    let root = Path::new(cwd);
    let mut out = String::new();
    let loaded = LoadedInstructions::new();
    for filename in INSTRUCTION_FILES {
        let path = root.join(filename);
        if let Ok(content) = fs::read_to_string(&path) {
            out.push_str(&format!(
                "\n\nProject instructions ({filename}):\n{content}"
            ));
            if let Ok(canonical) = path.canonicalize() {
                loaded.contains_or_insert(canonical);
            }
            break;
        }
    }
    (out, loaded)
}

pub fn find_subdirectory_instructions(
    dir: &Path,
    cwd: &Path,
    loaded: &LoadedInstructions,
) -> Vec<(String, String)> {
    let Ok(cwd) = cwd.canonicalize() else {
        return Vec::new();
    };
    let Ok(dir) = dir.canonicalize() else {
        return Vec::new();
    };

    if !dir.starts_with(&cwd) || dir == cwd {
        return Vec::new();
    }

    let mut results = Vec::new();
    let mut current = dir.as_path();
    while current != cwd {
        for filename in INSTRUCTION_FILES {
            let Ok(canonical) = current.join(filename).canonicalize() else {
                continue;
            };
            if loaded.contains_or_insert(canonical.clone()) {
                continue;
            }
            if let Ok(content) = fs::read_to_string(&canonical) {
                let display = canonical.display().to_string();
                results.push((display, content));
                break;
            }
        }
        current = match current.parent() {
            Some(p) => p,
            None => break,
        };
    }
    results
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use test_case::test_case;

    use super::*;

    const PLAN_PATH: &str = ".maki/plans/123.md";

    #[test_case(&AgentMode::Build, false ; "build_excludes_plan")]
    #[test_case(&AgentMode::Plan(PathBuf::from(PLAN_PATH)), true ; "plan_includes_plan")]
    fn plan_section_presence(mode: &AgentMode, expect_plan: bool) {
        let vars = Vars::new().set("{cwd}", "/tmp").set("{platform}", "linux");
        let prompt = build_system_prompt(&vars, mode, "");
        assert_eq!(prompt.contains("Plan Mode"), expect_plan);
        if expect_plan {
            assert!(prompt.contains(PLAN_PATH));
        }
    }

    #[test_case("AGENTS.md",                true  ; "direct_match")]
    #[test_case("CLAUDE.md",                true  ; "claude_md")]
    #[test_case("copilot-instructions.md",  true  ; "nested_path_filename")]
    #[test_case(".cursorrules",             true  ; "dotfile")]
    #[test_case("random.md",                false ; "unrelated_file")]
    #[test_case("not-AGENTS.md",            false ; "partial_match")]
    fn is_instruction_file_cases(name: &str, expected: bool) {
        assert_eq!(is_instruction_file(name), expected);
    }

    #[test]
    fn find_subdirectory_instructions_discovers_agents_md() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("src").join("api");
        fs::create_dir_all(&sub).unwrap();
        fs::write(dir.path().join("src").join("AGENTS.md"), "api rules").unwrap();

        let loaded = LoadedInstructions::new();
        let results = find_subdirectory_instructions(&sub, dir.path(), &loaded);

        assert_eq!(results.len(), 1);
        assert!(results[0].0.ends_with("AGENTS.md"));
        assert_eq!(results[0].1, "api rules");
    }

    #[test]
    fn find_subdirectory_instructions_skips_root() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("AGENTS.md"), "root rules").unwrap();

        let loaded = LoadedInstructions::new();
        let from_root = find_subdirectory_instructions(dir.path(), dir.path(), &loaded);
        assert!(from_root.is_empty(), "should skip root-level directory");
    }

    #[test]
    fn find_subdirectory_instructions_deduplicates() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("src");
        fs::create_dir_all(&sub).unwrap();
        let agents_path = sub.join("AGENTS.md");
        fs::write(&agents_path, "rules").unwrap();

        let canonical = agents_path.canonicalize().unwrap();
        let loaded = LoadedInstructions::new();
        loaded.contains_or_insert(canonical);
        let pre_loaded = find_subdirectory_instructions(&sub, dir.path(), &loaded);
        assert!(pre_loaded.is_empty(), "should skip already-loaded files");

        let loaded = LoadedInstructions::new();
        let first = find_subdirectory_instructions(&sub, dir.path(), &loaded);
        let second = find_subdirectory_instructions(&sub, dir.path(), &loaded);
        assert_eq!(first.len(), 1);
        assert!(
            second.is_empty(),
            "should not return same file twice across calls"
        );
    }

    #[test]
    fn load_instruction_files_populates_loaded_set() {
        let dir = tempfile::tempdir().unwrap();
        let agents_path = dir.path().join("AGENTS.md");
        fs::write(&agents_path, "project rules").unwrap();
        let expected_canonical = agents_path.canonicalize().unwrap();

        let (text, loaded) = load_instruction_files(dir.path().to_str().unwrap());

        assert!(text.contains("project rules"));
        assert!(loaded.contains_or_insert(expected_canonical));
    }
}
