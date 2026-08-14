//! Live tests against the real Devin API (`https://server.codeium.com`,
//! native Connect/gRPC-Web transport). `#[ignore]`d so a default `cargo test`/
//! `cargo nextest run` reports them as ignored, never as passed-without-running.
//! Opt in explicitly: `cargo nextest run -p n00n-providers --run-ignored ignored-only`
//! (or `cargo test -- --ignored`) with `DEVIN_API_KEY`/`WINDSURF_API_KEY` set, or
//! `~/.local/share/devin/credentials.toml` present. Without a credential each
//! still skips cleanly at runtime instead of failing; no fallback key is embedded.

use std::sync::Arc;

use n00n_providers::model::{Model, ModelFamily, ModelPricing, ModelTier};
use n00n_providers::provider::ProviderKind;
use n00n_providers::{
    CacheControl, ContentBlock, Message, ProviderEvent, RequestOptions, Role, System,
    SystemBlock, Timeouts,
};
use serde_json::json;

fn credentials_available() -> bool {
    std::env::var("DEVIN_API_KEY").is_ok()
        || std::env::var("WINDSURF_API_KEY").is_ok()
        || std::env::var("HOME")
            .ok()
            .map(|home| {
                std::path::Path::new(&home).join(".local/share/devin/credentials.toml")
            })
            .is_some_and(|path| path.exists())
}

macro_rules! skip_without_credentials {
    ($test_name:expr) => {
        if !credentials_available() {
            eprintln!(
                "SKIP {}: no Devin credentials found (set DEVIN_API_KEY or \
                 WINDSURF_API_KEY, or provide ~/.local/share/devin/credentials.toml)",
                $test_name
            );
            return;
        }
    };
}

fn devin_model(id: &str) -> Model {
    Model {
        id: id.to_string(),
        provider: Arc::from("devin"),
        tier: ModelTier::Medium,
        family: ModelFamily::Generic,
        supports_tool_examples_override: None,
        supports_thinking_override: None,
        supports_vision_override: None,
        supports_files_override: None,
        pricing: ModelPricing::default(),
        max_output_tokens: Some(8_192),
        context_window: 262_144,
        thinking_dialect: None,
        thinking_fields: None,
        body_override: None,
    }
}

fn user_message(text: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
        display_text: None,
        control: false,
    }
}

fn system_prompt(text: &str) -> System {
    let mut system = System::new();
    system.push(SystemBlock::new(text, CacheControl::None));
    system
}

/// Live model listing against the real Devin API.
#[test]
#[ignore = "needs a real Devin session token; run with --include-ignored / --run-ignored"]
fn devin_lists_models() {
    skip_without_credentials!("devin_lists_models");
    smol::block_on(async {
        let provider = ProviderKind::Devin
            .create(Timeouts::default())
            .expect("construct Devin provider");
        let models = provider
            .list_models()
            .await
            .expect("Devin list_models should succeed with valid credentials");
        assert!(
            !models.is_empty(),
            "Devin should report at least one model"
        );
        eprintln!(
            "devin_lists_models: {} models, first = {}",
            models.len(),
            models[0].id
        );
    });
}

/// Exercises a full completion end to end and checks the aggregated response
/// (the shape a non-streaming caller consumes: final content + stop reason).
#[test]
#[ignore = "needs a real Devin session token; run with --include-ignored / --run-ignored"]
fn devin_completion_aggregated_response() {
    skip_without_credentials!("devin_completion_aggregated_response");
    smol::block_on(async {
        let provider = ProviderKind::Devin
            .create(Timeouts::default())
            .expect("construct Devin provider");
        let (tx, _rx) = flume::unbounded();
        let model = devin_model("swe-1-7-lightning");
        let messages = [user_message(
            "Reply with exactly the single word: pong. No punctuation, no other text.",
        )];
        let system =
            system_prompt("You are a terse automated test harness. Follow instructions exactly.");
        let tools = json!([]);

        let result = provider
            .stream_message(
                &model,
                &messages,
                &system,
                &tools,
                &tx,
                RequestOptions::default(),
                None,
            )
            .await
            .expect("Devin stream_message should succeed with valid credentials");

        assert!(
            !result.message.content.is_empty(),
            "response should have at least one content block"
        );
        eprintln!(
            "devin_completion_aggregated_response: stop_reason={:?} content={:?}",
            result.stop_reason, result.message.content
        );
    });
}

