use tree_sitter::Node;

use crate::common::{
    ChildKind, FIELD_TRUNCATE_THRESHOLD, LanguageExtractor, Section, SkeletonEntry, compact_ws,
    find_child, line_range, node_text, prefixed,
};

pub(crate) struct CSharpExtractor;

const MODIFIER_KEYWORDS: &[&str] = &[
    "public",
    "private",
    "protected",
    "internal",
    "static",
    "abstract",
    "sealed",
    "override",
    "virtual",
    "async",
    "readonly",
    "extern",
    "partial",
    "new",
    "unsafe",
    "volatile",
];

impl CSharpExtractor {
    fn modifiers_text(&self, node: Node, source: &[u8]) -> String {
        let mut parts = Vec::new();
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            let text = node_text(child, source);
            let include = match child.kind() {
                "modifier" => MODIFIER_KEYWORDS.contains(&text),
                "attribute_list" => true,
                _ => false,
            };
            if include {
                parts.push(text.to_string());
            }
        }
        parts.join(" ")
    }

    fn base_list_text(&self, node: Node, source: &[u8]) -> String {
        let Some(bl) = find_child(node, "base_list") else {
            return String::new();
        };
        let text = node_text(bl, source);
        let trimmed = text.trim_start_matches(':').trim();
        format!(" : {trimmed}")
    }

    fn extract_import(&self, node: Node, source: &[u8]) -> Option<SkeletonEntry> {
        let text = node_text(node, source);
        let cleaned = text
            .strip_prefix("using ")
            .unwrap_or(text)
            .trim_end_matches(';')
            .trim();
        let paths = vec![cleaned.split('.').map(String::from).collect()];
        Some(SkeletonEntry::new_import(node, paths))
    }

    fn extract_namespace(&self, node: Node, source: &[u8]) -> Option<SkeletonEntry> {
        let name = node
            .child_by_field_name("name")
            .map(|n| node_text(n, source))?;
        Some(SkeletonEntry::new(Section::Module, node, name.to_string()))
    }

    fn extract_class(&self, node: Node, source: &[u8]) -> Option<SkeletonEntry> {
        let mods = self.modifiers_text(node, source);
        let name = node
            .child_by_field_name("name")
            .map(|n| node_text(n, source))?;
        let bases = self.base_list_text(node, source);
        let label = prefixed(&mods, format_args!("class {name}{bases}"));
        let children = self.extract_declaration_list(node, source);
        Some(SkeletonEntry::new(Section::Class, node, label).with_children(children))
    }

    fn extract_struct(&self, node: Node, source: &[u8]) -> Option<SkeletonEntry> {
        let mods = self.modifiers_text(node, source);
        let name = node
            .child_by_field_name("name")
            .map(|n| node_text(n, source))?;
        let bases = self.base_list_text(node, source);
        let label = prefixed(&mods, format_args!("struct {name}{bases}"));
        let children = self.extract_declaration_list(node, source);
        Some(SkeletonEntry::new(Section::Type, node, label).with_children(children))
    }

    fn extract_interface(&self, node: Node, source: &[u8]) -> Option<SkeletonEntry> {
        let mods = self.modifiers_text(node, source);
        let name = node
            .child_by_field_name("name")
            .map(|n| node_text(n, source))?;
        let bases = self.base_list_text(node, source);
        let label = prefixed(&mods, format_args!("interface {name}{bases}"));
        let children = self.extract_interface_body(node, source);
        Some(SkeletonEntry::new(Section::Trait, node, label).with_children(children))
    }

    fn extract_record(&self, node: Node, source: &[u8]) -> Option<SkeletonEntry> {
        let mods = self.modifiers_text(node, source);
        let name = node
            .child_by_field_name("name")
            .map(|n| node_text(n, source))?;
        let params = find_child(node, "parameter_list")
            .map(|n| node_text(n, source))
            .unwrap_or("");
        let bases = self.base_list_text(node, source);
        let label = prefixed(&mods, format_args!("record {name}{params}{bases}"));
        Some(SkeletonEntry::new(Section::Type, node, label))
    }

    fn extract_enum(&self, node: Node, source: &[u8]) -> Option<SkeletonEntry> {
        let mods = self.modifiers_text(node, source);
        let name = node
            .child_by_field_name("name")
            .map(|n| node_text(n, source))?;
        let label = prefixed(&mods, format_args!("enum {name}"));
        let body = node.child_by_field_name("body")?;
        let mut constants = Vec::new();
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            if child.kind() == "enum_member_declaration" {
                let cname = child
                    .child_by_field_name("name")
                    .map(|n| node_text(n, source))
                    .unwrap_or("_");
                constants.push(cname.to_string());
            }
        }
        Some(
            SkeletonEntry::new(Section::Type, node, label)
                .with_children(constants)
                .with_child_kind(ChildKind::Brief),
        )
    }

    fn extract_declaration_list(&self, node: Node, source: &[u8]) -> Vec<String> {
        let Some(body) = node.child_by_field_name("body") else {
            return Vec::new();
        };
        let mut members = Vec::new();
        let mut field_count = 0usize;
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            match child.kind() {
                "method_declaration" | "constructor_declaration" => {
                    let sig = self.method_signature(child, source);
                    let lr =
                        line_range(child.start_position().row + 1, child.end_position().row + 1);
                    members.push(format!("{sig} {lr}"));
                }
                "field_declaration" => {
                    field_count += 1;
                    if field_count <= FIELD_TRUNCATE_THRESHOLD {
                        let text = self.field_text(child, source);
                        let lr = line_range(
                            child.start_position().row + 1,
                            child.end_position().row + 1,
                        );
                        members.push(format!("{text} {lr}"));
                    }
                }
                "property_declaration" => {
                    field_count += 1;
                    if field_count <= FIELD_TRUNCATE_THRESHOLD {
                        let text = self.property_text(child, source);
                        let lr = line_range(
                            child.start_position().row + 1,
                            child.end_position().row + 1,
                        );
                        members.push(format!("{text} {lr}"));
                    }
                }
                _ => {}
            }
        }
        if field_count > FIELD_TRUNCATE_THRESHOLD {
            members.push("...".into());
        }
        members
    }

    fn extract_interface_body(&self, node: Node, source: &[u8]) -> Vec<String> {
        let Some(body) = node.child_by_field_name("body") else {
            return Vec::new();
        };
        let mut members = Vec::new();
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            let text = match child.kind() {
                "method_declaration" => self.method_signature(child, source),
                "property_declaration" => self.property_text(child, source),
                _ => continue,
            };
            let lr = line_range(child.start_position().row + 1, child.end_position().row + 1);
            members.push(format!("{text} {lr}"));
        }
        members
    }

    fn method_signature(&self, node: Node, source: &[u8]) -> String {
        let mods = self.modifiers_text(node, source);
        let ret = node
            .child_by_field_name("returns")
            .or_else(|| node.child_by_field_name("type"))
            .map(|n| node_text(n, source))
            .unwrap_or("");
        let name = node
            .child_by_field_name("name")
            .map(|n| node_text(n, source))
            .unwrap_or("_");
        let params = node
            .child_by_field_name("parameters")
            .map(|n| node_text(n, source))
            .unwrap_or("()");
        let base = if ret.is_empty() {
            format!("{name}{params}")
        } else {
            format!("{ret} {name}{params}")
        };
        compact_ws(&prefixed(&mods, format_args!("{base}")))
    }

    fn field_text(&self, node: Node, source: &[u8]) -> String {
        let mods = self.modifiers_text(node, source);
        let decl = find_child(node, "variable_declaration");
        let ty = decl
            .and_then(|n| n.child_by_field_name("type"))
            .map(|n| node_text(n, source))
            .unwrap_or("_");
        let name = decl
            .and_then(|d| find_child(d, "variable_declarator"))
            .and_then(|n| n.child_by_field_name("name"))
            .map(|n| node_text(n, source))
            .unwrap_or("_");
        prefixed(&mods, format_args!("{ty} {name}"))
    }

    fn property_text(&self, node: Node, source: &[u8]) -> String {
        let mods = self.modifiers_text(node, source);
        let ty = node
            .child_by_field_name("type")
            .map(|n| node_text(n, source))
            .unwrap_or("_");
        let name = node
            .child_by_field_name("name")
            .map(|n| node_text(n, source))
            .unwrap_or("_");
        prefixed(&mods, format_args!("{ty} {name}"))
    }
}

impl LanguageExtractor for CSharpExtractor {
    fn extract_nodes(&self, node: Node, source: &[u8], _attrs: &[Node]) -> Vec<SkeletonEntry> {
        match node.kind() {
            "using_directive" => self.extract_import(node, source).into_iter().collect(),
            "namespace_declaration" | "file_scoped_namespace_declaration" => {
                self.extract_namespace(node, source).into_iter().collect()
            }
            "class_declaration" => self.extract_class(node, source).into_iter().collect(),
            "struct_declaration" => self.extract_struct(node, source).into_iter().collect(),
            "interface_declaration" => self.extract_interface(node, source).into_iter().collect(),
            "enum_declaration" => self.extract_enum(node, source).into_iter().collect(),
            "record_declaration" => self.extract_record(node, source).into_iter().collect(),
            _ => Vec::new(),
        }
    }

    fn is_doc_comment(&self, node: Node, source: &[u8]) -> bool {
        node.kind() == "single_line_doc_comment"
            || (node.kind() == "comment" && node_text(node, source).starts_with("///"))
    }

    fn import_separator(&self) -> &'static str {
        "."
    }
}
