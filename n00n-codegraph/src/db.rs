use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

#[cfg(all(
    unix,
    not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
))]
use rustix::fs::{Mode, OFlags, open, openat};
#[cfg(all(
    unix,
    not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
))]
use std::fs;

use rusqlite::{Connection, OpenFlags};

use crate::CodegraphError;

const DEFAULT_RESULT_LIMIT: usize = 12;
const CALLS_EDGE_KIND: &str = "calls";
const SOURCE_CONTEXT_LINES: u32 = 2;
const SOURCE_MAX_BYTES: u64 = 1024 * 1024;
const SOURCE_UNAVAILABLE: &str = "(source unavailable)";

fn like_pattern(input: &str) -> String {
    let escaped = input
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

pub fn db_path(project: &Path) -> PathBuf {
    project.join(".codegraph/codegraph.db")
}

pub fn has_database(project: &Path) -> bool {
    db_path(project).is_file()
}

fn open_readonly(project: &Path) -> Result<Connection, CodegraphError> {
    let conn = Connection::open_with_flags(
        db_path(project),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|source| CodegraphError::Sqlite { source })?;

    conn.pragma_update(None, "query_only", "true")
        .map_err(|source| CodegraphError::Sqlite { source })?;

    Ok(conn)
}

pub fn explore_database(query: &str, project: &Path) -> Result<String, CodegraphError> {
    let conn = open_readonly(project)?;

    let nodes = search_nodes(&conn, query, DEFAULT_RESULT_LIMIT)?;
    if nodes.is_empty() {
        return Ok(format!("No codegraph matches for query: {query}"));
    }

    Ok(format_nodes(project, &nodes))
}

pub fn callers_database(symbol: &str, project: &Path) -> Result<String, CodegraphError> {
    let conn = open_readonly(project)?;

    let nodes = search_callers(&conn, symbol, DEFAULT_RESULT_LIMIT)?;
    if nodes.is_empty() {
        return Ok(format!("No callers found for symbol: {symbol}"));
    }

    Ok(format_nodes(project, &nodes))
}

pub fn callees_database(symbol: &str, project: &Path) -> Result<String, CodegraphError> {
    let conn = open_readonly(project)?;

    let nodes = search_callees(&conn, symbol, DEFAULT_RESULT_LIMIT)?;
    if nodes.is_empty() {
        return Ok(format!("No callees found for symbol: {symbol}"));
    }

    Ok(format_nodes(project, &nodes))
}

pub fn impact_database(symbol: &str, project: &Path) -> Result<String, CodegraphError> {
    let conn = open_readonly(project)?;

    let nodes = search_impact(&conn, symbol, DEFAULT_RESULT_LIMIT)?;
    if nodes.is_empty() {
        return Ok(format!("No impact found for symbol: {symbol}"));
    }

    Ok(format_nodes(project, &nodes))
}

pub fn node_database(name: &str, project: &Path) -> Result<String, CodegraphError> {
    let conn = open_readonly(project)?;

    let nodes = search_nodes(&conn, name, 1)?;
    if nodes.is_empty() {
        return Ok(format!("No node found for name: {name}"));
    }

    Ok(format_nodes(project, &nodes))
}

pub fn query_database(search: &str, project: &Path) -> Result<String, CodegraphError> {
    let conn = open_readonly(project)?;

    let nodes = search_nodes(&conn, search, DEFAULT_RESULT_LIMIT)?;
    if nodes.is_empty() {
        return Ok(format!("No codegraph matches for search: {search}"));
    }

    Ok(format_nodes(project, &nodes))
}

pub fn files_database(project: &Path) -> Result<String, CodegraphError> {
    let conn = open_readonly(project)?;

    let mut stmt = conn
        .prepare("SELECT DISTINCT file_path FROM nodes ORDER BY file_path")
        .map_err(|source| CodegraphError::Sqlite { source })?;

    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|source| CodegraphError::Sqlite { source })?;

    let mut files = Vec::new();
    for row in rows {
        files.push(row.map_err(|source| CodegraphError::Sqlite { source })?);
    }

    if files.is_empty() {
        return Ok(String::from("No files found in index"));
    }

    Ok(files.join("\n"))
}

