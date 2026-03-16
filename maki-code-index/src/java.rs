use tree_sitter::Node;

use crate::common::{
    ChildKind, FIELD_TRUNCATE_THRESHOLD, LanguageExtractor, Section, SkeletonEntry, compact_ws,
    find_child, line_range, node_text, prefixed,
};

pub(crate) struct JavaExtractor;

impl JavaExtractor {
    fn type_list_text(&self, parent: Node, source: &[u8]) -> String {
        let Some(tl) = find_child(parent, "type_list") else {
            return node_text(parent, source)
                .trim_start_matches("extends")
                .trim_start_matches("implements")
                .trim()
                .to_string();
        };
        node_text(tl, source).to_string()
    }

    fn implements_clause(&self, node: Node, source: &[u8]) -> String {
        node.child_by_field_name("interfaces")
            .map(|n| format!(" implements {}", self.type_list_text(n, source)))
            .unwrap_or_default()
    }

    fn extract_import(&self, node: Node, source: &[u8]) -> Option<SkeletonEntry> {
        let text = node_text(node, source);
        let cleaned = text
            .strip_prefix("import ")
            .unwrap_or(text)
            .trim_end_matches(';')
            .trim();
        Some(SkeletonEntry::new(
            Section::Import,
            node,
            cleaned.to_string(),
        ))
    }

    fn extract_package(&self, node: Node, source: &[u8]) -> Option<SkeletonEntry> {
        let text = node_text(node, source);
        let cleaned = text
            .strip_prefix("package ")
            .unwrap_or(text)
            .trim_end_matches(';')
            .trim()
            .to_string();
        Some(SkeletonEntry::new(Section::Module, node, cleaned))
    }

    fn modifiers_text(&self, node: Node, source: &[u8]) -> String {
        let Some(mods) = find_child(node, "modifiers") else {
            return String::new();
        };
        let mut annotations = Vec::new();
        let mut keywords = Vec::new();
        let mut cursor = mods.walk();
        for child in mods.children(&mut cursor) {
            match child.kind() {
                "marker_annotation" | "annotation" => {
                    annotations.push(node_text(child, source));
                }
                _ => {
                    let text = node_text(child, source);
                    if matches!(
                        text,
                        "public"
                            | "private"
                            | "protected"
                            | "static"
                            | "final"
                            | "abstract"
                            | "default"
                            | "synchronized"
                    ) {
                        keywords.push(text);
                    }
                }
            }
        }
        annotations.extend(keywords);
        annotations.join(" ")
    }

    fn extract_class(&self, node: Node, source: &[u8]) -> Option<SkeletonEntry> {
        let mods = self.modifiers_text(node, source);
        let name = node
            .child_by_field_name("name")
            .map(|n| node_text(n, source))?;
        let superclass = node
            .child_by_field_name("superclass")
            .and_then(|n| find_child(n, "type_identifier").or(Some(n)))
            .map(|n| format!(" extends {}", node_text(n, source)))
            .unwrap_or_default();
        let interfaces = self.implements_clause(node, source);

        let label = prefixed(&mods, format_args!("class {name}{superclass}{interfaces}"));

        let children = self.extract_class_body(node, source);
        let mut entry = SkeletonEntry::new(Section::Class, node, label);
        entry.children = children;
        Some(entry)
    }

