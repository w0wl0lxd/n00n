// Integration tests for the blackboard plugin.
use std::sync::Arc;

use n00n_agent::tools::ToolRegistry;
use n00n_lua::PluginHost;

#[test]
fn blackboard_plugin_loads() {
    let registry = Arc::new(ToolRegistry::default());
    let host = PluginHost::with_all_builtins(Arc::clone(&registry)).unwrap();
    drop(host);
}