#[allow(clippy::similar_names)]
pub fn affected_database(files: &[&str], project: &Path) -> Result<String, CodegraphError> {
    let conn = open_readonly(project)?;

    // Prepare statement once for finding nodes in changed files
    let mut find_nodes_stmt = conn
        .prepare("SELECT id FROM nodes WHERE file_path LIKE ?1 ESCAPE '\\'")
        .map_err(|source| CodegraphError::Sqlite { source })?;

    // Prepare statement for finding callers (files that call nodes in changed files)
    let mut callers_stmt = conn
        .prepare(
            "SELECT DISTINCT caller.file_path
             FROM nodes n
             JOIN edges e ON e.target = n.id
             JOIN nodes caller ON caller.id = e.source
             WHERE n.id = ?1 AND e.kind = ?2",
        )
        .map_err(|source| CodegraphError::Sqlite { source })?;

    // Prepare statement for finding callees (files called by nodes in changed files)
    let mut callees_stmt = conn
        .prepare(
            "SELECT DISTINCT callee.file_path
             FROM nodes n
             JOIN edges e ON e.source = n.id
             JOIN nodes callee ON callee.id = e.target
             WHERE n.id = ?1 AND e.kind = ?2",
        )
        .map_err(|source| CodegraphError::Sqlite { source })?;

    let mut seen = std::collections::BTreeSet::new();

    for file in files {
        let pattern = like_pattern(file.trim());

        // Find all nodes in the changed file
        let node_rows = find_nodes_stmt
            .query_map((pattern.as_str(),), |row| row.get::<_, String>(0))
            .map_err(|source| CodegraphError::Sqlite { source })?;

        for node_id_result in node_rows {
            let node_id = node_id_result.map_err(|source| CodegraphError::Sqlite { source })?;

            // Find files that call this node
            let callers_rows = callers_stmt
                .query_map((&node_id, CALLS_EDGE_KIND), |row| row.get::<_, String>(0))
                .map_err(|source| CodegraphError::Sqlite { source })?;

            for caller_path_result in callers_rows {
                seen.insert(
                    caller_path_result.map_err(|source| CodegraphError::Sqlite { source })?,
                );
            }

            // Find files called by this node
            let callees_rows = callees_stmt
                .query_map((&node_id, CALLS_EDGE_KIND), |row| row.get::<_, String>(0))
                .map_err(|source| CodegraphError::Sqlite { source })?;

            for callee_path_result in callees_rows {
                seen.insert(
                    callee_path_result.map_err(|source| CodegraphError::Sqlite { source })?,
                );
            }
        }

        // Also include the changed file itself
        // Since we're using LIKE pattern, we need to get the actual file paths
        let mut file_match_stmt = conn
            .prepare("SELECT DISTINCT file_path FROM nodes WHERE file_path LIKE ?1 ESCAPE '\\'")
            .map_err(|source| CodegraphError::Sqlite { source })?;

        let file_matches = file_match_stmt
            .query_map((pattern.as_str(),), |row| row.get::<_, String>(0))
            .map_err(|source| CodegraphError::Sqlite { source })?;

        for file_path_result in file_matches {
            seen.insert(file_path_result.map_err(|source| CodegraphError::Sqlite { source })?);
        }
    }

    let affected_files: Vec<String> = seen.into_iter().collect();

    if affected_files.is_empty() {
        return Ok(format!("No affected files found for: {}", files.join(", ")));
    }

    // Apply result limit
    let limited: Vec<String> = affected_files
        .into_iter()
        .take(DEFAULT_RESULT_LIMIT)
        .collect();

    Ok(limited.join("\n"))
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
    let limit_i64 = i64::try_from(limit).map_err(|_| CodegraphError::Cli {
        message: String::from("result limit out of range"),
    })?;

    let fts_query = fts_query(query);
    if !fts_query.is_empty() {
        let fts_res: Result<Vec<GraphNode>, rusqlite::Error> = (|| {
            let mut stmt = conn.prepare(
                "SELECT n.id, n.name, n.qualified_name, n.file_path, n.start_line, n.end_line, \
                 n.signature, n.docstring \
                 FROM nodes_fts fts \
                 JOIN nodes n ON n.id = fts.id \
                 WHERE nodes_fts MATCH ?1 \
                 LIMIT ?2",
            )?;
            let rows = stmt.query_map((fts_query.as_str(), limit_i64), map_graph_node)?;
            let mut nodes = Vec::new();
            for row in rows {
                nodes.push(row?);
            }
            Ok(nodes)
        })();

        match fts_res {
            Ok(nodes) if !nodes.is_empty() => return Ok(nodes),
            _ => {}
        }
    }

    let pattern = like_pattern(query.trim());
    let mut fallback = conn
        .prepare(
            "SELECT id, name, qualified_name, file_path, start_line, end_line, signature, docstring \
             FROM nodes \
             WHERE name LIKE ?1 ESCAPE '\\' OR qualified_name LIKE ?1 ESCAPE '\\' \
             LIMIT ?2",
        )
        .map_err(|source| CodegraphError::Sqlite { source })?;

    let fallback_rows = fallback
        .query_map((pattern.as_str(), limit_i64), map_graph_node)
        .map_err(|source| CodegraphError::Sqlite { source })?;

    let mut nodes = Vec::new();
    for row in fallback_rows {
        nodes.push(row.map_err(|source| CodegraphError::Sqlite { source })?);
    }
    Ok(nodes)
}