/// Exercises the same call but observes the incremental `ProviderEvent`
/// stream (the shape a streaming caller consumes), checking that text
/// deltas arrive over the channel before the call completes.
#[test]
#[ignore = "needs a real Devin session token; run with --include-ignored / --run-ignored"]
fn devin_streaming_emits_text_deltas() {
    skip_without_credentials!("devin_streaming_emits_text_deltas");
    smol::block_on(async {
        let provider = ProviderKind::Devin
            .create(Timeouts::default())
            .expect("construct Devin provider");
        let (tx, rx) = flume::unbounded();
        let model = devin_model("swe-1-7-lightning");
        let messages = [user_message(
            "Count from one to five, one number per line.",
        )];
        let system = system_prompt("You are a terse automated test harness.");
        let tools = json!([]);

        let result = provider
            .stream_message(
                &model,
                &messages,
                &system,
                &tools,
                &tx,
                RequestOptions::default(),
                None,
            )
            .await
            .expect("Devin stream_message should succeed with valid credentials");
        drop(tx);

        let mut delta_count = 0usize;
        while let Ok(event) = rx.try_recv() {
            if let ProviderEvent::TextDelta { .. } = event {
                delta_count += 1;
            }
        }

        assert!(
            delta_count > 0,
            "expected at least one TextDelta event over the channel"
        );
        eprintln!(
            "devin_streaming_emits_text_deltas: {delta_count} text deltas, stop_reason={:?}",
            result.stop_reason
        );
    });
}

/// Gives Devin a tool with a required argument and asks it to call the tool,
/// checking the resulting `ToolUse` block round-trips through
/// `encode_devin_tools` / `ordered_tool_call_blocks` with parsed JSON input.
#[test]
#[ignore = "needs a real Devin session token; run with --include-ignored / --run-ignored"]
fn devin_tool_call_round_trips() {
    skip_without_credentials!("devin_tool_call_round_trips");
    smol::block_on(async {
        let provider = ProviderKind::Devin
            .create(Timeouts::default())
            .expect("construct Devin provider");
        let (tx, _rx) = flume::unbounded();
        let model = devin_model("swe-1-7-lightning");
        let messages = [user_message(
            "Call the `echo` tool exactly once with message set to \"hi\", then stop. \
             Do not reply with any other text.",
        )];
        let system = system_prompt(
            "You are a terse automated test harness with exactly one tool available: `echo`.",
        );
        let tools = json!([{
            "name": "echo",
            "description": "Echoes the given message back to the caller.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "message": {"type": "string"}
                },
                "required": ["message"]
            }
        }]);

        let result = provider
            .stream_message(
                &model,
                &messages,
                &system,
                &tools,
                &tx,
                RequestOptions::default(),
                None,
            )
            .await
            .expect("Devin stream_message should succeed with valid credentials");

        let tool_uses: Vec<_> = result
            .message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { id, name, input } => Some((id, name, input)),
                _ => None,
            })
            .collect();

        eprintln!(
            "devin_tool_call_round_trips: content={:?}",
            result.message.content
        );

        assert!(
            !tool_uses.is_empty(),
            "expected Devin to call the `echo` tool at least once; got content: {:?}",
            result.message.content
        );
        for (_, name, input) in &tool_uses {
            assert_eq!(*name, "echo", "unexpected tool name");
            assert!(
                input.is_object(),
                "tool input must decode to a JSON object, got {input:?}"
            );
        }
    });
}
