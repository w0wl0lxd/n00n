use n00n_providers::manifest::ManifestRegistry;
use n00n_providers::model::{ModelEntry, ModelTier};
use n00n_providers::provider::ProviderKind;
use std::fmt::Write;
use strum::IntoEnumIterator;
use thiserror::Error;

const GPT_6_ASTRA: &str = "gpt-6-astra";
const GPT_5_5: &str = "gpt-5.5";
const GPT_5_5_PRO: &str = "gpt-5.5-pro";
const GPT_5_6_ALIAS: &str = "gpt-5.6";
const FRONT_MATTER: &str = r#"+++
title = "Providers"
weight = 5
[extra]
group = "Reference"
+++"#;

const TIER_PICKER_NOTE: &str = r"Open the model picker with `/model` and press `!`, `@`, `#`, or `$` on any row to assign it to strong, medium, weak, or compaction. Press the same key again to remove the assignment. Your overrides are saved to `~/.local/state/n00n/model-tiers` and apply across sessions.";

const AUTH_RELOADING: &str = r"## Auth Reloading

n00n re-reads auth from storage and environment variables each time a new agent spawns (`/new`, retry, session load). If you run `n00n auth login` in another terminal or change an env var, the next session picks it up without a restart.

You can set multiple API keys in one env var (`ANTHROPIC_API_KEY=sk-1,sk-2,sk-3`) and they rotate automatically on rate-limit or auth errors.";

const BASE_URL_OVERRIDES: &str = r"## Base URL Overrides

Every provider honors a `<SLUG>_BASE_URL` env var (`anthropic` -> `ANTHROPIC_BASE_URL`, `llama-cpp` -> `LLAMA_CPP_BASE_URL`). Set it to the origin of a proxy or a compatible endpoint and n00n appends the API paths itself:

```sh
ANTHROPIC_BASE_URL=https://my-proxy.internal n00n
```

It wins over `providers.toml` and built-in defaults. `ANTHROPIC_BASE_URL` and `OPENAI_BASE_URL` are the same names the official SDKs use, so an existing proxy setup carries over as is. One exception: `OPENAI_BASE_URL` only redirects the platform API, never the ChatGPT Coding Plan backend.";

const LONG_CONTEXT_NOTE: &str = r"Add `-1m` to any Claude model, like `claude-sonnet-4-6-1m`, to use the 1M token context window.";

const BEDROCK_NOTE: &str = r"#### Amazon Bedrock

If you already use Claude through AWS Bedrock, you can point n00n at it instead of the direct Anthropic API. Set `CLAUDE_CODE_USE_BEDROCK=1` and n00n will route all Anthropic requests through Bedrock. The same models, the same features, just a different door.

You will need `AWS_REGION` and one of the following for auth:

| Method | Env vars |
|--------|----------|
| IAM credentials | `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY` (and optionally `AWS_SESSION_TOKEN`) |
| Credentials file | `AWS_PROFILE` (defaults to `default`), reads `~/.aws/credentials` |
| Bearer token | `AWS_BEARER_TOKEN_BEDROCK` |
| Gateway proxy | `CLAUDE_CODE_SKIP_BEDROCK_AUTH=1` + `ANTHROPIC_BEDROCK_BASE_URL` (skips signing, useful behind a proxy that handles auth) |

You can override the model with `ANTHROPIC_MODEL` and the endpoint with `ANTHROPIC_BEDROCK_BASE_URL`. These env var names match Claude Code, so if you were already using Bedrock there, the same setup works here.";

const DEVIN_ACCOUNTS_NOTE: &str = r#"The primary account uses normal model specs such as `devin/swe-1-7-max`. Configure additional named accounts with either stored credentials:

```bash
n00n auth login devin@work
n00n -m devin/work::swe-1-7-max
```

or an existing Devin CLI credential file:

```toml
[devin.accounts.work]
display_name = "Work"
credential_path = "~/.local/share/devin-work/devin/credentials.toml"
```

