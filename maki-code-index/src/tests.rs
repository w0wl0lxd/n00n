use std::path::Path;

use test_case::test_case;

use crate::{IndexError, Language, MAX_FILE_SIZE, index_file, index_source};

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

#[test]
fn unsupported_extension() {
    assert!(matches!(
        index_file(Path::new("file.yaml"), MAX_FILE_SIZE),
        Err(IndexError::UnsupportedLanguage(_))
    ));
}

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
            "Red, Green",
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
            "pub utils, internal",
            "macros:",
            "my_macro!",
        ],
    );
    lacks(&out, &["{{"]);
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

#[test]
fn java_all_sections() {
    let src = r#"
package com.example;

import java.util.List;
import java.io.IOException;

public class Service extends BaseService implements Runnable, Serializable {
    private String name;
    public Service(String name) { this.name = name; }
    @Override
    public String toString() { return name; }
    public void process(List<String> items) throws IOException {}
}

/** Handler docs */
public interface Handler extends Comparable<Handler> {
    void handle(String request);
}

public enum Direction implements Displayable {
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
            "public class Service extends BaseService implements Runnable, Serializable",
            "private String name",
            "public Service(String name)",
            "@Override public String toString()",
            "public void process(List<String> items)",
            "traits:",
            "public interface Handler extends Comparable<Handler>",
            "void handle(String request)",
            "types:",
            "public enum Direction implements Displayable",
            "UP, DOWN",
        ],
    );
}
