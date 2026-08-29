pub mod auth;
mod platform;
pub(crate) mod responses;
pub(crate) mod websocket;

pub(crate) use platform::CODING_PLAN_CONTEXT_WINDOW;
pub use platform::{CodexCacheCapabilities, OpenAi, OpenAiOptions};

use std::cmp::Reverse;

use crate::model::{ModelEntry, ModelFamily, ModelInfo, ModelPricing, ModelTier, lookup_entry};

pub(crate) const OPENAI_API_BASE_URL: &str = "https://api.openai.com/v1";
const GPT_5_6_MAX_OUTPUT_TOKENS: u32 = 128_000;
const GPT_5_6_CONTEXT_WINDOW: u32 = 372_000;

const WEAK_PLAN_PRICING: ModelPricing = ModelPricing {
    input: 1.00,
    output: 6.00,
    cache_write: 1.25,
    cache_read: 0.10,
    fast: None,
};

const MEDIUM_PLAN_PRICING: ModelPricing = ModelPricing {
    input: 2.50,
    output: 15.00,
    cache_write: 3.125,
    cache_read: 0.25,
    fast: None,
};

const STRONG_PLAN_PRICING: ModelPricing = ModelPricing {
    input: 5.00,
    output: 30.00,
    cache_write: 6.25,
    cache_read: 0.50,
    fast: None,
};

inventory::submit!(n00n_config::providers::BuiltInProvider {
    slug: "openai",
    display_name: "OpenAI",
    protocol: n00n_config::providers::Protocol::Openai,
    default_base_url: OPENAI_API_BASE_URL,
    default_api_key_env: "OPENAI_API_KEY",
    default_model: "openai/gpt-5.5",
    plans: None,
    login_url: Some("https://platform.openai.com/api-keys"),
    needs_url: false,
});

inventory::submit!(n00n_config::providers::BuiltInProvider {
    slug: "codex",
    display_name: "Codex",
    protocol: n00n_config::providers::Protocol::Openai,
    default_base_url: "https://chatgpt.com/backend-api/codex",
    default_api_key_env: "",
    default_model: "codex/gpt-5.6-luna",
    plans: None,
    login_url: None,
    needs_url: false,
});

const fn model_entry(
    prefixes: &'static [&'static str],
    tier: ModelTier,
    vision: bool,
    default: bool,
    max_output_tokens: u32,
    context_window: u32,
    pricing: ModelPricing,
) -> ModelEntry {
    ModelEntry {
        prefixes,
        tier,
        family: ModelFamily::Gpt,
        vision,
        files: false,
        default,
        max_output_tokens,
        context_window,
        pricing,
    }
}

const fn with_files(entry: ModelEntry) -> ModelEntry {
    ModelEntry {
        files: true,
        ..entry
    }
}

const fn coding_plan_pricing(tier: ModelTier) -> ModelPricing {
    match tier {
        ModelTier::Weak => WEAK_PLAN_PRICING,
        ModelTier::Medium => MEDIUM_PLAN_PRICING,
        ModelTier::Strong => STRONG_PLAN_PRICING,
        ModelTier::Compaction => ModelPricing::ZERO,
    }
}

const fn with_coding_plan(
    entry: ModelEntry,
    prefixes: &'static [&'static str],
    max_output_tokens: u32,
    context_window: u32,
    vision: bool,
    default: bool,
) -> ModelEntry {
    ModelEntry {
        prefixes,
        max_output_tokens,
        context_window,
        vision,
        files: false,
        default,
        pricing: coding_plan_pricing(entry.tier),
        ..entry
    }
}

const OPENAI_GPT_5_6_LUNA: ModelEntry = with_files(model_entry(
    &["gpt-5.6-luna"],
    ModelTier::Weak,
    true,
    true,
    GPT_5_6_MAX_OUTPUT_TOKENS,
    GPT_5_6_CONTEXT_WINDOW,
    ModelPricing {
        input: 1.00,
        output: 6.00,
        cache_write: 1.25,
        cache_read: 0.10,
        fast: None,
    },
));

const OPENAI_GPT_5_6_TERRA: ModelEntry = with_files(model_entry(
    &["gpt-5.6-terra"],
    ModelTier::Medium,
    true,
    true,
    GPT_5_6_MAX_OUTPUT_TOKENS,
    GPT_5_6_CONTEXT_WINDOW,
    ModelPricing {
        input: 2.50,
        output: 15.00,
        cache_write: 3.125,
        cache_read: 0.25,
        fast: None,
    },
));

