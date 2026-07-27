use std::collections::{HashMap, VecDeque};
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub file: String,
    pub line_start: usize,
    pub line_end: usize,
}

#[derive(Debug, Clone)]
pub struct GraphEdge {
    pub source: usize,
    pub target: usize,
    pub kind: String,
}

#[derive(Debug, Deserialize)]
struct EdgeMeta {
    kind: String,
}

impl<'de> Deserialize<'de> for GraphEdge {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        let arr = value
            .as_array()
            .ok_or_else(|| serde::de::Error::custom("edge must be an array"))?;
        if arr.len() < 3 {
            return Err(serde::de::Error::custom(
                "edge array must have at least 3 elements",
            ));
        }

        let source_u64 = arr[0]
            .as_u64()
            .ok_or_else(|| serde::de::Error::custom("edge source must be a number"))?;
        let target_u64 = arr[1]
            .as_u64()
            .ok_or_else(|| serde::de::Error::custom("edge target must be a number"))?;
        let source = usize::try_from(source_u64)
            .map_err(|_| serde::de::Error::custom("edge source does not fit usize"))?;
        let target = usize::try_from(target_u64)
            .map_err(|_| serde::de::Error::custom("edge target does not fit usize"))?;

        let meta: EdgeMeta =
            serde_json::from_value(arr[2].clone()).map_err(serde::de::Error::custom)?;

        Ok(Self {
            source,
            target,
            kind: meta.kind,
        })
    }
}

#[derive(Debug, Deserialize)]
struct GraphData {
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
}

#[derive(Debug, Deserialize)]
struct RawArborGraph {
    file_index: HashMap<String, Vec<usize>>,
    id_index: HashMap<String, usize>,
    name_index: HashMap<String, Vec<usize>>,
    graph: GraphData,
}

#[derive(Debug, Clone)]
pub struct SymbolRef {
    pub index: usize,
    pub node: GraphNode,
}

#[derive(Debug)]
pub struct GraphIndex {
    nodes: Vec<GraphNode>,
    outgoing: HashMap<usize, Vec<usize>>,
    incoming: HashMap<usize, Vec<usize>>,
    file_index: HashMap<String, Vec<usize>>,
    id_index: HashMap<String, usize>,
    name_index: HashMap<String, Vec<usize>>,
}

impl GraphIndex {
    pub fn from_json_str(content: &str) -> Result<Self, crate::ArborError> {
        let raw: RawArborGraph =
            serde_json::from_str(content).map_err(|source| crate::ArborError::Parse { source })?;
        let mut outgoing: HashMap<usize, Vec<usize>> = HashMap::new();
        let mut incoming: HashMap<usize, Vec<usize>> = HashMap::new();

        for edge in raw.graph.edges {
            if edge.source >= raw.graph.nodes.len() || edge.target >= raw.graph.nodes.len() {
                return Err(crate::ArborError::Cli {
                    message: format!(
                        "graph edge index out of bounds: {} -> {} ({})",
                        edge.source, edge.target, edge.kind
                    ),
                });
            }
            outgoing.entry(edge.source).or_default().push(edge.target);
            incoming.entry(edge.target).or_default().push(edge.source);
        }

        Ok(Self {
            nodes: raw.graph.nodes,
            outgoing,
            incoming,
            file_index: raw.file_index,
            id_index: raw.id_index,
            name_index: raw.name_index,
        })
    }

    pub fn from_graph_json_path(path: &Path) -> Result<Self, crate::ArborError> {
        let content =
            std::fs::read_to_string(path).map_err(|source| crate::ArborError::Exec { source })?;
        Self::from_json_str(&content)
    }

    pub fn find_symbol(&self, name: &str) -> Vec<SymbolRef> {
        match self.name_index.get(name) {
            Some(indices) => indices
                .iter()
                .filter_map(|idx| {
                    self.nodes
                        .get(*idx)
                        .cloned()
                        .map(|node| SymbolRef { index: *idx, node })
                })
                .collect(),
            None => Vec::new(),
        }
    }

    pub fn find_in_file(&self, file: &str) -> Vec<SymbolRef> {
        match self.file_index.get(file) {
            Some(indices) => indices
                .iter()
                .filter_map(|idx| {
                    self.nodes
                        .get(*idx)
                        .cloned()
                        .map(|node| SymbolRef { index: *idx, node })
                })
                .collect(),
            None => Vec::new(),
        }
    }

