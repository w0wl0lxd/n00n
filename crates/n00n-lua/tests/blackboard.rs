#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::needless_pass_by_value
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

use n00n_agent::tools::ToolRegistry;
use n00n_agent::{AgentMode, ToolOutput};
use n00n_lua::PluginHost;
use serde_json::{Value, json};
use tempfile::TempDir;

const BLACKBOARD_SOURCE: &str = include_str!("../../plugins/blackboard/init.lua");

fn fixture_host(state_dir: &Path, agent_id: &str) -> (Arc<ToolRegistry>, PluginHost) {
    let registry = Arc::new(ToolRegistry::new());
    let host = PluginHost::new(Arc::clone(&registry)).unwrap();
    let state_dir = serde_json::to_string(&state_dir.to_string_lossy()).unwrap();
    let agent_id = serde_json::to_string(agent_id).unwrap();
    host.load_source(
        "blackboard_fixture",
        &format!(
            "n00n.env.state_dir = function() return {state_dir} end\n\
             n00n.session.current = function() return {agent_id} end\n\
             {BLACKBOARD_SOURCE}"
        ),
    )
    .unwrap();
    (registry, host)
}

fn exec_tool(registry: &ToolRegistry, input: Value) -> Result<String, String> {
    let entry = registry.get("blackboard").expect("blackboard registered");
    let invocation = entry.tool.parse(&input).expect("valid blackboard input");
    let context = n00n_agent::tools::test_support::stub_ctx(&AgentMode::Build);
    smol::block_on(invocation.execute(&context))
        .output
        .map(|output| match output {
            ToolOutput::Plain(output) | ToolOutput::Markdown(output) => output.text,
            other => panic!("unexpected blackboard output: {other:?}"),
        })
}

fn find_claim(root: &Path, task_id: &str) -> PathBuf {
    fn visit(dir: &Path, task_id: &str) -> Option<PathBuf> {
        for entry in fs::read_dir(dir).ok()? {
            let path = entry.ok()?.path();
            if path.file_name().is_some_and(|name| name == "claim.json")
                && path
                    .parent()
                    .and_then(Path::file_name)
                    .is_some_and(|name| name == task_id)
            {
                return Some(path);
            }
            if path.is_dir()
                && let Some(found) = visit(&path, task_id)
            {
                return Some(found);
            }
        }
        None
    }

    visit(root, task_id).expect("claim file created")
}

#[test]
fn blackboard_plugin_loads() {
    let registry = Arc::new(ToolRegistry::default());
    let host = PluginHost::with_all_builtins(Arc::clone(&registry)).unwrap();
    assert!(registry.get("blackboard").is_some());
    drop(host);
}

#[test]
fn exactly_one_host_reclaims_an_expired_task() {
    const TASK_ID: &str = "expired-task";
    const ROUNDS: usize = 12;

    let state = TempDir::new().unwrap();
    let (seed_registry, _seed_host) = fixture_host(state.path(), "seed-agent");
    exec_tool(
        &seed_registry,
        json!({ "action": "claim_task", "claim": { "task_id": TASK_ID, "expires_in": 60 } }),
    )
    .expect("seed claim succeeds");
    let claim_path = find_claim(state.path(), TASK_ID);

    let (first_registry, _first_host) = fixture_host(state.path(), "first-agent");
    let (second_registry, _second_host) = fixture_host(state.path(), "second-agent");

    for _ in 0..ROUNDS {
        fs::write(
            &claim_path,
            format!(
                r#"{{"task_id":"{TASK_ID}","agent_id":"seed-agent","claimed_at":0,"expires_at":0,"status":"claimed"}}"#
            ),
        )
        .unwrap();

        let barrier = Arc::new(Barrier::new(3));
        let input =
            json!({ "action": "claim_task", "claim": { "task_id": TASK_ID, "expires_in": 60 } });
        let (first, second) = std::thread::scope(|scope| {
            let first_barrier = Arc::clone(&barrier);
            let first_input = input.clone();
            let first_registry = &first_registry;
            let first = scope.spawn(move || {
                first_barrier.wait();
                exec_tool(first_registry, first_input)
            });
            let second_barrier = Arc::clone(&barrier);
            let second_registry = &second_registry;
            let second = scope.spawn(move || {
                second_barrier.wait();
                exec_tool(second_registry, input)
            });
            barrier.wait();
            (first.join().unwrap(), second.join().unwrap())
        });

        assert_eq!(
            usize::from(first.is_ok()) + usize::from(second.is_ok()),
            1,
            "exactly one independent Lua host must reclaim an expired task: first={first:?}, second={second:?}"
        );
    }
}