const OPENAI_GPT_5_6_SOL: ModelEntry = with_files(model_entry(
    &["gpt-5.6-sol", "gpt-5.6"],
    ModelTier::Strong,
    true,
    true,
    GPT_5_6_MAX_OUTPUT_TOKENS,
    GPT_5_6_CONTEXT_WINDOW,
    ModelPricing {
        input: 5.00,
        output: 30.00,
        cache_write: 6.25,
        cache_read: 0.50,
        fast: Some(crate::model::FastPricing {
            input: 10.00,
            output: 60.00,
        }),
    },
));

const OPENAI_GPT_5_4_NANO: ModelEntry = model_entry(
    &["gpt-5.4-nano"],
    ModelTier::Weak,
    true,
    false,
    128_000,
    400_000,
    ModelPricing {
        input: 0.20,
        output: 1.25,
        cache_write: 0.00,
        cache_read: 0.02,
        fast: None,
    },
);

const OPENAI_GPT_5_4_MINI: ModelEntry = model_entry(
    &["gpt-5.4-mini"],
    ModelTier::Weak,
    true,
    false,
    128_000,
    400_000,
    ModelPricing {
        input: 0.75,
        output: 4.50,
        cache_write: 0.00,
        cache_read: 0.075,
        fast: None,
    },
);

const OPENAI_GPT_4_1_NANO: ModelEntry = model_entry(
    &["gpt-4.1-nano"],
    ModelTier::Weak,
    true,
    false,
    32_768,
    1_047_576,
    ModelPricing {
        input: 0.10,
        output: 0.40,
        cache_write: 0.00,
        cache_read: 0.025,
        fast: None,
    },
);

const OPENAI_GPT_4_1_MINI: ModelEntry = model_entry(
    &["gpt-4.1-mini"],
    ModelTier::Medium,
    true,
    false,
    32_768,
    1_047_576,
    ModelPricing {
        input: 0.40,
        output: 1.60,
        cache_write: 0.00,
        cache_read: 0.10,
        fast: None,
    },
);

const OPENAI_GPT_4_1: ModelEntry = model_entry(
    &["gpt-4.1"],
    ModelTier::Medium,
    true,
    false,
    32_768,
    1_047_576,
    ModelPricing {
        input: 2.00,
        output: 8.00,
        cache_write: 0.00,
        cache_read: 0.50,
        fast: None,
    },
);

const OPENAI_O4_MINI: ModelEntry = model_entry(
    &["o4-mini"],
    ModelTier::Medium,
    true,
    false,
    100_000,
    200_000,
    ModelPricing {
        input: 1.10,
        output: 4.40,
        cache_write: 0.00,
        cache_read: 0.275,
        fast: None,
    },
);

const OPENAI_GPT_5_5: ModelEntry = model_entry(
    &["gpt-5.5"],
    ModelTier::Strong,
    true,
    false,
    128_000,
    1_050_000,
    ModelPricing {
        input: 5.00,
        output: 30.00,
        cache_write: 0.00,
        cache_read: 0.50,
        fast: None,
    },
);

const OPENAI_GPT_5_5_PRO: ModelEntry = model_entry(
    &["gpt-5.5-pro"],
    ModelTier::Strong,
    true,
    false,
    128_000,
    1_050_000,
    ModelPricing {
        input: 7.50,
        output: 45.00,
        cache_write: 0.00,
        cache_read: 0.75,
        fast: None,
    },
);

const OPENAI_GPT_5_4: ModelEntry = model_entry(
    &["gpt-5.4"],
    ModelTier::Strong,
    true,
    false,
    128_000,
    1_050_000,
    ModelPricing {
        input: 2.50,
        output: 15.00,
        cache_write: 0.00,
        cache_read: 0.25,
        fast: None,
    },
);

const OPENAI_O3: ModelEntry = model_entry(
    &["o3"],
    ModelTier::Strong,
    true,
    false,
    100_000,
    200_000,
    ModelPricing {
        input: 2.00,
        output: 8.00,
        cache_write: 0.00,
        cache_read: 1.00,
        fast: None,
    },
);

