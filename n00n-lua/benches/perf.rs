//! Manual performance harness (run with `cargo bench -p n00n-lua --bench perf`).
//!
//! Covers the two data-path claims introduced by the axon->n00n port:
//!   * PR-C: TOON encoding shrinks structured tool output vs JSON.
//!   * PR-B: `TokenUsage::cost` prices completions from provider pricing.

use n00n_providers::model::{Model, ModelPricing, TokenUsage};
use serde_json::json;

fn main() {
    // (1) TOON vs JSON size on a representative tool-output payload (PR-C).
    let payload = json!({
        "files": (0..50)
            .map(|i| json!({
                "path": format!("src/module_{i}/mod.rs"),
                "lines": 20 + i,
                "summary": "implements a helper that does something useful and is covered by tests",
            }))
            .collect::<Vec<_>>(),
    });
    let json = serde_json::to_string(&payload).expect("json");
    let toon = toon_format::encode_default(&payload).expect("toon");
    let saved = 100.0 * (1.0 - toon.len() as f64 / json.len() as f64);
    println!(
        "payload: json={}B toon={}B saved={:.1}%",
        json.len(),
        toon.len(),
        saved
    );

    // (2) TokenUsage::cost throughput + a sample cost (PR-B).
    let pricing = ModelPricing {
        input: 3.00,
        output: 15.00,
        cache_write: 3.75,
        cache_read: 0.30,
        fast: None,
    };
    let usage = TokenUsage {
        input: 1_000_000,
        output: 100_000,
        cache_creation: 0,
        cache_read: 0,
    };
    let cost = usage.cost(&pricing, false);
    println!("sample cost (1M in / 100k out @ $3/$15): ${:.4}", cost);

    // (3) Resolved-model cost for a known spec.
    if let Ok(m) = Model::from_spec("anthropic/claude-3-5-haiku-20241022") {
        let u = TokenUsage {
            input: 10_000,
            output: 2_000,
            cache_creation: 0,
            cache_read: 0,
        };
        println!(
            "haiku 10k/2k cost: ${:.6}",
            u.cost(&m.pricing, m.supports_fast())
        );
    }

    println!("bench ok");
}