Use `n00n auth status` to list accounts and `n00n auth logout devin@work` to remove one. Account names must already be lowercase slugs such as `work` or `team-prod`. The `account::model` form is reserved for explicit account routing, and unknown accounts fail closed instead of using primary credentials. A legacy numeric provider alias such as `devin2/model` is read as `devin/2::model` only when `[devin2]` is explicitly configured with `protocol = "devin"`; new configuration should use `[devin.accounts.<name>]` instead of separate provider definitions."#;

const OPENCODE_GO_NOTE: &str = r"OpenCode Go (`https://opencode.ai/zen/go/v1`) is a separate models.dev catalog entry with its own curated model set, including `muse-spark-1.2-contributor` (the Go-only contributor variant of `muse-spark-1.2`), `gpt-5.3-codex-spark`, and Go-exclusive `qwen3.7-*`/`qwen3.8-max` models; select it with a spec like `opencode/opencode-go/<model>`, or `opencode/opencode-go` for the default model. The main Zen catalog (`https://opencode.ai/zen/v1`) includes `muse-spark-1.2` and `opencode-go` as selectable model prefixes.";

const OPENCODE_FREE_MODELS_NOTE: &str = r"By default n00n hides free models from the Opencode catalog. To list free models (they use a public fallback, no API key needed), add this to `~/.config/n00n/providers.toml`:

```toml
[opencode]
enable_free_models = true
```

The default is `false`.";

const MODEL_IDENTIFIERS: &str = r"## Model Identifiers

Models are referenced as `provider/model_id`:

```
anthropic/claude-sonnet-4-6
openai/gpt-4.1
zai/glm-4.7
```

If the model name is unique across providers, the prefix can be omitted.";

fn dynamic_providers_section() -> String {
    let valid_values: Vec<String> = ProviderKind::iter().map(|k| format!("`{k}`")).collect();

    format!(
        r#"## Dynamic Providers

To add a custom provider or proxy, drop an executable script into `~/.config/n00n/providers/`. The script must handle these subcommands:

| Subcommand | Timeout | What it does |
|------------|---------|--------|
| `info` | 5s | Return JSON with `display_name`, `base` provider, `has_auth` |
| `models` | 5s | Return JSON array of model entries (optional) |
| `resolve` | 30s | Return auth JSON (`base_url`, `headers`) |
| `login` | interactive | OAuth or credential flow |
| `logout` | interactive | Clear credentials |
| `refresh` | 30s | Refresh auth tokens |

`resolve` is called each time a new agent spawns, so scripts should read tokens from disk instead of caching them in memory. That way auth changes from other processes get picked up.

The `base` field specifies which built-in provider to inherit the model catalog from. Valid values: {}.

If your provider serves models not in the base catalog, add a `models` subcommand returning:

```json
[{{"id": "my-model-v2", "tier": "strong", "context_window": 200000, "max_output_tokens": 16384}}]
```

Only `id` is required. Optional fields: `tier` (default `medium`), `context_window` (128K), `max_output_tokens` (16K), `pricing` (`{{input, output, cache_write, cache_read}}`, all per 1M tokens), `supports_tool_examples` (defaults to the base provider's setting), `supports_thinking` (defaults to the base provider's setting), `supports_vision` (defaults to the base provider's setting; when false, image input and the `view_image` tool are disabled), `thinking_dialect` (overrides the base provider's effort level mapping; one of `standard`, `openai-extended`, `prefer-high`, `high-only`, `glm`, `deep-seek`, `anthropic-adaptive`, `tensor-x`), `thinking_fields` (overrides where thinking values go in the body), `body_override` (per-model body manipulation). The first model listed per tier is used for sub-agents. Without this subcommand, the base provider's models are used.

`thinking_fields` controls where thinking values land: `effort_path` (dot-path for the effort string), `budget_path` (dot-path for a budget integer), `budget_max` (cap), and `toggles` (objects with `path`, `on`, `off`, `adaptive`, and `budget_key`). Fields left unset fall back to the base provider's layout.

`body_override` is the escape hatch for a field the base provider does not set, or one it always sets that this model rejects. Its three operations run after the typed thinking setup: `defaults` fills absent keys, `replace` overwrites existing ones, and `filter` strips keys. Each provider guards its conversation field (`messages`, `input`, or `contents`) from all three.

```json
[{{"id": "my-model", "tier": "strong", "body_override": {{"defaults": {{"chat_template_kwargs": {{"enable_thinking": true}}}}, "filter": ["some_base_key"]}}}}]
```

Provider-wide shaping goes in the `info` subcommand as `model_filters`, where each entry applies its `body_override` to every model id matching its `match` glob (`*` and `?`). A model's own `body_override` wins on conflicting keys.

```json
{{"display_name": "My Proxy", "base": "openai", "has_auth": true, "model_filters": [{{"match": "gpt-*", "body_override": {{"filter": ["context_management"]}}}}]}}
```

The same three fields are accepted per model in `providers.toml` for custom providers.

Dynamic provider models are namespaced as `{{slug}}/{{model_id}}` (e.g. `myproxy/claude-sonnet-4-6`).

### Script Name Rules

- Must start with a letter or digit
- Only letters, digits, underscores, and hyphens after that
- Can't reuse a built-in provider's slug
- Must be executable"#,
        valid_values.join(", "),
    )
}