#[allow(clippy::too_many_lines)]
pub(crate) const fn models() -> &'static [ModelEntry] {
    &[
        OPENAI_GPT_5_6_LUNA,
        OPENAI_GPT_5_6_TERRA,
        OPENAI_GPT_5_6_SOL,
        OPENAI_GPT_5_4_NANO,
        OPENAI_GPT_5_4_MINI,
        OPENAI_GPT_4_1_NANO,
        OPENAI_GPT_4_1_MINI,
        OPENAI_GPT_4_1,
        OPENAI_O4_MINI,
        OPENAI_GPT_5_5,
        OPENAI_GPT_5_5_PRO,
        OPENAI_GPT_5_4,
        OPENAI_O3,
    ]
}

#[allow(clippy::too_many_lines)]
pub(crate) const fn codex_models() -> &'static [ModelEntry] {
    const CODEX_MODELS: &[ModelEntry] = &[
        with_coding_plan(
            OPENAI_GPT_5_6_LUNA,
            &["gpt-5.6-luna"],
            GPT_5_6_MAX_OUTPUT_TOKENS,
            CODING_PLAN_CONTEXT_WINDOW,
            true,
            true,
        ),
        with_coding_plan(
            OPENAI_GPT_5_4_NANO,
            &["gpt-5.4-nano"],
            128_000,
            CODING_PLAN_CONTEXT_WINDOW,
            true,
            false,
        ),
        with_coding_plan(
            OPENAI_GPT_5_4_MINI,
            &["gpt-5.4-mini"],
            128_000,
            CODING_PLAN_CONTEXT_WINDOW,
            true,
            false,
        ),
        with_coding_plan(
            OPENAI_GPT_4_1_NANO,
            &["gpt-4.1-nano"],
            32_768,
            CODING_PLAN_CONTEXT_WINDOW,
            true,
            false,
        ),
        with_coding_plan(
            OPENAI_GPT_5_6_TERRA,
            &["gpt-5.6-terra"],
            GPT_5_6_MAX_OUTPUT_TOKENS,
            CODING_PLAN_CONTEXT_WINDOW,
            true,
            true,
        ),
        with_coding_plan(
            OPENAI_GPT_4_1_MINI,
            &["gpt-4.1-mini"],
            32_768,
            CODING_PLAN_CONTEXT_WINDOW,
            true,
            false,
        ),
        with_coding_plan(
            OPENAI_GPT_4_1,
            &["gpt-4.1"],
            32_768,
            CODING_PLAN_CONTEXT_WINDOW,
            true,
            false,
        ),
        with_coding_plan(
            OPENAI_O4_MINI,
            &["o4-mini"],
            100_000,
            CODING_PLAN_CONTEXT_WINDOW,
            true,
            false,
        ),
        with_coding_plan(
            OPENAI_GPT_5_6_TERRA,
            &["gpt-5.1-codex-mini"],
            GPT_5_6_MAX_OUTPUT_TOKENS,
            CODING_PLAN_CONTEXT_WINDOW,
            true,
            false,
        ),
        with_coding_plan(
            OPENAI_GPT_5_6_SOL,
            &["gpt-5.6-sol"],
            GPT_5_6_MAX_OUTPUT_TOKENS,
            CODING_PLAN_CONTEXT_WINDOW,
            true,
            true,
        ),
        with_coding_plan(
            OPENAI_GPT_5_5,
            &["gpt-5.5"],
            128_000,
            CODING_PLAN_CONTEXT_WINDOW,
            true,
            false,
        ),
        ModelEntry {
            pricing: OPENAI_GPT_5_5_PRO.pricing,
            ..with_coding_plan(
                OPENAI_GPT_5_5_PRO,
                &["gpt-5.5-pro"],
                128_000,
                CODING_PLAN_CONTEXT_WINDOW,
                true,
                false,
            )
        },
        with_coding_plan(
            OPENAI_GPT_5_4,
            &["gpt-5.4"],
            128_000,
            CODING_PLAN_CONTEXT_WINDOW,
            true,
            false,
        ),
        with_coding_plan(
            OPENAI_O3,
            &["o3"],
            100_000,
            CODING_PLAN_CONTEXT_WINDOW,
            true,
            false,
        ),
        with_coding_plan(
            OPENAI_GPT_5_6_SOL,
            &["gpt-5.3-codex-spark"],
            32_000,
            128_000,
            false,
            false,
        ),
        with_coding_plan(
            OPENAI_GPT_5_6_SOL,
            &["gpt-5.3-codex"],
            GPT_5_6_MAX_OUTPUT_TOKENS,
            CODING_PLAN_CONTEXT_WINDOW,
            true,
            false,
        ),
        with_coding_plan(
            OPENAI_GPT_5_6_SOL,
            &["gpt-5.2-codex"],
            GPT_5_6_MAX_OUTPUT_TOKENS,
            CODING_PLAN_CONTEXT_WINDOW,
            true,
            false,
        ),
        with_coding_plan(
            OPENAI_GPT_5_6_SOL,
            &["gpt-5.1-codex-max"],
            GPT_5_6_MAX_OUTPUT_TOKENS,
            CODING_PLAN_CONTEXT_WINDOW,
            true,
            false,
        ),
        with_coding_plan(
            OPENAI_GPT_5_6_SOL,
            &["gpt-5.1-codex"],
            GPT_5_6_MAX_OUTPUT_TOKENS,
            CODING_PLAN_CONTEXT_WINDOW,
            true,
            false,
        ),
        with_coding_plan(
            OPENAI_GPT_5_4_MINI,
            &["gpt-5.2"],
            128_000,
            CODING_PLAN_CONTEXT_WINDOW,
            true,
            false,
        ),
    ];

    CODEX_MODELS
}

