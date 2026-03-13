use std::fs;

use crate::DataDir;

const THEME_FILE: &str = "theme";

pub fn persist_theme_name(dir: &DataDir, name: &str) {
    let _ = fs::write(dir.path().join(THEME_FILE), name);
}

pub fn read_theme_name(dir: &DataDir) -> Option<String> {
    let name = fs::read_to_string(dir.path().join(THEME_FILE)).ok()?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn theme_persistence_round_trip() {
        let tmp = TempDir::new().unwrap();
        let dir = DataDir::from_path(tmp.path().to_path_buf());

        assert!(read_theme_name(&dir).is_none());

        persist_theme_name(&dir, "gruvbox");
        assert_eq!(read_theme_name(&dir).as_deref(), Some("gruvbox"));

        fs::write(dir.path().join(THEME_FILE), "  \n").unwrap();
        assert!(read_theme_name(&dir).is_none());
    }
}
