use tree_sitter::Node;

use crate::common::{
    FIELD_TRUNCATE_THRESHOLD, LanguageExtractor, Section, SkeletonEntry, find_child, line_range,
    node_text, truncate,
};

pub(crate) struct GoExtractor;

impl GoExtractor {
    fn extract_import(&self, node: Node, source: &[u8]) -> Vec<SkeletonEntry> {
        let mut entries = Vec::new();
        if let Some(spec_list) = find_child(node, "import_spec_list") {
            let mut cursor = spec_list.walk();
            for child in spec_list.children(&mut cursor) {
                if child.kind() == "import_spec" {
                    let path = self.import_path(child, source);
                    entries.push(SkeletonEntry::new(Section::Import, child, path));
                }
            }
        } else if let Some(spec) = find_child(node, "import_spec") {
            let path = self.import_path(spec, source);
            entries.push(SkeletonEntry::new(Section::Import, node, path));
        }
        entries
    }

    fn import_path(&self, spec: Node, source: &[u8]) -> String {
        spec.child_by_field_name("path")
            .map(|n| node_text(n, source).trim_matches('"').to_string())
            .unwrap_or_else(|| node_text(spec, source).trim_matches('"').to_string())
    }

    fn params_result(&self, node: Node, source: &[u8]) -> String {
        let params = node
            .child_by_field_name("parameters")
            .map(|n| node_text(n, source))
            .unwrap_or("()");
        let result = node
            .child_by_field_name("result")
            .map(|n| format!(" {}", node_text(n, source)))
            .unwrap_or_default();
        format!("{params}{result}")
    }

    fn extract_function(&self, node: Node, source: &[u8]) -> Option<SkeletonEntry> {
        let name = node
            .child_by_field_name("name")
            .map(|n| node_text(n, source))?;
        let sig = self.params_result(node, source);
        Some(SkeletonEntry::new(
            Section::Function,
            node,
            format!("{name}{sig}"),
        ))
    }

    fn extract_method(&self, node: Node, source: &[u8]) -> Option<SkeletonEntry> {
        let receiver = node
            .child_by_field_name("receiver")
            .map(|n| node_text(n, source))?;
        let name = node
            .child_by_field_name("name")
            .map(|n| node_text(n, source))?;
        let sig = self.params_result(node, source);
        Some(SkeletonEntry::new(
            Section::Impl,
            node,
            format!("{receiver} {name}{sig}"),
        ))
    }

    fn extract_type_declaration(&self, node: Node, source: &[u8]) -> Vec<SkeletonEntry> {
        let mut cursor = node.walk();
        node.children(&mut cursor)
            .filter_map(|child| match child.kind() {
                "type_spec" => self.extract_type_spec(child, source),
                "type_alias" => self.extract_type_alias(child, source),
                _ => None,
            })
            .collect()
    }

    fn extract_type_spec(&self, node: Node, source: &[u8]) -> Option<SkeletonEntry> {
        let name = node
            .child_by_field_name("name")
            .map(|n| node_text(n, source))?;
        let type_node = node.child_by_field_name("type")?;

        match type_node.kind() {
            "struct_type" => {
                let children = self.extract_struct_fields(type_node, source);
                let mut entry = SkeletonEntry::new(Section::Type, node, format!("struct {name}"));
                entry.children = children;
                Some(entry)
            }
            "interface_type" => {
                let children = self.extract_interface_methods(type_node, source);
                let mut entry = SkeletonEntry::new(Section::Trait, node, name.to_string());
                entry.children = children;
                Some(entry)
            }
            _ => Some(SkeletonEntry::new(
                Section::Type,
                node,
                format!("type {name} {}", truncate(node_text(type_node, source), 60)),
            )),
        }
    }

    fn extract_type_alias(&self, node: Node, source: &[u8]) -> Option<SkeletonEntry> {
        let name = node
            .child_by_field_name("name")
            .map(|n| node_text(n, source))?;
        let ty = node
            .child_by_field_name("type")
            .map(|n| node_text(n, source))
            .unwrap_or("_");
        Some(SkeletonEntry::new(
            Section::Type,
            node,
            format!("type {name} = {ty}"),
        ))
    }

