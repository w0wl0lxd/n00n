use std::path::Path;

const MAX_CHUNK_LINES: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub file_path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
    pub language: String,
}

pub fn language_for_path(path: &Path) -> String {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => ext.to_ascii_lowercase(),
        None => String::from("text"),
    }
}

pub fn chunk_file(path: &Path, content: &str) -> Vec<Chunk> {
    let file_path = path.to_string_lossy().into_owned();
    let language = language_for_path(path);
    let mut chunks = Vec::new();
    let mut block_start = 1usize;
    let mut block_lines: Vec<&str> = Vec::new();

    let flush = |chunks: &mut Vec<Chunk>,
                 file_path: &str,
                 language: &str,
                 block_start: &mut usize,
                 block_lines: &mut Vec<&str>| {
        if block_lines.is_empty() {
            return;
        }
        let start = *block_start;
        let end_line = start + block_lines.len() - 1;
        chunks.push(Chunk {
            file_path: file_path.to_owned(),
            start_line: start,
            end_line,
            content: block_lines.join("\n"),
            language: language.to_owned(),
        });
        *block_start = end_line + 1;
        block_lines.clear();
    };

    for (idx, line) in content.lines().enumerate() {
        let line_no = idx + 1;
        if line.trim().is_empty() {
            flush(
                &mut chunks,
                &file_path,
                &language,
                &mut block_start,
                &mut block_lines,
            );
            block_start = line_no + 1;
            continue;
        }

        block_lines.push(line);
        if block_lines.len() >= MAX_CHUNK_LINES {
            flush(
                &mut chunks,
                &file_path,
                &language,
                &mut block_start,
                &mut block_lines,
            );
            block_start = line_no + 1;
        }
    }

    flush(
        &mut chunks,
        &file_path,
        &language,
        &mut block_start,
        &mut block_lines,
    );
    chunks
}

#[cfg(test)]
mod tests {
    use super::{chunk_file, language_for_path};
    use std::path::Path;

    #[test]
    fn language_for_path_uses_extension() {
        assert_eq!(language_for_path(Path::new("src/lib.rs")), "rs");
    }

    #[test]
    fn chunk_file_splits_on_blank_lines() {
        let chunks = chunk_file(
            Path::new("src/a.rs"),
            "fn one() {}\n\nfn two() {\n  line\n}",
        );
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].end_line, 1);
        assert_eq!(chunks[1].start_line, 3);
        assert_eq!(chunks[1].end_line, 5);
    }
}
