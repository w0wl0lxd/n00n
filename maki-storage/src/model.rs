use std::fs;

use crate::{StateDir, atomic_write};

const MODEL_FILE: &str = "model";

pub fn persist_model(dir: &StateDir, spec: &str) {
    let _ = atomic_write(&dir.path().join(MODEL_FILE), spec.as_bytes());
}

pub fn read_model(dir: &StateDir) -> Option<String> {
    let raw = fs::read_to_string(dir.path().join(MODEL_FILE)).ok()?;
    let spec = raw.trim();
    (!spec.is_empty()).then(|| spec.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn round_trip() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());

        assert!(read_model(&dir).is_none());

        persist_model(&dir, "anthropic/claude-sonnet-4");
        assert_eq!(
            read_model(&dir).as_deref(),
            Some("anthropic/claude-sonnet-4")
        );

        persist_model(&dir, "openai/gpt-5.4-nano");
        assert_eq!(read_model(&dir).as_deref(), Some("openai/gpt-5.4-nano"));
    }

    #[test]
    fn whitespace_and_empty_treated_as_none() {
        let tmp = TempDir::new().unwrap();
        let dir = StateDir::from_path(tmp.path().to_path_buf());

        fs::write(dir.path().join(MODEL_FILE), "  \n").unwrap();
        assert!(read_model(&dir).is_none());

        fs::write(dir.path().join(MODEL_FILE), "").unwrap();
        assert!(read_model(&dir).is_none());
    }
}
