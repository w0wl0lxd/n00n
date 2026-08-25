#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use n00n_agent::tools::ToolRegistry;
use n00n_agent::{AgentMode, ToolOutput};
use n00n_lua::PluginHost;
use serde_json::{Value, json};
use tempfile::TempDir;

const MEMORY_SOURCE: &str = include_str!("../../plugins/memory/init.lua");

fn fixture_host(state_dir: &Path) -> (Arc<ToolRegistry>, PluginHost) {
    let registry = Arc::new(ToolRegistry::new());
    let host = PluginHost::new(Arc::clone(&registry)).unwrap();
    let state_dir = serde_json::to_string(&state_dir.to_string_lossy()).unwrap();
    host.load_source(
        "memory_fixture",
        &format!("n00n.env.state_dir = function() return {state_dir} end\n{MEMORY_SOURCE}"),
    )
    .unwrap();
    (registry, host)
}

fn exec_tool(registry: &ToolRegistry, input: Value) -> Result<String, String> {
    let entry = registry.get("memory").expect("memory registered");
    let invocation = entry.tool.parse(&input).expect("valid memory input");
    let context = n00n_agent::tools::test_support::stub_ctx(&AgentMode::Build);
    smol::block_on(invocation.execute(&context))
        .output
        .map(|output| match output {
            ToolOutput::Plain(output) | ToolOutput::Markdown(output) => output.text,
            other => panic!("unexpected memory output: {other:?}"),
        })
}

fn find_file(root: &Path, name: &str) -> PathBuf {
    fn visit(dir: &Path, name: &str) -> Option<PathBuf> {
        for entry in fs::read_dir(dir).ok()? {
            let path = entry.ok()?.path();
            if path.file_name().is_some_and(|file_name| file_name == name) {
                return Some(path);
            }
            if path.is_dir()
                && let Some(found) = visit(&path, name)
            {
                return Some(found);
            }
        }
        None
    }

    visit(root, name).expect("fixture file created")
}

#[test]
fn memory_rejects_physical_symlink_escapes_across_all_storage_paths() {
    let state = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let secret_path = outside.path().join("secret.md");
    fs::write(&secret_path, "outside-secret").unwrap();

    let (registry, _host) = fixture_host(state.path());
    exec_tool(
        &registry,
        json!({ "command": "write", "path": "safe.md", "content": "inside" }),
    )
    .expect("safe write succeeds");
    let memories_dir = find_file(state.path(), "safe.md")
        .parent()
        .expect("memory parent")
        .to_path_buf();
    symlink(&secret_path, memories_dir.join("leak.md")).unwrap();
    symlink(outside.path(), memories_dir.join("escape")).unwrap();

    for input in [
        json!({ "command": "view", "path": "leak.md" }),
        json!({ "command": "write", "path": "leak.md", "content": "overwrite" }),
        json!({ "command": "append", "path": "leak.md", "content": "append" }),
        json!({ "command": "write", "path": "escape/new.md", "content": "escaped" }),
    ] {
        let error = exec_tool(&registry, input).expect_err("symlink escape must be rejected");
        assert!(!error.is_empty(), "confinement error must be reported");
    }

    exec_tool(&registry, json!({ "command": "delete", "path": "leak.md" }))
        .expect("deleting a symlink unlinks the directory entry");
    assert!(!memories_dir.join("leak.md").exists());

    let listing = exec_tool(&registry, json!({ "command": "view" })).expect("listing succeeds");
    assert!(!listing.contains("leak.md"));
    assert!(!listing.contains("outside-secret"));
    let search = exec_tool(
        &registry,
        json!({ "command": "search", "query": "outside-secret" }),
    )
    .expect("search succeeds");
    assert!(search.contains("No matching memories"));
    assert_eq!(fs::read_to_string(&secret_path).unwrap(), "outside-secret");
    assert!(!outside.path().join("new.md").exists());
}