fn tier_strength(tier: Option<ModelTier>) -> u8 {
    tier.map_or(0, |t| match t {
        ModelTier::Strong => 3,
        ModelTier::Medium => 2,
        ModelTier::Weak => 1,
        ModelTier::Compaction => 0,
    })
}

fn parse_model_version(id: &str) -> (u32, u32) {
    fn take_digits(s: &str) -> Option<(u32, &str)> {
        let end = s
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or_else(|| s.len());
        if end == 0 {
            return None;
        }
        let n = s[..end].parse::<u32>().ok()?;
        Some((n, &s[end..]))
    }

    let start = id
        .find(|c: char| c.is_ascii_digit())
        .unwrap_or_else(|| id.len());
    let (major, tail) = take_digits(&id[start..]).unwrap_or_else(|| (0, ""));
    if let Some(tail) = tail.strip_prefix('.') {
        let (minor, _) = take_digits(tail).unwrap_or_else(|| (0, ""));
        (major, minor)
    } else {
        (major, 0)
    }
}

pub(crate) fn sort_models(models: &mut [ModelInfo], entries: &[ModelEntry]) {
    fn key<'a>(
        info: &'a ModelInfo,
        entries: &[ModelEntry],
    ) -> (
        Reverse<bool>,
        Reverse<u32>,
        Reverse<u32>,
        Reverse<u8>,
        &'a str,
    ) {
        let (is_known, tier) = match lookup_entry(entries, &info.id) {
            Ok(entry) => (true, Some(entry.tier)),
            Err(_) => (false, None),
        };
        let (major, minor) = parse_model_version(&info.id);
        (
            Reverse(is_known),
            Reverse(major),
            Reverse(minor),
            Reverse(tier_strength(tier)),
            info.id.as_str(),
        )
    }

    models.sort_by(|a, b| key(a, entries).cmp(&key(b, entries)));
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;

    #[test]
    fn bare_codex_resolves_to_a_valid_qualified_spec() {
        let default = n00n_config::providers::resolve_default_model("codex", None)
            .expect("Codex should have a default model");
        let model =
            crate::model::Model::from_spec(&crate::model_registry::test_registry(), "codex")
                .expect("bare Codex should resolve to its default model");

        assert_eq!(model.spec(), default);
        assert!(default.starts_with("codex/"));
    }

    #[test]
    fn codex_unknown_model_remains_accepted() {
        assert!(
            crate::model::Model::from_spec(
                &crate::model_registry::test_registry(),
                "codex/unknown-id"
            )
            .is_ok()
        );
    }

    #[test_case("gpt-5.6-luna", ModelTier::Weak, 1.0, 0.1, 1.25, 6.0)]
    #[test_case("gpt-5.6-terra", ModelTier::Medium, 2.5, 0.25, 3.125, 15.0)]
    #[test_case("gpt-5.6-sol", ModelTier::Strong, 5.0, 0.5, 6.25, 30.0)]
    #[allow(clippy::float_cmp)]
    fn openai_gpt_5_6_models_have_full_context_and_pricing(
        model_id: &str,
        tier: ModelTier,
        input: f64,
        cache_read: f64,
        cache_write: f64,
        output: f64,
    ) {
        let model = models()
            .iter()
            .find(|model| model.prefixes.contains(&model_id))
            .expect("GPT-5.6 model should be registered in openai catalog");

        assert_eq!(model.tier, tier);
        assert_eq!(model.context_window, GPT_5_6_CONTEXT_WINDOW);
        assert_eq!(model.pricing.input, input);
        assert_eq!(model.pricing.cache_read, cache_read);
        assert_eq!(model.pricing.cache_write, cache_write);
        assert_eq!(model.pricing.output, output);
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn gpt_5_6_sol_has_fast_pricing() {
        let model = models()
            .iter()
            .find(|model| model.prefixes.contains(&"gpt-5.6-sol"))
            .expect("GPT-5.6-sol should be registered");
        let fast = model
            .pricing
            .fast
            .expect("GPT-5.6-sol should have fast pricing");
        assert_eq!(fast.input, 10.0);
        assert_eq!(fast.output, 60.0);
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn gpt_5_6_alias_resolves_to_sol() {
        let model = models()
            .iter()
            .find(|model| model.prefixes.contains(&"gpt-5.6"))
            .expect("gpt-5.6 alias should be registered");
        assert_eq!(model.tier, ModelTier::Strong);
        assert_eq!(model.pricing.input, 5.0);
        assert_eq!(model.pricing.output, 30.0);
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn gpt_5_5_pro_is_registered() {
        let model = models()
            .iter()
            .find(|model| model.prefixes.contains(&"gpt-5.5-pro"))
            .expect("gpt-5.5-pro should be registered");
        assert_eq!(model.tier, ModelTier::Strong);
        assert_eq!(model.context_window, 1_050_000);
        assert_eq!(model.pricing.input, 7.5);
        assert_eq!(model.pricing.output, 45.0);

        let codex_model = codex_models()
            .iter()
            .find(|model| model.prefixes.contains(&"gpt-5.5-pro"))
            .expect("gpt-5.5-pro should be registered in Codex catalog");
        assert_eq!(codex_model.context_window, CODING_PLAN_CONTEXT_WINDOW);
        assert_eq!(codex_model.pricing.input, 7.5);
        assert_eq!(codex_model.pricing.output, 45.0);
    }

    #[test_case("gpt-5.6-luna", ModelTier::Weak, 1.0, 0.1, 1.25, 6.0)]
    #[test_case("gpt-5.6-terra", ModelTier::Medium, 2.5, 0.25, 3.125, 15.0)]
    #[test_case("gpt-5.6-sol", ModelTier::Strong, 5.0, 0.5, 6.25, 30.0)]
    #[allow(clippy::float_cmp)]
    fn codex_gpt_5_6_models_have_plan_context_and_pricing(
        model_id: &str,
        tier: ModelTier,
        input: f64,
        cache_read: f64,
        cache_write: f64,
        output: f64,
    ) {
        let model = codex_models()
            .iter()
            .find(|model| model.prefixes.contains(&model_id))
            .expect("GPT-5.6 model should be registered in codex catalog");

        assert_eq!(model.tier, tier);
        assert_eq!(model.context_window, CODING_PLAN_CONTEXT_WINDOW);
        assert_eq!(model.pricing.input, input);
        assert_eq!(model.pricing.cache_read, cache_read);
        assert_eq!(model.pricing.cache_write, cache_write);
        assert_eq!(model.pricing.output, output);
    }

    #[test]
    fn chat_models_sort_ahead_of_dated_non_chat_models() {
        let mut listed = vec![
            ModelInfo::id_only("omni-moderation-2024-09-26".into()),
            ModelInfo::id_only("gpt-5.6-sol".into()),
        ];

        sort_models(&mut listed, models());

        assert_eq!(listed[0].id, "gpt-5.6-sol");
    }

    #[test]
    fn chat_models_sort_by_version_then_tier() {
        let mut listed = vec![
            ModelInfo::id_only("gpt-5.4".into()),
            ModelInfo::id_only("gpt-5.6-luna".into()),
            ModelInfo::id_only("gpt-5.6-sol".into()),
        ];

        sort_models(&mut listed, models());

        assert_eq!(
            listed
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["gpt-5.6-sol", "gpt-5.6-luna", "gpt-5.4"]
        );
    }

    #[test]
    fn codex_models_exclude_openai_only_model_ids() {
        assert!(
            codex_models()
                .iter()
                .any(|e| e.prefixes.contains(&"gpt-5.3-codex"))
        );
        assert!(
            models()
                .iter()
                .all(|e| !e.prefixes.iter().any(|p| p.contains("-codex")))
        );
    }
}
