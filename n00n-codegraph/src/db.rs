use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use crate::CodegraphError;

const DEFAULT_RESULT_LIMIT: usize = 12;
const SOURCE_CONTEXT_LINES: u32 = 2;

pub fn db_path(project: &Path) -> PathBuf {
    project.join(".codegraph/codegraph.db")
}

pub fn has_database(project: &Path) -> bool {
    db_path(project).is_file()
}

pub fn explore_database(query: &str, project: &Path) -> Result<String, CodegraphError> {
    let db_path = db_path(project);
    let conn = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|source| CodegraphError::Sqlite { source })?;

    conn.pragma_update(None, "query_only", "true")
        .map_err(|source| CodegraphError::Sqlite { source })?;

    let nodes = search_nodes(&conn, query, DEFAULT_RESULT_LIMIT)?;
    if nodes.is_empty() {
        return Ok(format!("No codegraph matches for query: {query}"));
    }

    Ok(format_nodes(project, &nodes))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphNode {
    pub id: String,
    pub name: String,
    pub qualified_name: String,
    pub file_path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub signature: Option<String>,
    pub docstring: Option<String>,
}

fn search_nodes(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<GraphNode>, CodegraphError> {
    let fts_query = fts_query(query);
    let mut stmt = conn
        .prepare(
            "SELECT n.id, n.name, n.qualified_name, n.file_path, n.start_line, n.end_line, \
             n.signature, n.docstring \
             FROM nodes_fts fts \
             JOIN nodes n ON n.id = fts.id \
             WHERE nodes_fts MATCH ?1 \
             LIMIT ?2",
        )
        .map_err(|source| CodegraphError::Sqlite { source })?;

    let rows = stmt
        .query_map(
            (
                fts_query.as_str(),
                i64::try_from(limit).map_err(|_| CodegraphError::Cli {
                    message: String::from("result limit out of range"),
                })?,
            ),
            map_graph_node,
        )
        .map_err(|source| CodegraphError::Sqlite { source })?;

    let mut nodes = Vec::new();
    for row in rows {
        nodes.push(row.map_err(|source| CodegraphError::Sqlite { source })?);
    }

    if !nodes.is_empty() {
        return Ok(nodes);
    }

    let pattern = format!("%{}%", query.trim());
    let mut fallback = conn
        .prepare(
            "SELECT id, name, qualified_name, file_path, start_line, end_line, signature, docstring \
             FROM nodes \
             WHERE name LIKE ?1 OR qualified_name LIKE ?1 \
             LIMIT ?2",
        )
        .map_err(|source| CodegraphError::Sqlite { source })?;

    let fallback_rows = fallback
        .query_map(
            (
                pattern.as_str(),
                i64::try_from(limit).map_err(|_| CodegraphError::Cli {
                    message: String::from("result limit out of range"),
                })?,
            ),
            map_graph_node,
        )
        .map_err(|source| CodegraphError::Sqlite { source })?;

    for row in fallback_rows {
        nodes.push(row.map_err(|source| CodegraphError::Sqlite { source })?);
    }
    Ok(nodes)
}

fn map_graph_node(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphNode> {
    let start_line = line_u32(row.get(4)?)?;
    let end_line = line_u32(row.get(5)?)?;
    Ok(GraphNode {
        id: row.get(0)?,
        name: row.get(1)?,
        qualified_name: row.get(2)?,
        file_path: row.get(3)?,
        start_line,
        end_line,
        signature: row.get(6)?,
        docstring: row.get(7)?,
    })
}

fn line_u32(value: i64) -> rusqlite::Result<u32> {
    u32::try_from(value).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Integer,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "line number out of range",
            )),
        )
    })
}