    fn extract_struct_fields(&self, node: Node, source: &[u8]) -> Vec<String> {
        let Some(field_list) = find_child(node, "field_declaration_list") else {
            return Vec::new();
        };
        let mut fields = Vec::new();
        let mut total = 0;
        let mut cursor = field_list.walk();
        for child in field_list.children(&mut cursor) {
            if child.kind() == "field_declaration" {
                total += 1;
                if total <= FIELD_TRUNCATE_THRESHOLD {
                    let text = node_text(child, source).trim().to_string();
                    fields.push(text);
                }
            }
        }
        if total > FIELD_TRUNCATE_THRESHOLD {
            fields.push("...".into());
        }
        fields
    }

    fn extract_interface_methods(&self, node: Node, source: &[u8]) -> Vec<String> {
        let mut methods = Vec::new();
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "method_elem" => {
                    let name = child
                        .child_by_field_name("name")
                        .map(|n| node_text(n, source))
                        .unwrap_or("_");
                    let sig = self.params_result(child, source);
                    let lr =
                        line_range(child.start_position().row + 1, child.end_position().row + 1);
                    methods.push(format!("{name}{sig} {lr}"));
                }
                "type_elem" => {
                    let text = node_text(child, source).trim().to_string();
                    let lr =
                        line_range(child.start_position().row + 1, child.end_position().row + 1);
                    methods.push(format!("{text} {lr}"));
                }
                _ => {}
            }
        }
        methods
    }

    fn extract_const_var(&self, node: Node, source: &[u8]) -> Vec<SkeletonEntry> {
        let mut entries = Vec::new();
        let is_var = node.kind() == "var_declaration";
        let spec_kind = if is_var { "var_spec" } else { "const_spec" };
        let list_kind = if is_var {
            "var_spec_list"
        } else {
            "const_spec_list"
        };

        let specs: Vec<Node> = if let Some(list) = find_child(node, list_kind) {
            let mut cursor = list.walk();
            list.children(&mut cursor)
                .filter(|c| c.kind() == spec_kind)
                .collect()
        } else {
            let mut cursor = node.walk();
            node.children(&mut cursor)
                .filter(|c| c.kind() == spec_kind)
                .collect()
        };

        for spec in specs {
            let name = spec
                .child_by_field_name("name")
                .map(|n| node_text(n, source));
            if let Some(name) = name {
                let ty = spec
                    .child_by_field_name("type")
                    .map(|n| format!(" {}", node_text(n, source)))
                    .unwrap_or_default();
                let prefix = if is_var { "var " } else { "" };
                entries.push(SkeletonEntry::new(
                    Section::Constant,
                    spec,
                    format!("{prefix}{name}{ty}"),
                ));
            }
        }
        entries
    }
}

impl LanguageExtractor for GoExtractor {
    fn extract_nodes(&self, node: Node, source: &[u8], _attrs: &[Node]) -> Vec<SkeletonEntry> {
        match node.kind() {
            "import_declaration" => self.extract_import(node, source),
            "type_declaration" => self.extract_type_declaration(node, source),
            "const_declaration" | "var_declaration" => self.extract_const_var(node, source),
            "function_declaration" => self.extract_function(node, source).into_iter().collect(),
            "method_declaration" => self.extract_method(node, source).into_iter().collect(),
            _ => Vec::new(),
        }
    }

    fn is_test_node(&self, _node: Node, _source: &[u8], _attrs: &[Node]) -> bool {
        false
    }

    fn is_doc_comment(&self, node: Node, _source: &[u8]) -> bool {
        node.kind() == "comment"
    }

    fn import_separator(&self) -> &'static str {
        "/"
    }

    fn is_module_doc(&self, _node: Node, _source: &[u8]) -> bool {
        false
    }
}
