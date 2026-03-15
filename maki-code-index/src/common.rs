//! Shared skeleton formatting and tree-sitter helpers used by all language extractors.
//! `LanguageExtractor` trait defines the per-language hooks; `format_skeleton` groups entries
//! by `Section` (sorted by enum discriminant order, not source order) and renders them.
//! Imports get special treatment: same-root paths are consolidated (e.g. two `std::` uses merge).

use std::collections::BTreeMap;
use std::fmt::Write;

use tree_sitter::Node;

pub(crate) const FIELD_TRUNCATE_THRESHOLD: usize = 8;
const LINE_WRAP_THRESHOLD: usize = 120;
const WRAP_INDENT: &str = "    ";

pub(crate) fn node_text<'a>(node: Node<'a>, source: &'a [u8]) -> &'a str {
    node.utf8_text(source).unwrap_or("")
}

#[allow(dead_code)]
pub(crate) fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let boundary = s
        .char_indices()
        .nth(max_chars.saturating_sub(3))
        .map_or(s.len(), |(i, _)| i);
    format!("{}...", &s[..boundary])
}

pub(crate) fn line_range(start: usize, end: usize) -> String {
    if start == end {
        format!("[{start}]")
    } else {
        format!("[{start}-{end}]")
    }
}

#[cfg(feature = "lang-rust")]
pub(crate) fn has_test_attr(attrs: &[Node], source: &[u8]) -> bool {
    attrs.iter().any(|a| {
        let text = node_text(*a, source);
        text == "#[test]" || text == "#[cfg(test)]" || text.ends_with("::test]")
    })
}

pub(crate) fn doc_comment_start_line(
    node: Node,
    source: &[u8],
    extractor: &dyn LanguageExtractor,
) -> Option<usize> {
    let mut earliest: Option<usize> = None;
    let mut prev = node.prev_sibling();
    while let Some(p) = prev {
        if extractor.is_attr(p) {
            prev = p.prev_sibling();
            continue;
        }
        if extractor.is_doc_comment(p, source) {
            earliest = Some(p.start_position().row + 1);
            prev = p.prev_sibling();
        } else {
            break;
        }
    }
    earliest
}

pub(crate) fn detect_module_doc(
    root: Node,
    source: &[u8],
    extractor: &dyn LanguageExtractor,
) -> Option<(usize, usize)> {
    let mut cursor = root.walk();
    let mut start = None;
    let mut end = None;
    for child in root.children(&mut cursor) {
        if extractor.is_module_doc(child, source) {
            let line = child.start_position().row + 1;
            if start.is_none() {
                start = Some(line);
            }
            let end_pos = child.end_position();
            let end_line = if end_pos.column == 0 {
                end_pos.row
            } else {
                end_pos.row + 1
            };
            end = Some(end_line);
        } else if !extractor.is_attr(child) && !child.is_extra() {
            break;
        }
    }
    start.map(|s| (s, end.unwrap()))
}

#[cfg(feature = "lang-rust")]
pub(crate) fn relevant_attr_texts(attrs: &[Node], source: &[u8]) -> Vec<String> {
    attrs
        .iter()
        .filter_map(|a| {
            let text = node_text(*a, source);
            (text.contains("derive") || text.contains("cfg")).then(|| text.to_string())
        })
        .collect()
}

#[cfg(feature = "lang-rust")]
pub(crate) fn vis_prefix<'a>(node: Node<'a>, source: &'a [u8]) -> &'a str {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "visibility_modifier" {
            return node_text(child, source);
        }
    }
    ""
}

pub(crate) fn prefixed(vis: &str, rest: std::fmt::Arguments<'_>) -> String {
    if vis.is_empty() {
        format!("{rest}")
    } else {
        format!("{vis} {rest}")
    }
}

pub(crate) fn find_child<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut cursor = node.walk();
    node.children(&mut cursor).find(|c| c.kind() == kind)
}

