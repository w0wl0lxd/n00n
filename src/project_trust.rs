use std::path::{Path, PathBuf};

use color_eyre::Result;
use color_eyre::eyre::bail;

const PROJECT_CONFIG_FILES: [&str; 3] = ["init.lua", "permissions.toml", "mcp.toml"];

pub fn require(cwd: &Path, trusted: bool) -> Result<bool> {
    let files = project_config_files(cwd);
    if files.is_empty() || trusted {
        return Ok(trusted);
    }
    let listed = files
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    bail!(
        "project configuration is untrusted and will not be executed: {listed}. Re-run with --trust-project to opt in"
    )
}

fn project_config_files(cwd: &Path) -> Vec<PathBuf> {
    let directory = cwd.join(".n00n");
    PROJECT_CONFIG_FILES
        .iter()
        .map(|name| directory.join(name))
        .filter(|path| path.is_file())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_project_config_needs_no_opt_in() {
        let directory = tempfile::tempdir().unwrap();
        assert!(!require(directory.path(), false).unwrap());
    }

    #[test]
    fn project_config_fails_closed_without_opt_in() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join(".n00n")).unwrap();
        std::fs::write(directory.path().join(".n00n/mcp.toml"), "[mcp]").unwrap();
        let error = require(directory.path(), false).unwrap_err();
        assert!(error.to_string().contains("--trust-project"));
    }

    #[test]
    fn explicit_opt_in_trusts_all_project_config_surfaces() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join(".n00n")).unwrap();
        for name in PROJECT_CONFIG_FILES {
            std::fs::write(directory.path().join(".n00n").join(name), "").unwrap();
        }
        assert!(require(directory.path(), true).unwrap());
    }
}
