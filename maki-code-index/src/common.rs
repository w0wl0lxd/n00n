//! Shared skeleton formatting and tree-sitter helpers used by all language extractors.
//! `LanguageExtractor` trait defines the per-language hooks; `format_skeleton` groups entries
//! by `Section` (sorted by enum discriminant order, not source order) and renders them.
//! Imports get special treatment: same-root paths are consolidated (e.g. two `std::` uses merge).
//! `compress_line_ranges` turns `[1,2,3,10,11,20]` into `"1-3,10-11,20"` for compact output.

use std::fmt::Write;

use tree_sitter::Node;

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

#[cfg(feature = "lang-rust")]
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
    fn extract_node(&self, node: Node, source: &[u8], attrs: &[Node]) -> Option<SkeletonEntry>;
    fn is_test_node(&self, node: Node, source: &[u8], attrs: &[Node]) -> bool;
    fn is_doc_comment(&self, node: Node, source: &[u8]) -> bool;
    fn is_module_doc(&self, node: Node, source: &[u8]) -> bool;
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
) -> String {
    use std::collections::BTreeMap;

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
            format_imports(&mut out, items);
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
        let ranges = compress_line_ranges(test_lines);
        let sep = if out.is_empty() { "" } else { "\n" };
        let _ = writeln!(out, "{sep}tests: [{ranges}]");
    }

    out
}

fn format_imports(out: &mut String, entries: &[&SkeletonEntry]) {
    if entries.is_empty() {
        return;
    }

    let all_lines: Vec<usize> = entries
        .iter()
        .flat_map(|e| e.line_start..=e.line_end)
        .collect();

    let ranges = compress_line_ranges(&all_lines);
    let sep = if out.is_empty() { "" } else { "\n" };
    let _ = writeln!(out, "{sep}imports: [{ranges}]");

    let mut consolidated: Vec<(String, Vec<String>)> = Vec::new();
    for entry in entries {
        let text = &entry.text;
        let (root, parts) = match text.split_once("::") {
            Some((root, rest)) => {
                let rest = rest.trim();
                if rest.starts_with('{') && rest.ends_with('}') {
                    let inner = &rest[1..rest.len() - 1];
                    let items: Vec<String> = inner
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    (root.to_string(), items)
                } else {
                    (root.to_string(), vec![rest.to_string()])
                }
            }
            None => {
                consolidated.push((text.clone(), Vec::new()));
                continue;
            }
        };

        if let Some(existing) = consolidated.iter_mut().find(|(r, _)| *r == root) {
            existing.1.extend(parts);
        } else {
            consolidated.push((root, parts));
        }
    }

    for (root, parts) in &consolidated {
        if parts.is_empty() {
            let _ = writeln!(out, "  {root}");
        } else if parts.len() == 1 {
            let _ = writeln!(out, "  {root}::{}", parts[0]);
        } else {
            let _ = writeln!(out, "  {root}::{{{}}}", parts.join(", "));
        }
    }
}

pub(crate) fn compress_line_ranges(lines: &[usize]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut sorted = lines.to_vec();
    sorted.sort_unstable();
    sorted.dedup();

    let mut ranges = Vec::new();
    let mut start = sorted[0];
    let mut end = sorted[0];

    let push_range = |ranges: &mut Vec<String>, s: usize, e: usize| {
        ranges.push(if s == e {
            format!("{s}")
        } else {
            format!("{s}-{e}")
        });
    };

    for &line in &sorted[1..] {
        if line == end + 1 {
            end = line;
        } else {
            push_range(&mut ranges, start, end);
            start = line;
            end = line;
        }
    }
    push_range(&mut ranges, start, end);

    ranges.join(",")
}
