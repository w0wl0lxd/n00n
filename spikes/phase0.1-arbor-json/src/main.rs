use color_eyre::eyre::{Result, WrapErr, eyre};
use petgraph::graph::{DiGraph, NodeIndex};
use serde::{Deserialize, Deserializer};
use std::collections::HashMap;
use std::convert::TryFrom;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct ArborGraph {
    centrality: serde_json::Value,
    file_index: HashMap<String, Vec<usize>>,
    id_index: HashMap<String, usize>,
    name_index: HashMap<String, Vec<usize>>,
    graph: GraphData,
}

#[derive(Debug, Deserialize)]
struct GraphData {
    #[serde(default)]
    node_holes: Vec<usize>,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    edge_property: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
struct Node {
    id: String,
    name: String,
    qualified_name: String,
    kind: String,
    file: String,
    line_start: usize,
    line_end: usize,
    column: usize,
    signature: Option<String>,
    visibility: String,
    is_async: bool,
    is_static: bool,
    is_exported: bool,
    docstring: Option<String>,
    byte_start: usize,
    byte_end: usize,
    references: Vec<String>,
}

#[derive(Debug, Clone)]
struct Edge {
    source: usize,
    target: usize,
    kind: String,
    file: Option<String>,
    line: Option<usize>,
}

impl<'de> Deserialize<'de> for Edge {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct EdgeData {
            kind: String,
            file: Option<String>,
            line: Option<usize>,
        }

        let v = serde_json::Value::deserialize(deserializer)?;
        if let Some(arr) = v.as_array() {
            if arr.len() >= 3 {
                let source_u64 = arr[0]
                    .as_u64()
                    .ok_or(serde::de::Error::custom("Edge source must be a number"))?;
                let target_u64 = arr[1]
                    .as_u64()
                    .ok_or(serde::de::Error::custom("Edge target must be a number"))?;
                let source = usize::try_from(source_u64).map_err(|_| {
                    serde::de::Error::custom("Edge source does not fit in usize for this platform")
                })?;
                let target = usize::try_from(target_u64).map_err(|_| {
                    serde::de::Error::custom("Edge target does not fit in usize for this platform")
                })?;
                let data: EdgeData =
                    serde_json::from_value(arr[2].clone()).map_err(serde::de::Error::custom)?;
                Ok(Edge {
                    source,
                    target,
                    kind: data.kind,
                    file: data.file,
                    line: data.line,
                })
            } else {
                Err(serde::de::Error::custom(
                    "Edge array must have at least 3 elements",
                ))
            }
        } else {
            Err(serde::de::Error::custom("Edge must be an array"))
        }
    }
}

fn main() -> Result<()> {
    color_eyre::install()?;

    let graph_path = PathBuf::from("../../.arbor/graph.json");
    if !graph_path.exists() {
        return Err(eyre!("Graph file not found: {}", graph_path.display()));
    }

    println!("Reading graph from: {}", graph_path.display());
    let json_content =
        std::fs::read_to_string(&graph_path).wrap_err("Failed to read graph.json")?;

    let arbor_graph: ArborGraph =
        serde_json::from_str(&json_content).wrap_err("Failed to deserialize graph.json")?;

    println!("Loaded Arbor graph:");
    println!("  - Nodes: {}", arbor_graph.graph.nodes.len());
    println!("  - Edges: {}", arbor_graph.graph.edges.len());
    println!("  - Node holes: {}", arbor_graph.graph.node_holes.len());
    println!("  - ID index entries: {}", arbor_graph.id_index.len());
    println!("  - Name index entries: {}", arbor_graph.name_index.len());
    println!("  - File index entries: {}", arbor_graph.file_index.len());

    // Build petgraph
    let mut pet_graph: DiGraph<Node, Edge> = DiGraph::new();
    let mut id_to_node_idx: HashMap<usize, NodeIndex> = HashMap::new();

    // Add all nodes
    for (idx, node) in arbor_graph.graph.nodes.iter().enumerate() {
        let node_idx = pet_graph.add_node(node.clone());
        id_to_node_idx.insert(idx, node_idx);
    }

    // Add all edges
    for edge in &arbor_graph.graph.edges {
        let source_idx = id_to_node_idx.get(&edge.source).ok_or_else(|| {
            eyre!(
                "Missing source node index {} for edge kind {}",
                edge.source,
                edge.kind
            )
        })?;
        let target_idx = id_to_node_idx.get(&edge.target).ok_or_else(|| {
            eyre!(
                "Missing target node index {} for edge kind {}",
                edge.target,
                edge.kind
            )
        })?;
        pet_graph.add_edge(*source_idx, *target_idx, edge.clone());
    }

    println!("\nBuilt petgraph:");
    println!("  - Node count: {}", pet_graph.node_count());
    println!("  - Edge count: {}", pet_graph.edge_count());

    // Look up 'main' function via name_index
    if let Some(main_indices) = arbor_graph.name_index.get("main") {
        println!("\nFound {} node(s) named 'main'", main_indices.len());

        for &main_idx in main_indices {
            if let Some(&node_idx) = id_to_node_idx.get(&main_idx) {
                let node = &pet_graph[node_idx];
                println!("\n'main' at index {}:", main_idx);
                println!("  - Qualified name: {}", node.qualified_name);
                println!("  - Kind: {}", node.kind);
                println!("  - File: {}", node.file);
                println!("  - Lines: {}-{}", node.line_start, node.line_end);

                // Get callers (incoming edges)
                let callers: Vec<_> = pet_graph
                    .neighbors_directed(node_idx, petgraph::Direction::Incoming)
                    .map(|neighbor_idx| {
                        let neighbor = &pet_graph[neighbor_idx];
                        format!("{} ({})", neighbor.qualified_name, neighbor.kind)
                    })
                    .collect();

                println!("  - Callers ({}):", callers.len());
                for caller in callers {
                    println!("    - {}", caller);
                }

                // Get callees (outgoing edges)
                let callees: Vec<_> = pet_graph
                    .neighbors_directed(node_idx, petgraph::Direction::Outgoing)
                    .map(|neighbor_idx| {
                        let neighbor = &pet_graph[neighbor_idx];
                        format!("{} ({})", neighbor.qualified_name, neighbor.kind)
                    })
                    .collect();

                println!("  - Callees ({}):", callees.len());
                for callee in callees.iter().take(10) {
                    println!("    - {}", callee);
                }
                if callees.len() > 10 {
                    println!("    - ... and {} more", callees.len() - 10);
                }
            }
        }
    } else {
        println!("\nNo node named 'main' found in name_index");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Edge;

    #[test]
    fn edge_deserialization_parses_valid_triplet() {
        let edge: Edge =
            serde_json::from_str(r#"[1, 2, {"kind": "calls", "file": null, "line": 42}]"#)
                .expect("valid edge payload should deserialize");
        assert_eq!(edge.source, 1);
        assert_eq!(edge.target, 2);
        assert_eq!(edge.kind, "calls");
        assert_eq!(edge.file, None);
        assert_eq!(edge.line, Some(42));
    }

    #[test]
    fn edge_deserialization_rejects_non_numeric_source() {
        let result = serde_json::from_str::<Edge>(
            r#"["a", 2, {"kind": "calls", "file": null, "line": null}]"#,
        );
        assert!(result.is_err());
    }
}