pub fn search_callers(
    conn: &Connection,
    symbol: &str,
    limit: usize,
) -> Result<Vec<GraphNode>, CodegraphError> {
    let pattern = like_pattern(symbol.trim());
    let mut stmt = conn
        .prepare(
            "SELECT caller.id, caller.name, caller.qualified_name, caller.file_path, caller.start_line, caller.end_line, \
             caller.signature, caller.docstring \
             FROM nodes n \
             JOIN edges e ON e.target = n.id \
             JOIN nodes caller ON caller.id = e.source \
             WHERE (n.name LIKE ?1 ESCAPE '\\' OR n.qualified_name LIKE ?1 ESCAPE '\\') AND e.kind = ?2 \
             LIMIT ?3",
        )
        .map_err(|source| CodegraphError::Sqlite { source })?;

    let rows = stmt
        .query_map(
            (
                pattern.as_str(),
                CALLS_EDGE_KIND,
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
    Ok(nodes)
}

pub fn search_callees(
    conn: &Connection,
    symbol: &str,
    limit: usize,
) -> Result<Vec<GraphNode>, CodegraphError> {
    let pattern = like_pattern(symbol.trim());
    let mut stmt = conn
        .prepare(
            "SELECT callee.id, callee.name, callee.qualified_name, callee.file_path, callee.start_line, callee.end_line, \
             callee.signature, callee.docstring \
             FROM nodes n \
             JOIN edges e ON e.source = n.id \
             JOIN nodes callee ON callee.id = e.target \
             WHERE (n.name LIKE ?1 ESCAPE '\\' OR n.qualified_name LIKE ?1 ESCAPE '\\') AND e.kind = ?2 \
             LIMIT ?3",
        )
        .map_err(|source| CodegraphError::Sqlite { source })?;

    let rows = stmt
        .query_map(
            (
                pattern.as_str(),
                CALLS_EDGE_KIND,
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
    Ok(nodes)
}

pub fn search_impact(
    conn: &Connection,
    symbol: &str,
    limit: usize,
) -> Result<Vec<GraphNode>, CodegraphError> {
    let pattern = like_pattern(symbol.trim());
    let mut stmt = conn
        .prepare(
            "SELECT n.id, n.name, n.qualified_name, n.file_path, n.start_line, n.end_line, n.signature, n.docstring FROM nodes n
             WHERE n.name LIKE ?1 ESCAPE '\\' OR n.qualified_name LIKE ?1 ESCAPE '\\'
             UNION
             SELECT n2.id, n2.name, n2.qualified_name, n2.file_path, n2.start_line, n2.end_line, n2.signature, n2.docstring
             FROM nodes n
             JOIN edges e ON e.source = n.id
             JOIN nodes n2 ON n2.id = e.target
             WHERE (n.name LIKE ?1 ESCAPE '\\' OR n.qualified_name LIKE ?1 ESCAPE '\\') AND e.kind = ?2
             UNION
             SELECT n2.id, n2.name, n2.qualified_name, n2.file_path, n2.start_line, n2.end_line, n2.signature, n2.docstring
             FROM nodes n
             JOIN edges e ON e.target = n.id
             JOIN nodes n2 ON n2.id = e.source
             WHERE (n.name LIKE ?1 ESCAPE '\\' OR n.qualified_name LIKE ?1 ESCAPE '\\') AND e.kind = ?2
             LIMIT ?3",
        )
        .map_err(|source| CodegraphError::Sqlite { source })?;

    let rows = stmt
        .query_map(
            (
                pattern.as_str(),
                CALLS_EDGE_KIND,
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
        .map(|part| part.replace('"', ""))
        .filter(|cleaned| !cleaned.is_empty())
        .map(|cleaned| format!("\"{cleaned}\""))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn format_nodes(project: &Path, nodes: &[GraphNode]) -> String {
    let mut sections = Vec::new();

    for node in nodes {
        let header = format!("## {}\n", node.file_path);
        let meta = format!(
            "{} ({}, lines {}-{})\n",
            node.qualified_name, node.name, node.start_line, node.end_line
        );
        let snippet = match read_snippet(project, &node.file_path, node.start_line, node.end_line) {
            Ok(snippet) => snippet,
            Err(error) => {
                tracing::warn!(
                    file_path = %node.file_path,
                    error = %error,
                    "codegraph source snippet read failed"
                );
                String::from(SOURCE_UNAVAILABLE)
            }
        };
        sections.push(format!("{header}{meta}\n{snippet}"));
    }

    sections.join("\n")
}

fn validate_source_path(file_path: &str) -> Result<&Path, CodegraphError> {
    let path = Path::new(file_path);
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(CodegraphError::SourcePath {
            reason: "path must be a non-empty relative path without parent components",
        });
    }
    Ok(path)
}

#[cfg(all(
    unix,
    not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
))]
fn open_source_file(project: &Path, path: &Path) -> Result<File, CodegraphError> {
    open_source_file_with(project, path, || {})
}

#[cfg(all(
    unix,
    not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
))]
fn open_source_file_with<F>(
    project: &Path,
    path: &Path,
    mut after_directory_open: F,
) -> Result<File, CodegraphError>
where
    F: FnMut(),
{
    let project_root =
        fs::canonicalize(project).map_err(|source| CodegraphError::Exec { source })?;
    let directory_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory =
        open("/", directory_flags, Mode::empty()).map_err(|source| CodegraphError::Exec {
            source: source.into(),
        })?;
    for component in project_root.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                directory =
                    openat(&directory, name, directory_flags, Mode::empty()).map_err(|source| {
                        CodegraphError::Exec {
                            source: source.into(),
                        }
                    })?;
            }
            _ => {
                return Err(CodegraphError::SourcePath {
                    reason: "project root could not be resolved to an absolute directory",
                });
            }
        }
    }
    let mut components = path.components().peekable();

    while let Some(Component::Normal(component)) = components.next() {
        if components.peek().is_some() {
            directory = openat(&directory, component, directory_flags, Mode::empty()).map_err(
                |source| CodegraphError::Exec {
                    source: source.into(),
                },
            )?;
            after_directory_open();
        } else {
            let file = openat(
                &directory,
                component,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                Mode::empty(),
            )
            .map_err(|source| CodegraphError::Exec {
                source: source.into(),
            })?;
            return Ok(File::from(file));
        }
    }

    Err(CodegraphError::SourcePath {
        reason: "path must name a source file",
    })
}

