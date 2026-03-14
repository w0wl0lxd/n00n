//! TypeScript and JavaScript extractor (shared `TsJsExtractor` handles both).
//! Export visibility is detected by checking if the parent node is `export_statement`.
//! Interfaces expand their property/method signatures as children.
//! JSDoc comments (`/** ... */`) extend the line range of the next declaration.
//! No test detection for the same reasons as Python.

use tree_sitter::Node;

use crate::common::{
    LanguageExtractor, Section, SkeletonEntry, find_child, line_range, node_text, truncate,
};

fn ts_return_type(node: Node, source: &[u8]) -> String {
    let r = node_text(node, source);
    if r.starts_with(':') {
        r.to_string()
    } else {
        format!(": {r}")
    }
}

pub(crate) struct TsJsExtractor;

impl TsJsExtractor {
    fn extract_import(&self, node: Node, source: &[u8]) -> Option<SkeletonEntry> {
        let text = node_text(node, source);
        let cleaned = text
            .strip_prefix("import ")
            .unwrap_or(text)
            .trim_end_matches(';')
            .to_string();
        Some(SkeletonEntry::new(Section::Import, node, cleaned))
    }

    fn export_prefix(&self, node: Node) -> &'static str {
        if self.is_exported(node) {
            "export "
        } else {
            ""
        }
    }

    fn extract_class(&self, node: Node, source: &[u8]) -> Option<SkeletonEntry> {
        let name = node
            .child_by_field_name("name")
            .map(|n| node_text(n, source))?;

        let body = node.child_by_field_name("body")?;
        let mut methods = Vec::new();
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            match child.kind() {
                "method_definition" | "public_field_definition" => {
                    let mn = child
                        .child_by_field_name("name")
                        .map(|n| node_text(n, source))
                        .unwrap_or("_");
                    let params = child
                        .child_by_field_name("parameters")
                        .map(|n| node_text(n, source));
                    let ret = child
                        .child_by_field_name("return_type")
                        .map(|n| ts_return_type(n, source));
                    let params_str = params.unwrap_or_default();
                    let ret_str = ret.unwrap_or_default();
                    let lr =
                        line_range(child.start_position().row + 1, child.end_position().row + 1);
                    methods.push(format!("{mn}{params_str}{ret_str} {lr}"));
                }
                _ => {}
            }
        }

        let ep = self.export_prefix(node);
        let mut entry = SkeletonEntry::new(Section::Class, node, format!("{ep}{name}"));
        entry.children = methods;
        Some(entry)
    }

    fn extract_function(&self, node: Node, source: &[u8]) -> Option<SkeletonEntry> {
        let name = node
            .child_by_field_name("name")
            .map(|n| node_text(n, source))?;
        let params = node
            .child_by_field_name("parameters")
            .map(|n| node_text(n, source))
            .unwrap_or("()");
        let ret_str = node
            .child_by_field_name("return_type")
            .map(|n| ts_return_type(n, source))
            .unwrap_or_default();

        let ep = self.export_prefix(node);
        Some(SkeletonEntry::new(
            Section::Function,
            node,
            format!("{ep}{name}{params}{ret_str}"),
        ))
    }

    fn extract_interface(&self, node: Node, source: &[u8]) -> Option<SkeletonEntry> {
        let name = node
            .child_by_field_name("name")
            .map(|n| node_text(n, source))?;
        let body = node.child_by_field_name("body")?;

        let mut fields = Vec::new();
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            if child.kind() == "property_signature" || child.kind() == "method_signature" {
                let text = node_text(child, source)
                    .trim_end_matches([',', ';'])
                    .to_string();
                fields.push(text);
            }
        }

        let ep = self.export_prefix(node);
        let mut entry = SkeletonEntry::new(Section::Type, node, format!("{ep}interface {name}"));
        entry.children = fields;
        Some(entry)
    }

    fn extract_type_alias(&self, node: Node, source: &[u8]) -> Option<SkeletonEntry> {
        let name = node
            .child_by_field_name("name")
            .map(|n| node_text(n, source))?;
        let val_str = node
            .child_by_field_name("value")
            .map(|n| format!(" = {}", truncate(node_text(n, source), 80)))
            .unwrap_or_default();
        let ep = self.export_prefix(node);
        Some(SkeletonEntry::new(
            Section::Type,
            node,
            format!("{ep}type {name}{val_str}"),
        ))
    }

    fn extract_const(&self, node: Node, source: &[u8]) -> Option<SkeletonEntry> {
        let decl = find_child(node, "variable_declarator")?;
        let name = decl
            .child_by_field_name("name")
            .map(|n| node_text(n, source))?;
        let type_str = decl
            .child_by_field_name("type")
            .map(|n| ts_return_type(n, source))
            .unwrap_or_default();
        let val_str = decl
            .child_by_field_name("value")
            .map(|n| format!(" = {}", truncate(node_text(n, source), 60)))
            .unwrap_or_default();
        let ep = self.export_prefix(node);
        Some(SkeletonEntry::new(
            Section::Constant,
            node,
            format!("{ep}{name}{type_str}{val_str}"),
        ))
    }

    fn extract_enum(&self, node: Node, source: &[u8]) -> Option<SkeletonEntry> {
        let name = node
            .child_by_field_name("name")
            .map(|n| node_text(n, source))?;
        let ep = self.export_prefix(node);
        Some(SkeletonEntry::new(
            Section::Type,
            node,
            format!("{ep}enum {name}"),
        ))
    }

    fn is_exported(&self, node: Node) -> bool {
        node.parent()
            .is_some_and(|p| p.kind() == "export_statement")
    }

    fn extract_export_statement(&self, node: Node, source: &[u8]) -> Option<SkeletonEntry> {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "class_declaration" => return self.extract_class(child, source),
                "function_declaration" => return self.extract_function(child, source),
                "interface_declaration" => return self.extract_interface(child, source),
                "type_alias_declaration" => return self.extract_type_alias(child, source),
                "lexical_declaration" => return self.extract_lexical_declaration(child, source),
                "enum_declaration" => return self.extract_enum(child, source),
                _ => {}
            }
        }
        None
    }

    fn extract_lexical_declaration(&self, node: Node, source: &[u8]) -> Option<SkeletonEntry> {
        let kind_text = node.child(0).map(|n| node_text(n, source)).unwrap_or("");
        if kind_text == "const" {
            self.extract_const(node, source)
        } else {
            None
        }
    }
}

impl LanguageExtractor for TsJsExtractor {
    fn extract_node(&self, node: Node, source: &[u8], _attrs: &[Node]) -> Option<SkeletonEntry> {
        match node.kind() {
            "import_statement" => self.extract_import(node, source),
            "class_declaration" => self.extract_class(node, source),
            "function_declaration" => self.extract_function(node, source),
            "interface_declaration" => self.extract_interface(node, source),
            "type_alias_declaration" => self.extract_type_alias(node, source),
            "enum_declaration" => self.extract_enum(node, source),
            "lexical_declaration" => self.extract_lexical_declaration(node, source),
            "export_statement" => self.extract_export_statement(node, source),
            _ => None,
        }
    }

    fn is_test_node(&self, _node: Node, _source: &[u8], _attrs: &[Node]) -> bool {
        false
    }

    fn is_doc_comment(&self, node: Node, source: &[u8]) -> bool {
        node.kind() == "comment" && node_text(node, source).starts_with("/**")
    }

    fn is_module_doc(&self, _node: Node, _source: &[u8]) -> bool {
        false
    }
}