fn tier_label(tier: ModelTier) -> &'static str {
    match tier {
        ModelTier::Weak => "Weak",
        ModelTier::Medium => "Medium",
        ModelTier::Strong => "Strong",
        ModelTier::Compaction => "Compaction",
    }
}

fn format_pricing(entry: &ModelEntry) -> String {
    format!("${:.2} / ${:.2}", entry.pricing.input, entry.pricing.output)
}

fn format_context(entry: &ModelEntry) -> String {
    let ctx_k = entry.context_window / 1_000;
    let out_k = entry.max_output_tokens / 1_000;
    format!("{ctx_k}K ctx / {out_k}K out")
}

struct ProviderSection {
    kind: ProviderKind,
    name: &'static str,
    auth_line: String,
    urls: Vec<&'static str>,
    features: Option<&'static str>,
    entries: &'static [ModelEntry],
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum GenerateError {
    #[error("missing built-in provider manifest for {slug}")]
    MissingManifest { slug: String },
}

fn manifest_models(slug: &str) -> Result<&'static [ModelEntry], GenerateError> {
    ManifestRegistry::get(slug)
        .map(|manifest| manifest.models)
        .ok_or_else(|| GenerateError::MissingManifest {
            slug: slug.to_string(),
        })
}

fn format_auth(kind: ProviderKind) -> String {
    let env = kind.api_key_env();
    if kind == ProviderKind::Ollama {
        format!("`OLLAMA_HOST` for local/remote (e.g. `http://localhost:11434`), `{env}` for auth")
    } else {
        format!("`{env}`")
    }
}

fn build_sections() -> Result<Vec<ProviderSection>, GenerateError> {
    let mut sections = Vec::new();

    for kind in ProviderKind::iter() {
        match kind {
            ProviderKind::Zai => {
                sections.push(ProviderSection {
                    kind: ProviderKind::Zai,
                    name: "Z.AI",
                    auth_line: format!(
                        "{} (shared across both endpoints)",
                        format_auth(ProviderKind::Zai)
                    ),
                    urls: vec![
                        ProviderKind::Zai.base_url(),
                        "https://api.z.ai/api/coding/paas/v4",
                    ],
                    features: ProviderKind::Zai.features(),
                    entries: manifest_models("zai")?,
                });
            }
            ProviderKind::OpenAi => {
                sections.push(ProviderSection {
                    kind,
                    name: kind.display_name(),
                    auth_line: format!("{} (also supports OAuth device flow)", format_auth(kind)),
                    urls: vec![kind.base_url()],
                    features: kind.features(),
                    entries: manifest_models(&kind.to_string())?,
                });
            }
            ProviderKind::Copilot => {
                sections.push(ProviderSection {
                    kind,
                    name: kind.display_name(),
                    auth_line: format!(
                        "{} (or run `n00n auth login copilot` to import a token from gh)",
                        format_auth(kind)
                    ),
                    urls: vec![kind.base_url()],
                    features: kind.features(),
                    entries: manifest_models(&kind.to_string())?,
                });
            }
            ProviderKind::Devin => {
                sections.push(ProviderSection {
                    kind,
                    name: kind.display_name(),
                    auth_line: "`DEVIN_API_KEY`, `WINDSURF_API_KEY`, or `~/.local/share/devin/credentials.toml`"
                        .to_string(),
                    urls: vec![kind.base_url()],
                    features: kind.features(),
                    entries: manifest_models(&kind.to_string())?,
                });
            }
            _ => {
                sections.push(ProviderSection {
                    kind,
                    name: kind.display_name(),
                    auth_line: format_auth(kind),
                    urls: vec![kind.base_url()],
                    features: kind.features(),
                    entries: manifest_models(&kind.to_string())?,
                });
            }
        }
    }

    Ok(sections)
}

