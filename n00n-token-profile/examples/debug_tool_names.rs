use std::sync::Arc;

use n00n_agent::AgentConfig;
use n00n_agent::template::Vars;
use n00n_agent::tools::{ActiveTools, DescriptionContext, ToolAudience, ToolFilter, ToolRegistry};
use n00n_providers::Model;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = Model::from_spec("anthropic/claude-sonnet-4-6")?;
    let registry = Arc::new(ToolRegistry::new());
    let _host = n00n_lua::PluginHost::with_all_builtins(Arc::clone(&registry))?;
    let vars = Vars::new();
    let filter = ToolFilter::from_config(&AgentConfig::default(), &model, &[]);
    let active = ActiveTools::default();
    let ctx = DescriptionContext {
        filter: &filter,
        audience: ToolAudience::MAIN,
        workflow: false,
    };
    let payload = registry.definitions_active(&vars, &ctx, model.supports_tool_examples(), &active);
    let mut names: Vec<String> = payload
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            d.get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("?")
                .to_string()
        })
        .collect();
    names.sort();
    println!("count={}", names.len());
    for n in &names {
        println!("{n}");
    }
    Ok(())
}