    fn extract_class_body(&self, node: Node, source: &[u8]) -> Vec<String> {
        let Some(body) = node.child_by_field_name("body") else {
            return Vec::new();
        };
        let mut members = Vec::new();
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
                    if members.len() < FIELD_TRUNCATE_THRESHOLD {
                        let text = self.field_text(child, source);
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
        members
    }

    fn method_signature(&self, node: Node, source: &[u8]) -> String {
        let mods = self.modifiers_text(node, source);
        let ret = node
            .child_by_field_name("type")
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
        let ty = node
            .child_by_field_name("type")
            .map(|n| node_text(n, source))
            .unwrap_or("_");
        let name = find_child(node, "variable_declarator")
            .and_then(|n| n.child_by_field_name("name"))
            .map(|n| node_text(n, source))
            .unwrap_or("_");
        prefixed(&mods, format_args!("{ty} {name}"))
    }

    fn extract_interface(&self, node: Node, source: &[u8]) -> Option<SkeletonEntry> {
        let mods = self.modifiers_text(node, source);
        let name = node
            .child_by_field_name("name")
            .map(|n| node_text(n, source))?;
        let extends = find_child(node, "extends_interfaces")
            .map(|n| format!(" extends {}", self.type_list_text(n, source)))
            .unwrap_or_default();

        let label = prefixed(&mods, format_args!("interface {name}{extends}"));

        let children = self.extract_interface_body(node, source);
        let mut entry = SkeletonEntry::new(Section::Trait, node, label);
        entry.children = children;
        Some(entry)
    }

    fn extract_interface_body(&self, node: Node, source: &[u8]) -> Vec<String> {
        let Some(body) = node.child_by_field_name("body") else {
            return Vec::new();
        };
        let mut members = Vec::new();
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            if child.kind() == "method_declaration" || child.kind() == "constant_declaration" {
                let sig = self.method_signature(child, source);
                let lr = line_range(child.start_position().row + 1, child.end_position().row + 1);
                members.push(format!("{sig} {lr}"));
            }
        }
        members
    }

    fn extract_enum(&self, node: Node, source: &[u8]) -> Option<SkeletonEntry> {
        let mods = self.modifiers_text(node, source);
        let name = node
            .child_by_field_name("name")
            .map(|n| node_text(n, source))?;

        let interfaces = self.implements_clause(node, source);
        let label = prefixed(&mods, format_args!("enum {name}{interfaces}"));

        let body = node.child_by_field_name("body")?;
        let mut constants = Vec::new();
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            if child.kind() == "enum_constant" {
                let cname = child
                    .child_by_field_name("name")
                    .map(|n| node_text(n, source))
                    .unwrap_or("_");
                constants.push(cname.to_string());
            }
        }

        let mut entry = SkeletonEntry::new(Section::Type, node, label);
        entry.children = constants;
        entry.child_kind = ChildKind::Brief;
        Some(entry)
    }

    fn extract_record(&self, node: Node, source: &[u8]) -> Option<SkeletonEntry> {
        let mods = self.modifiers_text(node, source);
        let name = node
            .child_by_field_name("name")
            .map(|n| node_text(n, source))?;
        let params = find_child(node, "formal_parameters")
            .map(|n| node_text(n, source))
            .unwrap_or("()");

        let interfaces = self.implements_clause(node, source);
        let label = prefixed(&mods, format_args!("record {name}{params}{interfaces}"));

        Some(SkeletonEntry::new(Section::Type, node, label))
    }

    fn extract_annotation_type(&self, node: Node, source: &[u8]) -> Option<SkeletonEntry> {
        let mods = self.modifiers_text(node, source);
        let name = node
            .child_by_field_name("name")
            .map(|n| node_text(n, source))?;
        let label = prefixed(&mods, format_args!("@interface {name}"));
        Some(SkeletonEntry::new(Section::Type, node, label))
    }
}

impl LanguageExtractor for JavaExtractor {
    fn extract_nodes(&self, node: Node, source: &[u8], _attrs: &[Node]) -> Vec<SkeletonEntry> {
        match node.kind() {
            "import_declaration" => self.extract_import(node, source).into_iter().collect(),
            "package_declaration" => self.extract_package(node, source).into_iter().collect(),
            "class_declaration" => self.extract_class(node, source).into_iter().collect(),
            "interface_declaration" => self.extract_interface(node, source).into_iter().collect(),
            "enum_declaration" => self.extract_enum(node, source).into_iter().collect(),
            "record_declaration" => self.extract_record(node, source).into_iter().collect(),
            "annotation_type_declaration" => self
                .extract_annotation_type(node, source)
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    }

    fn is_test_node(&self, _node: Node, _source: &[u8], _attrs: &[Node]) -> bool {
        false
    }

    fn is_doc_comment(&self, node: Node, source: &[u8]) -> bool {
        node.kind() == "block_comment" && node_text(node, source).starts_with("/**")
    }

    fn import_separator(&self) -> &'static str {
        "."
    }

    fn is_module_doc(&self, _node: Node, _source: &[u8]) -> bool {
        false
    }
}