fn format_model_names(entry: &ModelEntry) -> String {
    if entry.default && entry.prefixes.contains(&GPT_5_6_ALIAS) {
        return entry
            .prefixes
            .iter()
            .map(|name| {
                if *name == GPT_5_6_ALIAS {
                    format!("**{name}** (default)")
                } else {
                    (*name).to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
    }

    let names = entry.prefixes.join(", ");
    if entry.default {
        format!("**{names}** (default)")
    } else {
        names
    }
}

fn write_model_row(out: &mut String, tier: ModelTier, entries: &[&ModelEntry]) {
    let models = entries
        .iter()
        .map(|entry| format_model_names(entry))
        .collect::<Vec<_>>()
        .join(", ");
    let Some(first) = entries.first() else {
        return;
    };
    let _ = writeln!(
        out,
        "| {} | {} | {} | {} |",
        tier_label(tier),
        models,
        format_pricing(first),
        format_context(first),
    );
}

fn write_model_table(out: &mut String, entries: &[ModelEntry]) {
    let _ = writeln!(
        out,
        "| Tier | Models | Pricing (in/out per 1M tokens) | Context |"
    );
    let _ = writeln!(
        out,
        "|------|--------|-------------------------------|---------|"
    );

    for tier in [ModelTier::Weak, ModelTier::Medium, ModelTier::Strong] {
        let tier_entries: Vec<_> = entries.iter().filter(|e| e.tier == tier).collect();
        if tier_entries.is_empty() {
            continue;
        }

        let (separate_entries, grouped_entries): (Vec<_>, Vec<_>) =
            tier_entries.into_iter().partition(|entry| {
                entry.prefixes.contains(&GPT_6_ASTRA)
                    || entry.prefixes.contains(&GPT_5_5)
                    || entry.prefixes.contains(&GPT_5_5_PRO)
            });
        write_model_row(out, tier, &grouped_entries);
        for entry in separate_entries {
            write_model_row(out, tier, &[entry]);
        }
    }

    let defaults: Vec<String> = entries
        .iter()
        .filter(|e| e.default)
        .map(|e| {
            format!(
                "{} ({})",
                if e.prefixes.contains(&GPT_5_6_ALIAS) {
                    GPT_5_6_ALIAS
                } else {
                    e.prefixes.first().unwrap_or(&"?")
                },
                tier_label(e.tier).to_lowercase(),
            )
        })
        .collect();

    if !defaults.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "Defaults: {}", defaults.join(", "));
    }
}

fn no_catalog_note(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Ollama => {
            "This provider talks the OpenAI-compatible `/v1` API, so it also works with \
             llama.cpp's server, LocalAI, or anything else that speaks the same protocol. \
             Just point `OLLAMA_HOST` to the right address \
             (e.g. `http://localhost:8080` for llama.cpp)."
        }
        ProviderKind::LlamaCpp => {
            "Connects to any OpenAI-compatible `/v1` endpoint. Point `LLAMA_CPP_HOST` \
             to your server address (defaults to `http://localhost:8080`)."
        }
        ProviderKind::OpenRouter => {
            "OpenRouter aggregates models from many providers behind a single API key. \
             Browse available models at [openrouter.ai/models](https://openrouter.ai/models). \
             Use any model ID directly (e.g. `openrouter/anthropic/claude-sonnet-4`)."
        }
        _ => "No hardcoded model catalog. Use any model ID supported by this provider.",
    }
}

