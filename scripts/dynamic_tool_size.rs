use std::sync::Arc;

use n00n_agent::tokenize::count_json_for_model;
use n00n_agent::{
    template::Vars,
    tools::{ActiveTools, DescriptionContext, ToolAudience, ToolFilter, ToolRegistry},
};
use n00n_providers::Model;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let registry = ToolRegistry::global_arc();
    let _host = n00n_lua::PluginHost::with_all_builtins(Arc::clone(registry))?;

    let vars = Vars::new();
    let model = Model::from_spec("anthropic/claude-sonnet-4-6")?;

    let filter = ToolFilter::All;
    let active = ActiveTools::default();

    println!("Tool definition size by audience:");
    println!(
        "{:<18} {:<15} {:<15} {:<15}",
        "Audience", "Tool Count", "Bytes", "Tokens (est)"
    );
    println!("{}", "-".repeat(63));

    let audiences = [
        ("main", ToolAudience::MAIN, false),
        ("research_sub", ToolAudience::RESEARCH_SUB, false),
        ("general_sub", ToolAudience::GENERAL_SUB, false),
        ("interpreter", ToolAudience::INTERPRETER, false),
        ("workflow", ToolAudience::WORKFLOW, true),
    ];

    for (label, audience, workflow) in &audiences {
        let ctx = DescriptionContext {
            filter: &filter,
            audience: *audience,
            workflow: *workflow,
        };
        let defs =
            registry.definitions_active(&vars, &ctx, model.supports_tool_examples(), &active);
        let bytes = serde_json::to_vec(&defs)?.len();
        let tokens = count_json_for_model(&model.id, &defs);
        let count = defs.as_array().map_or(0, std::vec::Vec::len);

        println!("{label:<18} {count:<15} {bytes:<15} {tokens:<15}");
    }

    let ctx = DescriptionContext {
        filter: &filter,
        audience: ToolAudience::MAIN,
        workflow: false,
    };
    let all_defs = registry.definitions(&vars, &ctx, model.supports_tool_examples());
    let all_bytes = serde_json::to_vec(&all_defs)?.len();
    let all_tokens = count_json_for_model(&model.id, &all_defs);
    let all_count = registry.names().len();

    println!(
        "{:<18} {:<15} {:<15} {:<15}",
        "all (unfiltered)", all_count, all_bytes, all_tokens
    );

    Ok(())
}
