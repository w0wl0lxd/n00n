#![allow(clippy::print_literal, clippy::uninlined_format_args)]

use std::sync::Arc;

use n00n_agent::AgentConfig;
use n00n_agent::prompt::{
    COMPACTION_SYSTEM, COMPACTION_USER, GENERAL_PROMPT, PLAN_PROMPT, RESEARCH_PROMPT, SYSTEM_PROMPT,
};
use n00n_agent::tokenize::{count_json_for_model, count_tokens_for_model};
use n00n_agent::{
    template::Vars,
    tools::{ActiveTools, DescriptionContext, ToolAudience, ToolFilter, ToolRegistry},
};
use n00n_providers::Model;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let registry = Arc::new(ToolRegistry::new());
    let _host = n00n_lua::PluginHost::with_all_builtins(Arc::clone(&registry))?;

    let vars = Vars::new()
        .set("{cwd}", "/tmp/n00n-token-profile")
        .set("{platform}", "linux")
        .set("{date}", "2026-07-27");
    let model = Model::from_spec("anthropic/claude-sonnet-4-6")?;

    let filter = ToolFilter::from_config(&AgentConfig::default(), &model, &[]);
    let active = ActiveTools::default();

    println!("Tool definition size by audience (cold-start filter, MCP excluded):");
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

    let mut main_per_tool: Vec<(String, usize)> = Vec::new();

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

        if *audience == ToolAudience::MAIN && !workflow {
            for def in defs.as_array().into_iter().flatten() {
                let name = def["name"].as_str().unwrap_or_else(|| "?").to_owned();
                let tool_tokens = count_json_for_model(&model.id, def);
                main_per_tool.push((name, tool_tokens));
            }
        }
    }

    main_per_tool.sort_by_key(|b| std::cmp::Reverse(b.1));
    println!();
    println!("Top tools by token cost (main audience):");
    println!("{:<22} Tokens (est)", "Tool");
    println!("{}", "-".repeat(34));
    for (name, tokens) in main_per_tool.iter().take(15) {
        println!("{name:<22} {tokens}");
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
    println!();
    println!("For CI gates use: cargo test -p n00n-token-profile");

    println!();
    println!("Prompt template size:");
    println!("{:<22} {:<15} {:<15}", "Prompt", "Bytes", "Tokens (est)");
    println!("{}", "-".repeat(52));

    let prompts = [
        ("system", SYSTEM_PROMPT),
        ("general", GENERAL_PROMPT),
        ("research", RESEARCH_PROMPT),
        ("plan", PLAN_PROMPT),
        ("compaction", COMPACTION_SYSTEM),
        ("compaction_user", COMPACTION_USER),
    ];
    for (label, text) in &prompts {
        let bytes = text.len();
        let tokens = count_tokens_for_model(&model.id, text);
        println!("{label:<22} {bytes:<15} {tokens:<15}");
    }

    Ok(())
}
