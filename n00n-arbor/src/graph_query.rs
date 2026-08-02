use std::collections::HashSet;
use std::path::Path;

use crate::graph_json::{GraphIndex, SymbolQuery, SymbolRef};
use crate::{ArborError, Client, MapEntry, MapSymbol, Relation, index_health};

fn line_number(line_start: usize) -> Option<u64> {
    // usize -> u64 cannot overflow on 64-bit targets; on other targets we
    // intentionally drop lines that do not fit rather than panic.
    #[allow(clippy::manual_ok_err)]
    match u64::try_from(line_start) {
        Ok(n) => Some(n),
        Err(_) => None,
    }
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

/// Load a graph index, refreshing via Arbor CLI when available.
///
/// When the Arbor binary cannot be spawned (`ArborError::Exec`), fall back to
/// reading `.arbor/graph.json` directly so in-memory queries remain usable in
/// CLI-free environments and unit tests.
fn load_query_index(project: &Path) -> Result<GraphIndex, ArborError> {
    match Client::load_graph_index(project) {
        Ok(index) => Ok(index),
        Err(ArborError::Exec { .. }) => {
            let graph_path = index_health::graph_json_path(project);
            GraphIndex::from_graph_json_path(&graph_path)
        }
        Err(err) => Err(err),
    }
}

pub fn graph_callers(symbol: &str, project: &Path) -> Result<Vec<Relation>, ArborError> {
    let index = load_query_index(project)?;
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
    let index = load_query_index(project)?;
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
    let index = load_query_index(project)?;
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
    index_health::graph_index_available(project)
}

// T067: Native map implementation using graph.json
#[allow(clippy::cast_precision_loss, clippy::similar_names)]
pub fn graph_map(project: &Path, token_budget: Option<u64>) -> Result<Vec<MapEntry>, ArborError> {
    let index = load_query_index(project)?;
    let mut file_map: std::collections::HashMap<String, Vec<MapSymbol>> =
        std::collections::HashMap::new();

    for (idx, node) in index.nodes().iter().enumerate() {
        let num_callers = index.find_callers(idx).len();
        let num_callees = index.find_callees(idx).len();
        let centrality = if num_callers + num_callees > 0 {
            Some((num_callers as f64 + num_callees as f64) / (index.nodes().len() as f64))
        } else {
            None
        };

        let symbol = MapSymbol {
            name: node.name.clone(),
            kind: node.kind.clone(),
            line: node.line_start as u64,
            centrality,
            callers: Some(num_callers as u64),
            is_entry_point: Some(num_callers == 0 && num_callees > 0),
        };

        file_map.entry(node.file.clone()).or_default().push(symbol);
    }

    let mut entries: Vec<MapEntry> = file_map
        .into_iter()
        .map(|(file, symbols)| MapEntry { file, symbols })
        .collect();

    // Sort by centrality and apply token budget
    entries.sort_by(|a, b| {
        let max_a = a
            .symbols
            .iter()
            .filter_map(|s| s.centrality)
            .fold(0.0_f64, f64::max);
        let max_b = b
            .symbols
            .iter()
            .filter_map(|s| s.centrality)
            .fold(0.0_f64, f64::max);
        max_b.total_cmp(&max_a)
    });

    if let Some(budget) = token_budget {
        let mut total_tokens = 0u64;
        let mut filtered = Vec::new();
        for entry in entries {
            let entry_tokens = estimate_entry_tokens(&entry);
            if total_tokens + entry_tokens <= budget {
                total_tokens += entry_tokens;
                filtered.push(entry);
            }
        }
        entries = filtered;
    }

    Ok(entries)
}

// T067: Native entry_points implementation using graph.json
pub fn graph_entry_points(project: &Path) -> Result<Vec<Relation>, ArborError> {
    let index = load_query_index(project)?;
    let mut entries = Vec::new();

    for (idx, node) in index.nodes().iter().enumerate() {
        let caller_nodes = index.find_callers(idx);
        if caller_nodes.is_empty() {
            entries.push(Relation {
                name: node.name.clone(),
                path: node.file.clone(),
                kind: Some(node.kind.clone()),
                line: line_number(node.line_start),
            });
        }
    }

    Ok(entries)
}

fn estimate_entry_tokens(entry: &MapEntry) -> u64 {
    let file_tokens = entry.file.len() as u64;
    let symbol_tokens: u64 = entry
        .symbols
        .iter()
        .map(|s| {
            s.name.len() as u64 + s.kind.len() as u64 + 20 // overhead for centrality, callers, etc.
        })
        .sum();
    file_tokens + symbol_tokens + 10 // overhead per entry
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
