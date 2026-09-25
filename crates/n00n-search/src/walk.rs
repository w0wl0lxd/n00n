use std::fs;
use std::path::{Component, Path};

use ignore::WalkBuilder;

use crate::Error;
use crate::chunk::{Chunk, chunk_file};

const MAX_FILE_BYTES: u64 = 1_048_576;
const ENV_EXAMPLE_FILE: &str = ".env.example";
const STATE_DIRECTORIES: [&str; 5] = [".git", ".hg", ".jj", ".n00n", ".svn"];

pub fn collect_chunks(repo: &Path) -> Result<Vec<Chunk>, Error> {
    let repo = repo.canonicalize().map_err(Error::from)?;
    let mut chunks = Vec::new();

    let walker = WalkBuilder::new(&repo)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build();

    for entry in walker {
        let entry = entry.map_err(|err| Error::Io {
            source: std::io::Error::other(err.to_string()),
        })?;
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.into_path();
        let Ok(rel) = path.strip_prefix(&repo) else {
            continue;
        };
        let rel = rel.to_path_buf();
        if should_skip(&rel) {
            continue;
        }
        let metadata = fs::metadata(&path)?;
        if metadata.len() > MAX_FILE_BYTES {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        chunks.extend(chunk_file(&rel, &content));
    }

    Ok(chunks)
}

/// Skips hidden files and anything inside a generated state directory such
/// as `.n00n/` or `.git/`. Other hidden directories (`.github/`, `.cargo/`)
/// hold tracked source and configuration, so they stay indexed.
fn should_skip(relative: &Path) -> bool {
    let Some(name) = relative.file_name().and_then(|n| n.to_str()) else {
        return true;
    };
    if relative.components().any(|component| {
        matches!(component, Component::Normal(part)
            if part.to_str().is_some_and(|part| STATE_DIRECTORIES.contains(&part)))
    }) {
        return true;
    }
    if name.starts_with('.') && name != ENV_EXAMPLE_FILE {
        return true;
    }
    matches!(
        relative.extension().and_then(|ext| ext.to_str()),
        Some(
            "png"
                | "jpg"
                | "jpeg"
                | "gif"
                | "webp"
                | "ico"
                | "pdf"
                | "zip"
                | "gz"
                | "wasm"
                | "so"
                | "dylib"
                | "dll"
                | "exe"
                | "bin"
                | "lock"
        )
    )
}

#[cfg(test)]
mod tests {
    use super::{collect_chunks, should_skip};
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;
    use test_case::test_case;

    #[test_case(".github/workflows/rust.yml", false ; "tracked_ci_workflow")]
    #[test_case(".cargo/config.toml", false ; "tracked_cargo_config")]
    #[test_case("src/a.rs", false ; "plain_source")]
    #[test_case(".env.example", false ; "env_example")]
    #[test_case(".n00n/search/meta.json", true ; "n00n_state")]
    #[test_case(".git/config", true ; "git_state")]
    #[test_case("vendor/lib/.git/config", true ; "nested_git_state")]
    #[test_case(".env", true ; "hidden_file")]
    #[test_case("assets/logo.png", true ; "binary_extension")]
    fn should_skip_only_state_directories_and_hidden_files(path: &str, skipped: bool) {
        assert_eq!(should_skip(Path::new(path)), skipped);
    }

    #[test]
    fn collect_chunks_indexes_text_files() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("mkdir");
        fs::write(
            root.join("src").join("a.rs"),
            "fn alpha() {}\n\nfn beta() {}",
        )
        .expect("write");

        let chunks = collect_chunks(root).expect("collect");
        assert_eq!(chunks.len(), 2);
        assert!(Path::new(&chunks[0].file_path).ends_with(Path::new("src/a.rs")));
    }

    #[test]
    fn collect_chunks_skips_hidden_state_directories() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join(".n00n/search/tantivy_index")).expect("mkdir");
        fs::write(
            root.join(".n00n/search/tantivy_index/meta.json"),
            r#"{"index_format_version":7}"#,
        )
        .expect("write");
        fs::write(root.join("lib.rs"), "fn indexed_symbol() {}").expect("write");

        let chunks = collect_chunks(root).expect("collect");

        let indexed: Vec<&str> = chunks
            .iter()
            .map(|chunk| chunk.file_path.as_str())
            .collect();
        assert!(
            indexed.iter().all(|path| !path.starts_with(".n00n")),
            "index state must not be indexed: {indexed:?}"
        );
        assert!(
            indexed.contains(&"lib.rs"),
            "source file missing: {indexed:?}"
        );
    }
}