fn fts_query(raw: &str) -> String {
    raw.split_whitespace()
        .filter(|part| !part.is_empty())
        .map(|part| {
            let cleaned = part.replace('"', "");
            format!("\"{cleaned}\"")
        })
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn format_nodes(project: &Path, nodes: &[GraphNode]) -> String {
    let mut sections = Vec::new();
    let mut seen_files = std::collections::BTreeSet::new();

    for node in nodes {
        if !seen_files.insert(node.file_path.clone()) {
            continue;
        }

        let file_path = resolve_file_path(project, &node.file_path);
        let header = format!("## {}\n", node.file_path);
        let meta = format!(
            "{} ({}, lines {}-{})\n",
            node.qualified_name, node.name, node.start_line, node.end_line
        );
        let snippet = read_snippet(&file_path, node.start_line, node.end_line)
            .unwrap_or_else(|err| format!("(source unavailable: {err:#})"));
        sections.push(format!("{header}{meta}\n{snippet}"));
    }

    sections.join("\n")
}

fn resolve_file_path(project: &Path, file_path: &str) -> PathBuf {
    let path = Path::new(file_path);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    project.join(path)
}

fn read_snippet(path: &Path, start_line: u32, end_line: u32) -> Result<String, CodegraphError> {
    let content = fs::read_to_string(path).map_err(|source| CodegraphError::Exec { source })?;
    let start = start_line.saturating_sub(SOURCE_CONTEXT_LINES).max(1) as usize;
    let end = end_line.saturating_add(SOURCE_CONTEXT_LINES) as usize;
    let mut lines = Vec::new();
    for (line_no, line) in content.lines().enumerate() {
        let current = line_no + 1;
        if current < start {
            continue;
        }
        if current > end {
            break;
        }
        lines.push(format!("{current:>6}\t{line}"));
    }
    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::{GraphNode, fts_query, search_nodes};
    use rusqlite::Connection;

    fn write_fixture(conn: &Connection) {
        conn.execute_batch(
            "CREATE TABLE nodes (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                name TEXT NOT NULL,
                qualified_name TEXT NOT NULL,
                file_path TEXT NOT NULL,
                language TEXT NOT NULL,
                start_line INTEGER NOT NULL,
                end_line INTEGER NOT NULL,
                start_column INTEGER NOT NULL,
                end_column INTEGER NOT NULL,
                docstring TEXT,
                signature TEXT,
                visibility TEXT,
                is_exported INTEGER DEFAULT 0,
                is_async INTEGER DEFAULT 0,
                is_static INTEGER DEFAULT 0,
                is_abstract INTEGER DEFAULT 0,
                decorators TEXT,
                type_parameters TEXT
            );
            CREATE VIRTUAL TABLE nodes_fts USING fts5(
                id UNINDEXED,
                name,
                qualified_name,
                docstring
            );
            INSERT INTO nodes VALUES (
                'node-1', 'function', 'restore_item', 'n00n_lua::restore_item',
                'src/restore.rs', 'rust', 10, 20, 0, 0, 'restore helper', 'fn restore_item()', 'pub', 1, 0, 0, 0, NULL, NULL
            );
            INSERT INTO nodes_fts(id, name, qualified_name, docstring)
                VALUES ('node-1', 'restore_item', 'n00n_lua::restore_item', 'restore helper');",
        )
        .expect("fixture schema");
    }

    #[test]
    fn fts_query_quotes_terms() {
        assert_eq!(fts_query("session restore"), "\"session\" OR \"restore\"");
    }

    #[test]
    fn search_nodes_matches_fts_index() {
        let conn = Connection::open_in_memory().expect("memory db");
        write_fixture(&conn);
        let nodes = search_nodes(&conn, "restore_item", 5).expect("search");
        assert_eq!(nodes.len(), 1);
        assert_eq!(
            nodes[0],
            GraphNode {
                id: String::from("node-1"),
                name: String::from("restore_item"),
                qualified_name: String::from("n00n_lua::restore_item"),
                file_path: String::from("src/restore.rs"),
                start_line: 10,
                end_line: 20,
                signature: Some(String::from("fn restore_item()")),
                docstring: Some(String::from("restore helper")),
            }
        );
    }
}
