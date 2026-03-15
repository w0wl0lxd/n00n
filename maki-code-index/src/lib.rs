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

use common::{LanguageExtractor, detect_module_doc, doc_comment_start_line, format_skeleton};

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

pub fn index_file(path: &Path) -> Result<String, IndexError> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let lang = Language::from_extension(ext)
        .ok_or_else(|| IndexError::UnsupportedLanguage(format!(".{ext}")))?;

    let meta = std::fs::metadata(path)?;
    if meta.len() > MAX_FILE_SIZE {
        return Err(IndexError::FileTooLarge {
            size: meta.len(),
            max: MAX_FILE_SIZE,
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

#[cfg(test)]
mod tests {
    use super::*;
    use common::truncate;
    use test_case::test_case;

    fn idx(source: &str, lang: Language) -> String {
        index_source(source.as_bytes(), lang).unwrap()
    }

    fn has(output: &str, needles: &[&str]) {
        for n in needles {
            assert!(output.contains(n), "missing {n:?} in:\n{output}");
        }
    }

    fn lacks(output: &str, needles: &[&str]) {
        for n in needles {
            assert!(!output.contains(n), "unexpected {n:?} in:\n{output}");
        }
    }

    #[test_case("rs", Some(Language::Rust)       ; "rust")]
    #[test_case("py", Some(Language::Python)      ; "python")]
    #[test_case("ts", Some(Language::TypeScript)  ; "typescript")]
    #[test_case("js", Some(Language::JavaScript)  ; "javascript")]
    #[test_case("tsx", Some(Language::TypeScript)  ; "tsx")]
    #[test_case("jsx", Some(Language::JavaScript)  ; "jsx")]
    #[test_case("pyi", Some(Language::Python)      ; "pyi")]
    #[test_case("mjs", Some(Language::JavaScript)  ; "mjs")]
    #[test_case("cjs", Some(Language::JavaScript)  ; "cjs")]
    #[test_case("go", Some(Language::Go)              ; "go")]
    #[test_case("java", Some(Language::Java)          ; "java")]
    #[test_case("yaml", None                       ; "unsupported")]
    #[test_case("", None                           ; "empty_ext")]
    fn language_from_extension(ext: &str, expected: Option<Language>) {
        assert_eq!(Language::from_extension(ext), expected);
    }

    #[test]
    fn unsupported_extension() {
        assert!(matches!(
            index_file(Path::new("file.yaml")),
            Err(IndexError::UnsupportedLanguage(_))
        ));
    }

    #[test]
    fn truncate_preserves_multibyte_boundary() {
        let long = format!("{}{}", "a".repeat(55), "ü".repeat(10));
        let result = truncate(&long, 60);
        assert!(result.ends_with("..."));
        assert!(result.chars().count() <= 60);
    }

    #[test]
    fn truncate_short_unchanged() {
        assert_eq!(truncate("hello", 60), "hello");
    }

    // --- Rust extraction ---

    #[test]
    fn rust_all_sections() {
        let src = "\
//! Module doc
use std::collections::HashMap;
use std::io;
use std::io::*;
use std::{fs, net};

const MAX: usize = 1024;
static COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct Config {
    pub name: String,
    pub port: u16,
}

pub struct Empty;

enum Color { Red, Green }

pub type Result<T> = std::result::Result<T, MyError>;

pub trait Handler {
    fn handle(&self, req: Request) -> Response;
}

impl Display for Foo {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, \"Foo\")
    }
}

impl Config {
    pub fn new(name: String) -> Self { todo!() }
    fn validate(&self) -> bool { true }
}

pub fn process(input: &str) -> Result<String, Error> { todo!() }

pub mod utils;
mod internal;

macro_rules! my_macro { () => {}; }
";
        let out = idx(src, Language::Rust);
        has(
            &out,
            &[
                "module doc:",
                "imports:",
                "std::",
                "collections::HashMap",
                "io",
                "io::*",
                "fs",
                "net",
                "consts:",
                "MAX: usize",
                "static COUNTER: AtomicU64",
                "types:",
                "#[derive(Debug, Clone)]",
                "pub struct Config",
                "pub name: String",
                "pub struct Empty",
                "enum Color",
                "Red",
                "type Result",
                "traits:",
                "pub Handler",
                "handle(&self, req: Request) -> Response",
                "impls:",
                "Display for Foo",
                "Config",
                "pub new(name: String) -> Self",
                "validate(&self) -> bool",
                "fns:",
                "pub process(input: &str)",
                "mod:",
                "pub utils",
                "internal",
                "macros:",
                "my_macro!",
            ],
        );
        lacks(&out, &["{{"]);
    }

    #[test]
    fn rust_section_ordering() {
        let src =
            "fn foo() {}\nuse std::io;\nconst X: u8 = 1;\npub struct S {}\ntrait T {}\nimpl S {}\n";
        let out = idx(src, Language::Rust);
        let positions: Vec<_> = ["imports:", "consts:", "types:", "traits:", "impls:", "fns:"]
            .iter()
            .map(|s| out.find(s).unwrap_or_else(|| panic!("missing {s}")))
            .collect();
        assert!(
            positions.windows(2).all(|w| w[0] < w[1]),
            "sections out of order in:\n{out}"
        );
    }

    #[test]
    fn rust_many_fields_truncated() {
        let out = idx(
            "struct Big {\n    a: u8,\n    b: u8,\n    c: u8,\n    d: u8,\n    e: u8,\n    f: u8,\n    g: u8,\n    h: u8,\n    i: u8,\n    j: u8,\n}\n",
            Language::Rust,
        );
        has(&out, &["..."]);
    }

    #[test]
    fn rust_test_module_collapsed() {
        let src = "fn main() {}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n    #[test]\n    fn it_works() { assert!(true); }\n}\n";
        let out = idx(src, Language::Rust);
        has(&out, &["tests:"]);
        lacks(&out, &["it_works"]);
    }

    #[test_case("#[test]\nfn it_works() { assert!(true); }\n",         true  ; "standalone_test")]
    #[test_case("#[tokio::test]\nasync fn my_test() {}\n",             true  ; "tokio_test")]
    #[test_case("#[attested]\nfn foo() {}\n",                          false ; "attested_not_test")]
    #[test_case("#[cfg(not(test))]\nfn real_fn() {}\n",                false ; "cfg_not_test")]
    #[test_case("#[my_crate::test_helper]\nfn setup() {}\n",           false ; "test_helper_not_test")]
    fn rust_test_detection(src: &str, is_test: bool) {
        let out = idx(src, Language::Rust);
        if is_test {
            has(&out, &["tests:"]);
            lacks(&out, &["fns:"]);
        } else {
            has(&out, &["fns:"]);
            lacks(&out, &["tests:"]);
        }
    }

    #[test_case(
        "/// Documented\n/// More docs\npub fn foo() {}\n",
        "pub foo() [1-3]"
        ; "doc_comment_extends_range"
    )]
    #[test_case(
        "/// Doc\n#[derive(Debug)]\npub struct Bar {\n    pub x: i32,\n}\n",
        "pub struct Bar [1-5]"
        ; "doc_plus_attr_extends_range"
    )]
    #[test_case(
        "pub fn plain() {}\n",
        "pub plain() [1]"
        ; "no_doc_comment"
    )]
    #[test_case(
        "// regular comment\npub fn foo() {}\n",
        "pub foo() [2]"
        ; "regular_comment_not_doc"
    )]
    fn rust_doc_comment_line_ranges(src: &str, expected: &str) {
        let out = idx(src, Language::Rust);
        has(&out, &[expected]);
    }

    // --- Python extraction ---

    #[test]
    fn python_all_sections() {
        let src = "\
\"\"\"Module docstring.\"\"\"

import os
from typing import Optional

MAX_RETRIES = 3
MY_VAR: int = 10

@dataclass
class MyClass:
    x: int = 0

class AuthService:
    def __init__(self, secret: str):
        self.secret = secret
    @staticmethod
    def validate(token: str) -> bool:
        return True

def process(data: list) -> dict:
    return {}
";
        let out = idx(src, Language::Python);
        has(
            &out,
            &[
                "module doc:",
                "imports:",
                "os",
                "typing.Optional",
                "consts:",
                "MAX_RETRIES",
                "MY_VAR = 10",
                "classes:",
                "MyClass [9-11]",
                "@staticmethod",
                "AuthService",
                "__init__(self, secret: str)",
                "validate(token: str) -> bool",
                "fns:",
                "process(data: list) -> dict",
            ],
        );
        lacks(&out, &["MY_VAR = int"]);
    }

    // --- TypeScript/JavaScript extraction ---

    #[test]
    fn ts_all_sections() {
        let src = "\
/** Function docs */
import { Request, Response } from 'express';

export interface Config {
    port: number;
    host: string;
}

export type ID = string | number;

export enum Direction { Up, Down }

export const PORT: number = 3000;

export class Service {
    process(input: string): string { return input; }
}

/** Handler doc */
export function handler(req: Request): Response { return new Response(); }
";
        let out = idx(src, Language::TypeScript);
        has(
            &out,
            &[
                "imports:",
                "{ Request, Response } from 'express'",
                "types:",
                "export interface Config",
                "port: number",
                "type ID",
                "export enum Direction",
                "consts:",
                "PORT",
                "classes:",
                "export Service",
                "fns:",
                "export handler(req: Request)",
            ],
        );
    }

    #[test]
    fn js_function() {
        let out = idx(
            "function hello(name) {\n    console.log(name);\n}\n",
            Language::JavaScript,
        );
        has(&out, &["fns:", "hello(name)"]);
    }

    // --- Go extraction ---

    #[test]
    fn go_all_sections() {
        let src = r#"
package main

import (
	"fmt"
	"os"
)

const MaxRetries = 3

const (
	A = 1
	B = 2
)

var GlobalVar = "hello"

type Point struct {
	X int
	Y int
}

type Reader interface {
	Read(p []byte) (int, error)
}

type Alias = int

// Method doc
func (p *Point) Distance() float64 {
	return 0
}

func main() {
	fmt.Println("hello")
}
"#;
        let out = idx(src, Language::Go);
        has(
            &out,
            &[
                "imports:",
                "fmt",
                "os",
                "consts:",
                "MaxRetries",
                "A",
                "B",
                "var GlobalVar",
                "types:",
                "struct Point",
                "X int",
                "Y int",
                "type Alias",
                "traits:",
                "Reader",
                "Read(p []byte) (int, error)",
                "impls:",
                "(p *Point) Distance() float64",
                "fns:",
                "main()",
            ],
        );
        lacks(&out, &["package"]);
    }

    // --- Java extraction ---

    #[test]
    fn java_all_sections() {
        let src = r#"
package com.example;

import java.util.List;
import java.io.IOException;

public class Service {
    private String name;
    public Service(String name) { this.name = name; }
    @Override
    public String toString() { return name; }
    public void process(List<String> items) throws IOException {}
}

/** Handler docs */
public interface Handler {
    void handle(String request);
}

public enum Direction {
    UP, DOWN, LEFT, RIGHT
}
"#;
        let out = idx(src, Language::Java);
        has(
            &out,
            &[
                "imports:",
                "java.{io.IOException, util.List}",
                "mod:",
                "com.example",
                "classes:",
                "public class Service",
                "private String name",
                "public Service(String name)",
                "@Override public String toString()",
                "public void process(List<String> items)",
                "traits:",
                "public interface Handler",
                "void handle(String request)",
                "types:",
                "public enum Direction",
                "UP",
                "DOWN",
            ],
        );
    }
}