#[cfg(feature = "lang-rust")]
pub(crate) fn fn_signature(node: Node, source: &[u8]) -> Option<String> {
    let name = node
        .child_by_field_name("name")
        .map(|n| node_text(n, source))?;
    let params = find_child(node, "parameters")
        .map(|n| node_text(n, source))
        .unwrap_or("()");
    let ret = node
        .child_by_field_name("return_type")
        .map(|n| {
            let t = node_text(n, source);
            if t.starts_with("->") {
                format!(" {t}")
            } else {
                format!(" -> {t}")
            }
        })
        .unwrap_or_default();
    Some(format!("{name}{params}{ret}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[allow(dead_code)]
pub(crate) enum Section {
    Import,
    Module,
    Constant,
    Type,
    Trait,
    Impl,
    Function,
    Class,
    Macro,
    Test,
}

impl Section {
    pub(crate) fn header(self) -> &'static str {
        match self {
            Self::Import => "imports:",
            Self::Module => "mod:",
            Self::Constant => "consts:",
            Self::Type => "types:",
            Self::Trait => "traits:",
            Self::Impl => "impls:",
            Self::Function => "fns:",
            Self::Class => "classes:",
            Self::Macro => "macros:",
            Self::Test => "tests:",
        }
    }
}

pub(crate) struct SkeletonEntry {
    pub(crate) section: Section,
    pub(crate) line_start: usize,
    pub(crate) line_end: usize,
    pub(crate) text: String,
    pub(crate) children: Vec<String>,
    pub(crate) attrs: Vec<String>,
}

impl SkeletonEntry {
    pub(crate) fn new(section: Section, node: Node, text: String) -> Self {
        Self {
            section,
            line_start: node.start_position().row + 1,
            line_end: node.end_position().row + 1,
            text,
            children: Vec::new(),
            attrs: Vec::new(),
        }
    }
}

pub(crate) trait LanguageExtractor {
    fn extract_nodes(&self, node: Node, source: &[u8], attrs: &[Node]) -> Vec<SkeletonEntry>;
    fn is_test_node(&self, node: Node, source: &[u8], attrs: &[Node]) -> bool;
    fn is_doc_comment(&self, node: Node, source: &[u8]) -> bool;
    fn is_module_doc(&self, node: Node, source: &[u8]) -> bool;
    fn import_separator(&self) -> &'static str {
        "::"
    }
    fn is_attr(&self, _node: Node) -> bool {
        false
    }
    fn collect_preceding_attrs<'a>(&self, node: Node<'a>) -> Vec<Node<'a>> {
        let mut attrs = Vec::new();
        let mut prev = node.prev_sibling();
        while let Some(p) = prev {
            if self.is_attr(p) {
                attrs.push(p);
            } else {
                break;
            }
            prev = p.prev_sibling();
        }
        attrs.reverse();
        attrs
    }
}

pub(crate) fn format_skeleton(
    entries: &[SkeletonEntry],
    test_lines: &[usize],
    module_doc: Option<(usize, usize)>,
    import_sep: &str,
) -> String {
    let mut out = String::new();

    if let Some((start, end)) = module_doc {
        let _ = writeln!(out, "module doc: {}", line_range(start, end));
    }

    let mut grouped: BTreeMap<Section, Vec<&SkeletonEntry>> = BTreeMap::new();
    for entry in entries {
        grouped.entry(entry.section).or_default().push(entry);
    }

    for (section, items) in &grouped {
        if section == &Section::Import {
            format_imports(&mut out, items, import_sep);
        } else {
            let sep = if out.is_empty() { "" } else { "\n" };
            let _ = writeln!(out, "{sep}{}", section.header());
            for entry in items {
                for attr in &entry.attrs {
                    let _ = writeln!(out, "  {attr}");
                }
                let _ = writeln!(
                    out,
                    "  {} {}",
                    entry.text,
                    line_range(entry.line_start, entry.line_end)
                );
                for child in &entry.children {
                    let _ = writeln!(out, "    {child}");
                }
            }
        }
    }

    if !test_lines.is_empty() {
        let min = *test_lines.iter().min().unwrap();
        let max = *test_lines.iter().max().unwrap();
        let sep = if out.is_empty() { "" } else { "\n" };
        let _ = writeln!(out, "{sep}tests: {}", line_range(min, max));
    }

    out
}

