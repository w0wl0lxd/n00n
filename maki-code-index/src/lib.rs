//! Parses source files into compact skeletons: imports, types, functions, line numbers.
//! Uses tree-sitter for language-specific AST walking. Each language has a `LanguageExtractor`
//! that knows which nodes matter and how to summarize them. Output is ~70-90% smaller than
//! the original file while preserving the structural information an LLM needs.
//! Language support is feature-gated so unused grammars are not compiled in.

use std::path::Path;

use tree_sitter::Parser;

mod common;
#[cfg(feature = "lang-go")]
mod go;
#[cfg(feature = "lang-java")]
mod java;
#[cfg(feature = "lang-python")]
mod python;
#[cfg(feature = "lang-rust")]
mod rust;
#[cfg(feature = "lang-typescript")]
mod typescript;

#[cfg(test)]
mod tests;

use common::{LanguageExtractor, detect_module_doc, doc_comment_start_line, format_skeleton};

#[cfg(test)]
const MAX_FILE_SIZE: u64 = 2 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("unsupported file type: {0}")]
    UnsupportedLanguage(String),
    #[error("file too large ({size} bytes, max {max})")]
    FileTooLarge { size: u64, max: u64 },
    #[error("read error: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse error: tree-sitter failed to parse file")]
    ParseFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    #[cfg(feature = "lang-rust")]
    Rust,
    #[cfg(feature = "lang-python")]
    Python,
    #[cfg(feature = "lang-typescript")]
    TypeScript,
    #[cfg(feature = "lang-typescript")]
    JavaScript,
    #[cfg(feature = "lang-go")]
    Go,
    #[cfg(feature = "lang-java")]
    Java,
}

impl Language {
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            #[cfg(feature = "lang-rust")]
            "rs" => Some(Self::Rust),
            #[cfg(feature = "lang-python")]
            "py" | "pyi" => Some(Self::Python),
            #[cfg(feature = "lang-typescript")]
            "ts" | "tsx" => Some(Self::TypeScript),
            #[cfg(feature = "lang-typescript")]
            "js" | "jsx" | "mjs" | "cjs" => Some(Self::JavaScript),
            #[cfg(feature = "lang-go")]
            "go" => Some(Self::Go),
            #[cfg(feature = "lang-java")]
            "java" => Some(Self::Java),
            _ => None,
        }
    }

    fn ts_language(&self) -> tree_sitter::Language {
        match self {
            #[cfg(feature = "lang-rust")]
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            #[cfg(feature = "lang-python")]
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            #[cfg(feature = "lang-typescript")]
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            #[cfg(feature = "lang-typescript")]
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            #[cfg(feature = "lang-go")]
            Self::Go => tree_sitter_go::LANGUAGE.into(),
            #[cfg(feature = "lang-java")]
            Self::Java => tree_sitter_java::LANGUAGE.into(),
        }
    }

    fn extractor(&self) -> &dyn LanguageExtractor {
        match self {
            #[cfg(feature = "lang-rust")]
            Self::Rust => &rust::RustExtractor,
            #[cfg(feature = "lang-python")]
            Self::Python => &python::PythonExtractor,
            #[cfg(feature = "lang-typescript")]
            Self::TypeScript => &typescript::TsJsExtractor,
            #[cfg(feature = "lang-typescript")]
            Self::JavaScript => &typescript::TsJsExtractor,
            #[cfg(feature = "lang-go")]
            Self::Go => &go::GoExtractor,
            #[cfg(feature = "lang-java")]
            Self::Java => &java::JavaExtractor,
        }
    }
}

pub fn index_file(path: &Path, max_file_size: u64) -> Result<String, IndexError> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let lang = Language::from_extension(ext)
        .ok_or_else(|| IndexError::UnsupportedLanguage(format!(".{ext}")))?;

    let meta = std::fs::metadata(path)?;
    if meta.len() > max_file_size {
        return Err(IndexError::FileTooLarge {
            size: meta.len(),
            max: max_file_size,
        });
    }

    let source = std::fs::read(path)?;
    index_source(&source, lang)
}

pub fn index_source(source: &[u8], lang: Language) -> Result<String, IndexError> {
    let mut parser = Parser::new();
    parser
        .set_language(&lang.ts_language())
        .map_err(|_| IndexError::ParseFailed)?;

    let tree = parser.parse(source, None).ok_or(IndexError::ParseFailed)?;
    let root = tree.root_node();
    let extractor = lang.extractor();

    let module_doc = detect_module_doc(root, source, extractor);
    let mut entries = Vec::new();
    let mut test_lines: Vec<usize> = Vec::new();

    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if extractor.is_attr(child) || extractor.is_doc_comment(child, source) {
            continue;
        }
        let attrs = extractor.collect_preceding_attrs(child);
        if extractor.is_test_node(child, source, &attrs) {
            test_lines.push(child.start_position().row + 1);
            continue;
        }
        for (i, mut entry) in extractor
            .extract_nodes(child, source, &attrs)
            .into_iter()
            .enumerate()
        {
            if i == 0
                && let Some(doc_start) = doc_comment_start_line(child, source, extractor)
            {
                entry.line_start = entry.line_start.min(doc_start);
            }
            entries.push(entry);
        }
    }

    Ok(format_skeleton(
        &entries,
        &test_lines,
        module_doc,
        extractor.import_separator(),
    ))
}