fn write_section(out: &mut String, section: &ProviderSection) {
    let _ = writeln!(out, "### {}\n", section.name);
    let auth_label = if section.kind == ProviderKind::Devin {
        "Authentication"
    } else {
        "Env var"
    };
    let _ = writeln!(out, "- **{auth_label}**: {}", section.auth_line);

    if section.urls.len() == 1 {
        let _ = writeln!(out, "- **API**: `{}`", section.urls[0]);
    } else {
        let _ = writeln!(out, "- **API endpoints**:");
        for url in &section.urls {
            let _ = writeln!(out, "  - `{url}`");
        }
    }

    if let Some(features) = section.features {
        let _ = writeln!(out, "- **Features**: {features}");
    }

    let _ = writeln!(out);

    if section.kind == ProviderKind::Devin {
        let _ = writeln!(out, "{DEVIN_ACCOUNTS_NOTE}\n");
    }

    if section.entries.is_empty() {
        let _ = writeln!(out, "{}", no_catalog_note(section.kind));
    } else {
        write_model_table(out, section.entries);
    }

    if section.name == "Anthropic" {
        let _ = writeln!(out, "\n{LONG_CONTEXT_NOTE}");
        let _ = writeln!(out, "\n{BEDROCK_NOTE}");
    }

    if section.kind == ProviderKind::Opencode {
        let _ = writeln!(out, "\n{OPENCODE_GO_NOTE}");
        let _ = writeln!(out, "\n{OPENCODE_FREE_MODELS_NOTE}");
    }
}

pub fn generate() -> Result<String, GenerateError> {
    let mut out = String::with_capacity(4096);

    let _ = writeln!(out, "{FRONT_MATTER}\n");
    let _ = writeln!(out, "# Providers\n");
    let _ = writeln!(
        out,
        "n00n talks to LLM providers over their HTTP APIs. \
         Models are split into three tiers: **weak** (cheap and fast), \
         **medium** (balanced), and **strong** (highest capability, highest cost). \
         There is also a **compaction** tier for choosing a dedicated model to summarize context when the conversation grows long.\n"
    );
    let _ = writeln!(out, "{TIER_PICKER_NOTE}\n");
    let _ = writeln!(out, "{AUTH_RELOADING}\n");
    let _ = writeln!(out, "{BASE_URL_OVERRIDES}\n");
    let _ = writeln!(out, "## Built-in Providers\n");

    for section in &build_sections()? {
        write_section(&mut out, section);
        let _ = writeln!(out);
    }

    let _ = writeln!(out, "{MODEL_IDENTIFIERS}\n");
    let _ = writeln!(out, "{}", dynamic_providers_section());

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_docs_show_alias_default_and_distinct_pro_metadata() {
        let generated = generate().unwrap();
        assert!(generated.contains("gpt-5.6-sol, **gpt-5.6** (default)"));
        assert!(
            generated.contains("| Strong | gpt-6-astra | $10.00 / $50.00 | 1050K ctx / 128K out |")
        );
        assert!(
            generated.contains("| Strong | gpt-6-astra | $5.00 / $30.00 | 272K ctx / 128K out |")
        );
        assert!(
            generated.contains(
                "Defaults: gpt-5.6-luna (weak), gpt-5.6-terra (medium), gpt-5.6 (strong)"
            )
        );
        assert!(generated.contains("| Strong | gpt-5.5 | $5.00 / $30.00 | 1050K ctx / 128K out |"));
        assert!(
            generated.contains("| Strong | gpt-5.5-pro | $7.50 / $45.00 | 1050K ctx / 128K out |")
        );
        assert!(
            generated.contains("| Strong | gpt-5.5-pro | $7.50 / $45.00 | 272K ctx / 128K out |")
        );
    }

    #[test]
    fn missing_manifest_returns_typed_error() {
        let error = manifest_models("missing-provider").expect_err("missing manifest");
        assert_eq!(
            error,
            GenerateError::MissingManifest {
                slug: "missing-provider".to_string(),
            }
        );
    }
}
