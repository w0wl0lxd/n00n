use std::collections::HashSet;
use std::path::Path;

use crate::graph_json::{GraphIndex, SymbolQuery, SymbolRef};
use crate::{ArborError, Relation};

fn line_number(line_start: usize) -> Option<u64> {
    u64::try_from(line_start).ok()
}

fn symbol_to_relation(symbol: &SymbolRef) -> Relation {
    Relation {
        name: symbol.node.name.clone(),
        path: symbol.node.file.clone(),
        kind: Some(symbol.node.kind.clone()),
        line: line_number(symbol.node.line_start),
    }
}

fn dedupe_relations(relations: Vec<Relation>) -> Vec<Relation> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for relation in relations {
        let key = (relation.name.clone(), relation.path.clone(), relation.line);
        if seen.insert(key) {
            out.push(relation);
        }
    }
    out
}

pub fn graph_callers(symbol: &str, project: &Path) -> Result<Vec<Relation>, ArborError> {
    let index = crate::Client::load_graph_index(project)?;
    let query = SymbolQuery {
        name: symbol.to_string(),
        ..SymbolQuery::default()
    };
    Ok(graph_relations_for_matches(
        &index,
        &query,
        NeighborDirection::Callers,
    ))
}

pub fn graph_callees(symbol: &str, project: &Path) -> Result<Vec<Relation>, ArborError> {
    let index = crate::Client::load_graph_index(project)?;
    let query = SymbolQuery {
        name: symbol.to_string(),
        ..SymbolQuery::default()
    };
    Ok(graph_relations_for_matches(
        &index,
        &query,
        NeighborDirection::Callees,
    ))
}

pub fn graph_trace_path(from: &str, to: &str, project: &Path) -> Result<Vec<Relation>, ArborError> {
    let index = crate::Client::load_graph_index(project)?;
    let from_query = SymbolQuery {
        name: from.to_string(),
        ..SymbolQuery::default()
    };
    let to_query = SymbolQuery {
        name: to.to_string(),
        ..SymbolQuery::default()
    };
    let path = index.trace_path_symbols(&from_query, &to_query)?;
    Ok(path.iter().map(symbol_to_relation).collect())
}

#[derive(Copy, Clone)]
enum NeighborDirection {
    Callers,
    Callees,
}

fn graph_relations_for_matches(
    index: &GraphIndex,
    query: &SymbolQuery,
    direction: NeighborDirection,
) -> Vec<Relation> {
    let mut relations = Vec::new();
    for matched in index.resolve_symbol(query) {
        let neighbors = match direction {
            NeighborDirection::Callers => index.find_callers(matched.index),
            NeighborDirection::Callees => index.find_callees(matched.index),
        };
        for neighbor in neighbors {
            relations.push(symbol_to_relation(&neighbor));
        }
    }
    dedupe_relations(relations)
}

pub fn graph_index_available(project: &Path) -> bool {
    crate::index_health::graph_index_available(project)
}

#[cfg(test)]
mod tests {
    use super::{graph_callers, graph_trace_path};
    use crate::graph_json::GraphIndex;

    const SAMPLE_GRAPH: &str = r#"{
      "file_index": { "src/main.rs": [0, 1], "src/lib.rs": [2] },
      "id_index": { "sym-main": 0, "sym-helper": 1, "sym-lib": 2 },
      "name_index": { "main": [0], "helper": [1], "lib_fn": [2] },
      "graph": {
        "nodes": [
          {
            "id": "sym-main",
            "name": "main",
            "qualified_name": "crate::main",
            "kind": "function",
            "file": "src/main.rs",
            "line_start": 1,
            "line_end": 10
          },
          {
            "id": "sym-helper",
            "name": "helper",
            "qualified_name": "crate::helper",
            "kind": "function",
            "file": "src/main.rs",
            "line_start": 12,
            "line_end": 20
          },
          {
            "id": "sym-lib",
            "name": "lib_fn",
            "qualified_name": "crate::lib::lib_fn",
            "kind": "function",
            "file": "src/lib.rs",
            "line_start": 2,
            "line_end": 5
          }
        ],
        "edges": [
          [0, 1, { "kind": "calls" }],
          [1, 2, { "kind": "calls" }]
        ]
      }
    }"#;

    #[test]
    fn graph_callers_reads_in_memory_index() {
        let dir = tempfile::tempdir().expect("tempdir");
        let arbor_dir = dir.path().join(".arbor");
        std::fs::create_dir_all(&arbor_dir).expect("arbor dir");
        std::fs::write(arbor_dir.join("graph.json"), SAMPLE_GRAPH).expect("graph");
        let index = GraphIndex::from_graph_json_path(&arbor_dir.join("graph.json"))
            .expect("graph should parse");
        assert_eq!(index.find_symbol("helper").len(), 1);
        let callers = graph_callers("helper", dir.path()).expect("callers");
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].name, "main");
    }

    #[test]
    fn graph_trace_path_reads_in_memory_index() {
        let dir = tempfile::tempdir().expect("tempdir");
        let arbor_dir = dir.path().join(".arbor");
        std::fs::create_dir_all(&arbor_dir).expect("arbor dir");
        std::fs::write(arbor_dir.join("graph.json"), SAMPLE_GRAPH).expect("graph");
        let path = graph_trace_path("main", "lib_fn", dir.path()).expect("path");
        let names: Vec<_> = path.into_iter().map(|relation| relation.name).collect();
        assert_eq!(names, vec!["main", "helper", "lib_fn"]);
    }
}