fn format_imports(out: &mut String, entries: &[&SkeletonEntry], sep: &str) {
    if entries.is_empty() {
        return;
    }

    let min_line = entries.iter().map(|e| e.line_start).min().unwrap();
    let max_line = entries.iter().map(|e| e.line_end).max().unwrap();

    let prefix = if out.is_empty() { "" } else { "\n" };
    let _ = writeln!(out, "{prefix}imports: {}", line_range(min_line, max_line));

    let mut trie = ImportTrie::default();
    for entry in entries {
        for path in expand_import(&entry.text, sep) {
            trie.insert(&path);
        }
    }

    for line in trie.render(sep) {
        let wrapped = wrap_line(&line);
        let _ = writeln!(out, "  {wrapped}");
    }
}

/// Finds the first occurrence of `sep` outside braces.
fn find_sep(text: &str, sep: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let sep_bytes = sep.as_bytes();
    let mut depth = 0u32;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            _ if depth == 0 && bytes[i..].starts_with(sep_bytes) => return Some(i),
            _ => {}
        }
        i += 1;
    }
    None
}

/// Splits `text` on `delim` at brace-depth 0, trimming each part.
fn split_top_level(text: &str, delim: u8) -> Vec<&str> {
    let mut results = Vec::new();
    let mut depth = 0u32;
    let mut start = 0;
    for (i, &b) in text.as_bytes().iter().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => depth = depth.saturating_sub(1),
            c if c == delim && depth == 0 => {
                results.push(text[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = text[start..].trim();
    if !last.is_empty() {
        results.push(last);
    }
    results
}

/// Expands a possibly-braced import string into a list of segment paths.
///
/// `"a::{b::C, d}"` with sep `"::"` -> `[["a", "b", "C"], ["a", "d"]]`
///
/// Iterative: splits on the first separator, then if the remainder is a
/// brace-group, fans out over each comma-delimited alternative. Uses an
/// explicit work-stack instead of recursion so deeply nested inputs can't
/// blow the call stack.
fn expand_import(text: &str, sep: &str) -> Vec<Vec<String>> {
    let mut results: Vec<Vec<String>> = Vec::new();
    let mut stack: Vec<(Vec<String>, &str)> = vec![(Vec::new(), text.trim())];

    while let Some((prefix, remaining)) = stack.pop() {
        if remaining.is_empty() {
            if !prefix.is_empty() {
                results.push(prefix);
            }
            continue;
        }

        let Some(pos) = find_sep(remaining, sep) else {
            let mut path = prefix;
            path.push(remaining.to_string());
            results.push(path);
            continue;
        };

        let segment = &remaining[..pos];
        let rest = &remaining[pos + sep.len()..];

        let mut new_prefix = prefix;
        new_prefix.push(segment.to_string());

        if let Some(inner) = rest.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            let items = split_top_level(inner, b',');
            for item in &items[..items.len() - 1] {
                stack.push((new_prefix.clone(), item));
            }
            if let Some(last) = items.last() {
                stack.push((new_prefix, last));
            }
        } else {
            stack.push((new_prefix, rest));
        }
    }

    results
}

#[derive(Default)]
struct ImportTrie {
    children: BTreeMap<String, ImportTrie>,
    is_leaf: bool,
}

impl ImportTrie {
    fn insert(&mut self, segments: &[String]) {
        let mut node = self;
        for seg in segments {
            node = node.children.entry(seg.clone()).or_default();
        }
        node.is_leaf = true;
    }

    fn render(&self, sep: &str) -> Vec<String> {
        render_children(&self.children, sep)
    }
}

/// Renders a single trie node as one or more output strings.
///
/// - Leaf with no children -> `"seg"`
/// - Leaf WITH children -> emits `seg` on its own line, plus all children
///   prefixed with `seg{sep}` (e.g. `io` and `io::Write` stay separate so
///   both the module import and the item import are visible)
/// - Non-leaf with exactly 1 child -> collapse: `seg{sep}child`
/// - Non-leaf with N children -> brace-group: `seg{sep}{a, b, c}`
fn render_node(seg: &str, node: &ImportTrie, sep: &str) -> Vec<String> {
    if node.children.is_empty() {
        return vec![seg.to_string()];
    }

    let rendered = render_children(&node.children, sep);

    if node.is_leaf {
        let mut out = vec![seg.to_string()];
        for item in &rendered {
            out.push(format!("{seg}{sep}{item}"));
        }
        return out;
    }

    if rendered.len() == 1 {
        vec![format!("{seg}{sep}{}", rendered[0])]
    } else {
        vec![format!("{seg}{sep}{{{}}}", rendered.join(", "))]
    }
}

fn render_children(children: &BTreeMap<String, ImportTrie>, sep: &str) -> Vec<String> {
    children
        .iter()
        .flat_map(|(seg, node)| render_node(seg, node, sep))
        .collect()
}

fn wrap_line(line: &str) -> String {
    if line.len() <= LINE_WRAP_THRESHOLD {
        return line.to_string();
    }

    let Some(brace_start) = line.find('{') else {
        return line.to_string();
    };
    let brace_end = line.rfind('}').unwrap_or(line.len());
    let inner = &line[brace_start + 1..brace_end];

    let parts = split_top_level(inner, b',');
    if parts.len() <= 1 {
        return line.to_string();
    }

    let prefix = &line[..brace_start];
    let joined = parts
        .iter()
        .map(|p| format!("\n{WRAP_INDENT}{p}"))
        .collect::<String>();

    format!("{prefix}{{{joined}\n}}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    fn trie_render(imports: &[&str], sep: &str) -> Vec<String> {
        let mut trie = ImportTrie::default();
        for &imp in imports {
            for segments in expand_import(imp, sep) {
                trie.insert(&segments);
            }
        }
        trie.render(sep)
    }

    #[test_case(&["std::io"],           "::", &["std::io"]                              ; "single_import")]
    #[test_case(&["std::io", "std::fs"],"::", &["std::{fs, io}"]                        ; "shared_prefix")]
    #[test_case(
        &["crate::a::X", "crate::a::Y", "crate::b::Z"],
        "::",
        &["crate::{a::{X, Y}, b::Z}"]
        ; "deep_shared_prefix"
    )]
    #[test_case(
        &["std::io", "std::io::Write"],
        "::",
        &["std::{io, io::Write}"]
        ; "leaf_and_children"
    )]
    #[test_case(&["std::{fs, net}"],    "::", &["std::{fs, net}"]                       ; "grouped_input")]
    #[test_case(
        &["java.util.List", "java.io.IOException"],
        ".",
        &["java.{io.IOException, util.List}"]
        ; "java_dots"
    )]
    #[test_case(
        &["github.com/user/foo", "github.com/user/bar"],
        "/",
        &["github.com/user/{bar, foo}"]
        ; "go_slashes"
    )]
    #[test_case(
        &["crate::x::key as kb", "crate::x::Foo"],
        "::",
        &["crate::x::{Foo, key as kb}"]
        ; "as_rename"
    )]
    #[test_case(&["os"],                "::", &["os"]                                   ; "no_separator")]
    fn trie_compression(imports: &[&str], sep: &str, expected: &[&str]) {
        let result = trie_render(imports, sep);
        assert_eq!(result, expected, "imports: {imports:?}, sep: {sep:?}");
    }

    #[test]
    fn wrap_long_line() {
        let long = format!(
            "crate::module::{{{}}}",
            (0..20)
                .map(|i| format!("Item{i}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let wrapped = wrap_line(&long);
        assert!(wrapped.contains('\n'));
        assert!(wrapped.starts_with("crate::module::{"));
        assert!(wrapped.ends_with('}'));
    }

    #[test]
    fn short_line_not_wrapped() {
        let short = "std::{fs, io}";
        assert_eq!(wrap_line(short), short);
    }

    #[test]
    fn expand_nested_braces() {
        let mut result = expand_import("a::{b::{C, D}, e::F}", "::");
        result.sort();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0], vec!["a", "b", "C"]);
        assert_eq!(result[1], vec!["a", "b", "D"]);
        assert_eq!(result[2], vec!["a", "e", "F"]);
    }

    #[test]
    fn wildcard_import() {
        let result = trie_render(&["std::io::*", "std::io::Write"], "::");
        assert_eq!(result, &["std::io::{*, Write}"]);
    }

    #[test]
    fn expand_empty_input() {
        assert!(expand_import("", "::").is_empty());
        assert!(expand_import("  ", "::").is_empty());
    }

    #[test]
    fn duplicate_imports_deduplicated() {
        let result = trie_render(&["std::io", "std::io", "std::fs"], "::");
        assert_eq!(result, &["std::{fs, io}"]);
    }

    #[test]
    fn single_segment_mixed_with_paths() {
        let result = trie_render(&["os", "std::io"], "::");
        assert_eq!(result, &["os", "std::io"]);
    }
}
