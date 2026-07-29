use std::fs;
use std::path::Path;

use ignore::WalkBuilder;

use crate::Error;
use crate::chunk::{Chunk, chunk_file};

const MAX_FILE_BYTES: u64 = 1_048_576;

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
        if should_skip(&path) {
            continue;
        }
        let metadata = fs::metadata(&path)?;
        if metadata.len() > MAX_FILE_BYTES {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let rel = match path.strip_prefix(&repo) {
            Ok(rel) => rel.to_path_buf(),
            Err(_) => path.clone(),
        };
        chunks.extend(chunk_file(&rel, &content));
    }

    Ok(chunks)
}

fn should_skip(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return true;
    };
    if name.starts_with('.') && name != ".env.example" {
        return true;
    }
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
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
    use super::collect_chunks;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn collect_chunks_indexes_text_files() {
        let dir = tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("mkdir");
        fs::write(root.join("src/a.rs"), "fn alpha() {}\n\nfn beta() {}").expect("write");

        let chunks = collect_chunks(root).expect("collect");
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].file_path.ends_with("src/a.rs"));
    }
}