#[cfg(not(all(
    unix,
    not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
)))]
fn open_source_file(_project: &Path, _path: &Path) -> Result<File, CodegraphError> {
    Err(CodegraphError::SourceSnippetsUnsupported)
}

fn read_snippet(
    project: &Path,
    file_path: &str,
    start_line: u32,
    end_line: u32,
) -> Result<String, CodegraphError> {
    let path = validate_source_path(file_path)?;
    let mut file = open_source_file(project, path)?;
    let metadata = file
        .metadata()
        .map_err(|source| CodegraphError::Exec { source })?;
    if !metadata.is_file() {
        return Err(CodegraphError::SourcePath {
            reason: "resolved path is not a regular file",
        });
    }
    if metadata.len() > SOURCE_MAX_BYTES {
        return Err(CodegraphError::SourceTooLarge {
            size: metadata.len(),
            max: SOURCE_MAX_BYTES,
        });
    }

    let mut content = String::new();
    file.by_ref()
        .take(SOURCE_MAX_BYTES + 1)
        .read_to_string(&mut content)
        .map_err(|source| CodegraphError::Exec { source })?;
    if content.len() as u64 > SOURCE_MAX_BYTES {
        return Err(CodegraphError::SourceTooLarge {
            size: content.len() as u64,
            max: SOURCE_MAX_BYTES,
        });
    }
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
    use std::fs;
    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    use std::io::Read as _;

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    use super::SOURCE_MAX_BYTES;
    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    use super::open_source_file_with;
    use super::{
        GraphNode, SOURCE_UNAVAILABLE, format_nodes, fts_query, search_callees, search_callers,
        search_impact, search_nodes,
    };
    use rusqlite::Connection;

    const SECRET: &str = "must not escape project root";

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
            CREATE TABLE edges (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                source TEXT NOT NULL,
                target TEXT NOT NULL,
                kind TEXT NOT NULL,
                metadata TEXT,
                line INTEGER,
                col INTEGER,
                provenance TEXT DEFAULT NULL,
                FOREIGN KEY (source) REFERENCES nodes(id) ON DELETE CASCADE,
                FOREIGN KEY (target) REFERENCES nodes(id) ON DELETE CASCADE
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
            INSERT INTO nodes VALUES (
                'node-2', 'function', 'main', 'n00n::main',
                'src/main.rs', 'rust', 1, 5, 0, 0, 'entry point', 'fn main()', 'pub', 1, 0, 0, 0, NULL, NULL
            );
            INSERT INTO nodes VALUES (
                'node-3', 'function', 'process', 'n00n::process',
                'src/process.rs', 'rust', 5, 15, 0, 0, 'process data', 'fn process()', 'pub', 1, 0, 0, 0, NULL, NULL
            );
            INSERT INTO edges (source, target, kind) VALUES ('node-2', 'node-1', 'calls');
            INSERT INTO edges (source, target, kind) VALUES ('node-2', 'node-3', 'calls');
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
    fn fts_query_filters_quotes_and_empty_terms() {
        assert_eq!(fts_query("\"\""), "");
        assert_eq!(fts_query("\""), "");
        assert_eq!(fts_query("foo \"\" bar"), "\"foo\" OR \"bar\"");
        assert_eq!(fts_query("   "), "");
    }

    #[test]
    fn search_nodes_handles_quote_only_query_gracefully() {
        let conn = Connection::open_in_memory().expect("memory db");
        write_fixture(&conn);

        let nodes_quote = search_nodes(&conn, "\"", 5).expect("quote search");
        assert!(nodes_quote.is_empty());

        let nodes_double_quotes = search_nodes(&conn, "\"\"", 5).expect("double quote search");
        assert!(nodes_double_quotes.is_empty());
    }

    #[test]
    fn search_nodes_handles_malformed_fts_queries() {
        let conn = Connection::open_in_memory().expect("memory db");
        write_fixture(&conn);

        let result = search_nodes(&conn, "AND OR NOT NEAR", 5);
        assert!(result.is_ok());
    }

    /// The fixture previously declared `edges(source_id, target_id)` while the
    /// index codegraph actually writes uses `edges(source, target)`. Every
    /// caller/callee/impact query therefore failed at runtime with
    /// `no such column: e.target_id` while the tests passed against the wrong
    /// schema. Pin the column names so the fixture cannot drift back.
    #[test]
    fn fixture_edges_match_the_indexed_schema() {
        const REQUIRED_COLUMNS: [&str; 3] = ["source", "target", "kind"];
        const REJECTED_COLUMNS: [&str; 2] = ["source_id", "target_id"];

        let conn = Connection::open_in_memory().expect("memory db");
        write_fixture(&conn);
        let mut stmt = conn
            .prepare("SELECT name FROM pragma_table_info('edges')")
            .expect("pragma");
        let columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("columns");

        for column in REQUIRED_COLUMNS {
            assert!(
                columns.iter().any(|name| name == column),
                "fixture edges is missing {column}: {columns:?}"
            );
        }
        for column in REJECTED_COLUMNS {
            assert!(
                !columns.iter().any(|name| name == column),
                "fixture edges still declares {column}: {columns:?}"
            );
        }
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

    #[test]
    fn search_callers_finds_calling_nodes() {
        let conn = Connection::open_in_memory().expect("memory db");
        write_fixture(&conn);
        let nodes = search_callers(&conn, "restore_item", 5).expect("search callers");
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].name, "main");
    }

    #[test]
    fn search_callees_finds_called_nodes() {
        let conn = Connection::open_in_memory().expect("memory db");
        write_fixture(&conn);
        let nodes = search_callees(&conn, "main", 5).expect("search callees");
        assert_eq!(nodes.len(), 2);
        assert!(nodes.iter().any(|n| n.name == "restore_item"));
        assert!(nodes.iter().any(|n| n.name == "process"));
    }

    #[test]
    fn search_impact_finds_blast_radius() {
        let conn = Connection::open_in_memory().expect("memory db");
        write_fixture(&conn);
        let nodes = search_impact(&conn, "main", 5).expect("search impact");
        // main calls restore_item and process, so impact should include main + callees
        assert!(!nodes.is_empty());
        assert!(nodes.iter().any(|n| n.name == "main"));
        assert!(nodes.iter().any(|n| n.name == "restore_item"));
        assert!(nodes.iter().any(|n| n.name == "process"));
    }

    fn node(file_path: String) -> GraphNode {
        GraphNode {
            id: String::from("node-1"),
            name: String::from("secret"),
            qualified_name: String::from("secret"),
            file_path,
            start_line: 1,
            end_line: 1,
            signature: None,
            docstring: None,
        }
    }

    fn assert_source_unavailable(project: &std::path::Path, file_path: String) {
        let output = format_nodes(project, &[node(file_path)]);
        assert!(output.contains(SOURCE_UNAVAILABLE));
        assert!(!output.contains(SECRET));
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn format_nodes_reads_regular_source_inside_project() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("source.rs"), "fn visible() {}").expect("source fixture");

        let output = format_nodes(temp.path(), &[node(String::from("source.rs"))]);
        assert!(output.contains("fn visible() {}"));
        assert!(!output.contains("source unavailable"));
    }

    #[test]
    fn format_nodes_rejects_absolute_source_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().join("project");
        fs::create_dir(&project).expect("project directory");
        let secret = temp.path().join("secret.rs");
        fs::write(&secret, SECRET).expect("secret fixture");

        assert_source_unavailable(&project, secret.to_string_lossy().into_owned());
    }

    #[test]
    fn format_nodes_rejects_empty_source_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        assert_source_unavailable(temp.path(), String::new());
    }

    #[test]
    fn format_nodes_rejects_parent_source_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().join("project");
        fs::create_dir(&project).expect("project directory");
        fs::write(temp.path().join("secret.rs"), SECRET).expect("secret fixture");

        assert_source_unavailable(&project, String::from("../secret.rs"));
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn format_nodes_rejects_symlink_outside_project() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().join("project");
        fs::create_dir(&project).expect("project directory");
        fs::write(temp.path().join("secret.rs"), SECRET).expect("secret fixture");
        symlink(temp.path().join("secret.rs"), project.join("linked.rs")).expect("source symlink");

        assert_source_unavailable(&project, String::from("linked.rs"));
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn format_nodes_rejects_symlink_path_component() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().join("project");
        let outside = temp.path().join("outside");
        fs::create_dir(&project).expect("project directory");
        fs::create_dir(&outside).expect("outside directory");
        fs::write(outside.join("secret.rs"), SECRET).expect("secret fixture");
        symlink(&outside, project.join("linked")).expect("directory symlink");

        assert_source_unavailable(&project, String::from("linked/secret.rs"));
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn source_open_stays_anchored_when_directory_is_swapped() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().join("project");
        let source_directory = project.join("component");
        let moved_directory = project.join("original");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&source_directory).expect("source directory");
        fs::create_dir(&outside).expect("outside directory");
        fs::write(source_directory.join("source.rs"), "safe source").expect("safe fixture");
        fs::write(outside.join("source.rs"), SECRET).expect("secret fixture");

        let mut file = open_source_file_with(
            &project,
            std::path::Path::new("component/source.rs"),
            || {
                fs::rename(&source_directory, &moved_directory).expect("move opened directory");
                symlink(&outside, &source_directory).expect("swap directory for symlink");
            },
        )
        .expect("open anchored source");
        let mut content = String::new();
        file.read_to_string(&mut content).expect("read source");

        assert_eq!(content, "safe source");
        assert!(!content.contains(SECRET));
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn format_nodes_rejects_non_regular_source() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::create_dir(temp.path().join("src")).expect("source directory");

        assert_source_unavailable(temp.path(), String::from("src"));
    }

    #[cfg(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    ))]
    #[test]
    fn format_nodes_rejects_oversized_source() {
        let temp = tempfile::tempdir().expect("tempdir");
        let oversized_len = usize::try_from(SOURCE_MAX_BYTES).expect("source limit fits usize") + 1;
        fs::write(temp.path().join("large.rs"), vec![b'x'; oversized_len])
            .expect("large source fixture");

        assert_source_unavailable(temp.path(), String::from("large.rs"));
    }

    #[cfg(not(all(
        unix,
        not(any(target_os = "espidf", target_os = "horizon", target_os = "redox"))
    )))]
    #[test]
    fn format_nodes_disables_source_reads_without_safe_opening() {
        let output = format_nodes(
            std::path::Path::new("."),
            &[node(String::from("source.rs"))],
        );

        assert!(output.contains(SOURCE_UNAVAILABLE));
    }
}
