use std::sync::Arc;

use n00n_agent::template::Vars;
use n00n_agent::tools::{ActiveTools, DescriptionContext, ToolAudience, ToolFilter, ToolRegistry};
use n00n_providers::Model;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let registry = ToolRegistry::global_arc();
    let _host = n00n_lua::PluginHost::with_all_builtins(Arc::clone(registry))?;

    let vars = Vars::new();
    let model = Model::from_spec("anthropic/claude-sonnet-4-6")?;

    let filter = ToolFilter::All;
    let ctx = DescriptionContext {
        filter: &filter,
        audience: ToolAudience::MAIN,
        workflow: false,
    };

    let tool_modes = ["default", "research", "build", "compact"];

    println!("Tool definition size by mode:");
    println!("{:<15} {:<15} {:<15}", "Mode", "Tool Count", "Bytes");
    println!("{}", "-".repeat(45));

    for mode in &tool_modes {
        let active = ActiveTools::default();
        let defs =
            registry.definitions_active(&vars, &ctx, model.supports_tool_examples(), &active);
        let bytes = serde_json::to_vec(&defs)?.len();
        let count = defs.as_array().map_or(0, std::vec::Vec::len);

        println!("{mode:<15} {count:<15} {bytes:<15}");
    }

    let all_defs = registry.definitions(&vars, &ctx, model.supports_tool_examples());
    let all_bytes = serde_json::to_vec(&all_defs)?.len();
    let all_count = registry.names().len();

    println!(
        "{:<15} {:<15} {:<15}",
        "all (unfiltered)", all_count, all_bytes
    );

    Ok(())
}