    pub fn find_by_id(&self, id: &str) -> Option<SymbolRef> {
        self.id_index.get(id).and_then(|idx| {
            self.nodes
                .get(*idx)
                .cloned()
                .map(|node| SymbolRef { index: *idx, node })
        })
    }

    pub fn find_callers(&self, index: usize) -> Vec<SymbolRef> {
        match self.incoming.get(&index) {
            Some(indices) => self.symbols_from_indices(indices),
            None => Vec::new(),
        }
    }

    pub fn find_callees(&self, index: usize) -> Vec<SymbolRef> {
        match self.outgoing.get(&index) {
            Some(indices) => self.symbols_from_indices(indices),
            None => Vec::new(),
        }
    }

    pub fn trace_path(&self, from: usize, to: usize) -> Option<Vec<SymbolRef>> {
        if from >= self.nodes.len() || to >= self.nodes.len() {
            return None;
        }
        if from == to {
            return self
                .nodes
                .get(from)
                .cloned()
                .map(|node| vec![SymbolRef { index: from, node }]);
        }

        let mut queue = VecDeque::new();
        let mut visited: HashMap<usize, Option<usize>> = HashMap::new();
        queue.push_back(from);
        visited.insert(from, None);

        while let Some(current) = queue.pop_front() {
            let neighbors = match self.outgoing.get(&current) {
                Some(existing) => existing.clone(),
                None => Vec::new(),
            };
            for neighbor in neighbors {
                if visited.contains_key(&neighbor) {
                    continue;
                }
                visited.insert(neighbor, Some(current));
                if neighbor == to {
                    return self.reconstruct_path(&visited, to);
                }
                queue.push_back(neighbor);
            }
        }

        None
    }

    fn reconstruct_path(
        &self,
        visited: &HashMap<usize, Option<usize>>,
        end: usize,
    ) -> Option<Vec<SymbolRef>> {
        let mut path = Vec::new();
        let mut current = Some(end);
        while let Some(idx) = current {
            let node = self.nodes.get(idx).cloned()?;
            path.push(SymbolRef { index: idx, node });
            current = visited.get(&idx).copied().flatten();
        }
        path.reverse();
        Some(path)
    }

    fn symbols_from_indices(&self, indices: &[usize]) -> Vec<SymbolRef> {
        indices
            .iter()
            .filter_map(|idx| {
                self.nodes
                    .get(*idx)
                    .cloned()
                    .map(|node| SymbolRef { index: *idx, node })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::GraphIndex;

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
    fn find_symbol_returns_matches() {
        let graph = GraphIndex::from_json_str(SAMPLE_GRAPH).expect("sample graph should parse");
        let symbols = graph.find_symbol("helper");
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].node.qualified_name, "crate::helper");
    }

    #[test]
    fn find_callers_and_callees_use_edges() {
        let graph = GraphIndex::from_json_str(SAMPLE_GRAPH).expect("sample graph should parse");
        let incoming_neighbors = graph.find_callers(1);
        let outgoing_targets = graph.find_callees(1);
        assert_eq!(incoming_neighbors.len(), 1);
        assert_eq!(incoming_neighbors[0].node.name, "main");
        assert_eq!(outgoing_targets.len(), 1);
        assert_eq!(outgoing_targets[0].node.name, "lib_fn");
    }

    #[test]
    fn trace_path_returns_shortest_path() {
        let graph = GraphIndex::from_json_str(SAMPLE_GRAPH).expect("sample graph should parse");
        let path = graph.trace_path(0, 2).expect("path should exist");
        let names: Vec<_> = path.into_iter().map(|symbol| symbol.node.name).collect();
        assert_eq!(names, vec!["main", "helper", "lib_fn"]);
    }

    #[test]
    fn parse_rejects_out_of_bounds_edge() {
        let json = r#"{
          "file_index": {},
          "id_index": {},
          "name_index": {},
          "graph": {
            "nodes": [],
            "edges": [[0, 1, { "kind": "calls" }]]
          }
        }"#;
        let result = GraphIndex::from_json_str(json);
        assert!(result.is_err());
    }
}
