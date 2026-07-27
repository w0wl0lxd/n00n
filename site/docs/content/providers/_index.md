+++
title = "Providers"
weight = 5
[extra]
group = "Reference"
+++

# Providers

n00n talks to LLM providers over their HTTP APIs. Models are split into three tiers: **weak** (cheap and fast), **medium** (balanced), and **strong** (highest capability, highest cost). There is also a **compaction** tier for choosing a dedicated model to summarize context when the conversation grows long.

Open the model picker with `/model` and press `!`, `@`, `#`, or `$` on any row to assign it to strong, medium, weak, or compaction. Press the same key again to remove the assignment. Your overrides are saved to `~/.local/state/n00n/model-tiers` and apply across sessions.

## Auth Reloading

n00n re-reads auth from storage and environment variables each time a new agent spawns (`/new`, retry, session load). If you run `n00n auth login` in another terminal or change an env var, the next session picks it up without a restart.

You can set multiple API keys in one env var (`ANTHROPIC_API_KEY=sk-1,sk-2,sk-3`) and they rotate automatically on rate-limit or auth errors.

## Base URL Overrides

Every provider honors a `<SLUG>_BASE_URL` env var (`anthropic` -> `ANTHROPIC_BASE_URL`, `llama-cpp` -> `LLAMA_CPP_BASE_URL`). Set it to the origin of a proxy or a compatible endpoint and n00n appends the API paths itself:

```sh
ANTHROPIC_BASE_URL=https://my-proxy.internal n00n
```

It wins over `providers.toml` and built-in defaults. `ANTHROPIC_BASE_URL` and `OPENAI_BASE_URL` are the same names the official SDKs use, so an existing proxy setup carries over as is. One exception: `OPENAI_BASE_URL` only redirects the platform API, never the ChatGPT Coding Plan backend.

## Built-in Providers

### Anthropic

- **Env var**: `ANTHROPIC_API_KEY`
- **API**: `https://api.anthropic.com/v1/messages`
- **Features**: Prompt caching, thinking mode (adaptive/budgeted), advanced tool use

| Tier | Models | Pricing (in/out per 1M tokens) | Context |
|------|--------|-------------------------------|---------|
| Weak | **claude-haiku-4-5** (default) | $1.00 / $5.00 | 200K ctx / 64K out |
| Medium | claude-sonnet-4-5, **claude-sonnet-4-6** (default), claude-sonnet-4 | $3.00 / $15.00 | 200K ctx / 64K out |
| Strong | claude-opus-4-5, claude-opus-4-6, claude-opus-4-7, **claude-opus-4-8** (default), claude-fable-5, claude-opus-4-0, claude-opus-4-1 | $5.00 / $25.00 | 200K ctx / 64K out |

Defaults: claude-haiku-4-5 (weak), claude-sonnet-4-6 (medium), claude-opus-4-8 (strong)

Add `-1m` to any Claude model, like `claude-sonnet-4-6-1m`, to use the 1M token context window.

#### Amazon Bedrock

If you already use Claude through AWS Bedrock, you can point n00n at it instead of the direct Anthropic API. Set `CLAUDE_CODE_USE_BEDROCK=1` and n00n will route all Anthropic requests through Bedrock. The same models, the same features, just a different door.

You will need `AWS_REGION` and one of the following for auth:

| Method | Env vars |
|--------|----------|
| IAM credentials | `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY` (and optionally `AWS_SESSION_TOKEN`) |
| Credentials file | `AWS_PROFILE` (defaults to `default`), reads `~/.aws/credentials` |
| Bearer token | `AWS_BEARER_TOKEN_BEDROCK` |
| Gateway proxy | `CLAUDE_CODE_SKIP_BEDROCK_AUTH=1` + `ANTHROPIC_BEDROCK_BASE_URL` (skips signing, useful behind a proxy that handles auth) |

You can override the model with `ANTHROPIC_MODEL` and the endpoint with `ANTHROPIC_BEDROCK_BASE_URL`. These env var names match Claude Code, so if you were already using Bedrock there, the same setup works here.

### OpenAI

- **Env var**: `OPENAI_API_KEY` (also supports OAuth device flow)
- **API**: `https://api.openai.com/v1`

| Tier | Models | Pricing (in/out per 1M tokens) | Context |
|------|--------|-------------------------------|---------|
| Weak | **gpt-5.6-luna** (default), gpt-5.4-nano, gpt-5.4-mini, gpt-4.1-nano | $1.00 / $6.00 | 272K ctx / 128K out |
| Medium | **gpt-5.6-terra** (default), gpt-4.1-mini, gpt-4.1, o4-mini, gpt-5.1-codex-mini | $2.50 / $15.00 | 272K ctx / 128K out |
| Strong | **gpt-5.6-sol** (default), gpt-5.5, gpt-5.4, o3, gpt-5.3-codex-spark, gpt-5.3-codex, gpt-5.2-codex, gpt-5.1-codex-max, gpt-5.1-codex | $5.00 / $30.00 | 272K ctx / 128K out |

Defaults: gpt-5.6-luna (weak), gpt-5.6-terra (medium), gpt-5.6-sol (strong)

### Google

- **Env var**: `GEMINI_API_KEY`
- **API**: `https://generativelanguage.googleapis.com/v1beta`
- **Features**: Native Gemini API with thinking support

| Tier | Models | Pricing (in/out per 1M tokens) | Context |
|------|--------|-------------------------------|---------|
| Weak | **gemini-2.0-flash-lite** (default) | $0.07 / $0.30 | 1048K ctx / 65K out |
| Medium | **gemini-2.5-flash** (default) | $0.15 / $0.60 | 1048K ctx / 65K out |
| Strong | **gemini-2.5-pro** (default) | $1.25 / $5.00 | 1048K ctx / 65K out |

Defaults: gemini-2.5-pro (strong), gemini-2.5-flash (medium), gemini-2.0-flash-lite (weak)

### Copilot

- **Env var**: `GH_COPILOT_TOKEN` (or run `n00n auth login copilot` to import a token from gh)
- **API**: `https://api.githubcopilot.com (or GraphQL-discovered Copilot API endpoint)`
- **Features**: Native Copilot Chat HTTP API with model endpoint discovery

| Tier | Models | Pricing (in/out per 1M tokens) | Context |
|------|--------|-------------------------------|---------|
| Weak | **gpt-5-mini, gpt-5 mini, claude-haiku-4.5** (default) | $0.00 / $0.00 | 200K ctx / 100K out |
| Medium | **gpt-5.2, gpt-4.1, claude-sonnet-4.5** (default) | $0.00 / $0.00 | 200K ctx / 100K out |
| Strong | **gpt-5.4, gpt-5.3-codex, claude-opus-4.6, grok-code-fast-1** (default), claude-opus-4.7 | $0.00 / $0.00 | 200K ctx / 100K out |

Defaults: gpt-5-mini (weak), gpt-5.2 (medium), gpt-5.4 (strong)

### Ollama

- **Env var**: `OLLAMA_HOST` for local/remote (e.g. `http://localhost:11434`), `OLLAMA_API_KEY` for auth
- **API**: `http://localhost:11434/v1`
- **Features**: Local or remote inference via OLLAMA_HOST, cloud fallback via OLLAMA_API_KEY

This provider talks the OpenAI-compatible `/v1` API, so it also works with llama.cpp's server, LocalAI, or anything else that speaks the same protocol. Just point `OLLAMA_HOST` to the right address (e.g. `http://localhost:8080` for llama.cpp).

### LlamaCpp

- **Env var**: `LLAMA_CPP_API_KEY`
- **API**: `http://localhost:8080/v1`
- **Features**: Local or remote inference via LLAMA_CPP_HOST, set optional key via LLAMA_CPP_API_KEY

Connects to any OpenAI-compatible `/v1` endpoint. Point `LLAMA_CPP_HOST` to your server address (defaults to `http://localhost:8080`).

### Mistral

- **Env var**: `MISTRAL_API_KEY`
- **API**: `https://api.mistral.ai/v1`

| Tier | Models | Pricing (in/out per 1M tokens) | Context |
|------|--------|-------------------------------|---------|
| Weak | **ministral-14b-latest, ministral-14b-2512** (default) | $0.20 / $0.20 | 262K ctx / 262K out |
| Medium | **mistral-small-latest, mistral-small-2603** (default) | $0.15 / $0.60 | 262K ctx / 262K out |
| Strong | **mistral-medium-latest, mistral-medium-3.5, mistral-medium-2604** (default) | $1.50 / $7.50 | 262K ctx / 262K out |

Defaults: mistral-medium-latest (strong), mistral-small-latest (medium), ministral-14b-latest (weak)

### Z.AI

- **Env var**: `ZHIPU_API_KEY` (shared across both endpoints)
- **API endpoints**:
  - `https://api.z.ai/api/paas/v4`
  - `https://api.z.ai/api/coding/paas/v4`

| Tier | Models | Pricing (in/out per 1M tokens) | Context |
|------|--------|-------------------------------|---------|
| Weak | **glm-4.7-flash** (default), glm-4.5-flash, glm-4.5-air | $0.00 / $0.00 | 200K ctx / 131K out |
| Medium | **glm-4.7, glm-4.6** (default), glm-4.5 | $0.60 / $2.20 | 200K ctx / 131K out |
| Strong | **glm-5-code** (default), glm-5.2, glm-5.1, glm-5 | $1.20 / $5.00 | 200K ctx / 131K out |

Defaults: glm-5-code (strong), glm-4.7-flash (weak), glm-4.7 (medium)

### DeepSeek

- **Env var**: `DEEPSEEK_API_KEY`
- **API**: `https://api.deepseek.com`
- **Features**: Thinking mode toggle (on/off), open-weight models

| Tier | Models | Pricing (in/out per 1M tokens) | Context |
|------|--------|-------------------------------|---------|
| Medium | **deepseek-v4-flash** (default) | $0.14 / $0.28 | 1000K ctx / 384K out |
| Strong | **deepseek-v4-pro** (default) | $0.43 / $0.87 | 1000K ctx / 384K out |

Defaults: deepseek-v4-flash (medium), deepseek-v4-pro (strong)

### OpenRouter

- **Env var**: `OPENROUTER_API_KEY`
- **API**: `https://openrouter.ai/api/v1`
- **Features**: 300+ models from all providers, prompt caching, provider routing

OpenRouter aggregates models from many providers behind a single API key. Browse available models at [openrouter.ai/models](https://openrouter.ai/models). Use any model ID directly (e.g. `openrouter/anthropic/claude-sonnet-4`).

### Synthetic

- **Env var**: `SYNTHETIC_API_KEY`
- **API**: `https://api.synthetic.new/openai/v1`
- **Features**: Reasoning effort support (low/medium/high), open-weight models

| Tier | Models | Pricing (in/out per 1M tokens) | Context |
|------|--------|-------------------------------|---------|
| Weak | **hf:zai-org/GLM-4.7-Flash** (default) | $0.10 / $0.50 | 200K ctx / 131K out |
| Medium | **hf:deepseek-ai/DeepSeek-V3.2** (default) | $0.56 / $1.68 | 200K ctx / 131K out |
| Strong | **hf:moonshotai/Kimi-K2.5** (default) | $0.45 / $3.40 | 200K ctx / 131K out |

Defaults: hf:moonshotai/Kimi-K2.5 (strong), hf:deepseek-ai/DeepSeek-V3.2 (medium), hf:zai-org/GLM-4.7-Flash (weak)

### TensorX

- **Env var**: `TENSORX_API_KEY`
- **API**: `https://api.tensorx.ai/v1`
- **Features**: Open-weight models, zero data retention, prompt caching

No hardcoded model catalog. Use any model ID supported by this provider.

### Opencode

- **Env var**: `OPENCODE_API_KEY`
- **API**: `https://opencode.ai/zen/v1`
- **Features**: Dynamically discovered models via [models.dev](https://models.dev/) + all the models provided by Opencode Zen API

No hardcoded model catalog. Use any model ID supported by this provider.

By default n00n hides free models from the Opencode catalog. To list free models (they use a public fallback, no API key needed), add this to `~/.config/n00n/providers.toml`:

```toml
[opencode]
enable_free_models = true
```

The default is `false`.

### Devin

- **Env var**: `DEVIN_API_KEY`
- **API**: `devin acp subprocess (Agent Client Protocol)`
- **Features**: Agent Client Protocol via devin acp subprocess

| Tier | Models | Pricing (in/out per 1M tokens) | Context |
|------|--------|-------------------------------|---------|
| Strong | swe-1-7, **swe-1-7-max** (default), swe-1-7-lightning, claude-sonnet-4-6, gpt-5-4-none, gemini-3-1-pro-low | $0.00 / $0.00 | 262K ctx / 128K out |

Defaults: swe-1-7-max (strong)

### Cursor

- **Env var**: `CURSOR_API_KEY`
- **API**: `cursor-agent subprocess (Cursor Agent CLI)`
- **Features**: Cursor Agent CLI subprocess with stream-json parsing, session resume, and tool-call passthrough

| Tier | Models | Pricing (in/out per 1M tokens) | Context |
|------|--------|-------------------------------|---------|
| Weak | gpt-5.6-luna-none, gpt-5.6-luna-none-fast, gpt-5.6-luna-low, gpt-5.6-luna-low-fast, gpt-5.6-luna-medium, gpt-5.6-luna-medium-fast, gpt-5.6-luna-high, gpt-5.6-luna-high-fast, gpt-5.6-luna-xhigh, gpt-5.6-luna-xhigh-fast, gpt-5.6-luna-max, gpt-5.6-luna-max-fast, gemini-3.6-flash-minimal, gemini-3.6-flash-low, gemini-3.6-flash-medium, gemini-3.6-flash-high, gpt-5.4-mini-none, gpt-5.4-mini-low, gpt-5.4-mini-medium, gpt-5.4-mini-high, gpt-5.4-mini-xhigh, gpt-5.4-nano-none, gpt-5.4-nano-low, gpt-5.4-nano-medium, gpt-5.4-nano-high, gpt-5.4-nano-xhigh, gemini-3-flash, gpt-5-mini | $1.00 / $6.00 | 1000K ctx / 128K out |
| Medium | gpt-5.6-terra-none, gpt-5.6-terra-none-fast, gpt-5.6-terra-low, gpt-5.6-terra-low-fast, gpt-5.6-terra-medium, gpt-5.6-terra-medium-fast, gpt-5.6-terra-high, gpt-5.6-terra-high-fast, gpt-5.6-terra-xhigh, gpt-5.6-terra-xhigh-fast, gpt-5.6-terra-max, gpt-5.6-terra-max-fast, claude-4.5-sonnet, claude-4.5-sonnet-thinking, gpt-5.1-low, gpt-5.1, gpt-5.1-high, gemini-3.5-flash, claude-4-sonnet, claude-4-sonnet-thinking, kimi-k2.7-code, glm-5.2-high, glm-5.2-max | $2.50 / $15.00 | 1000K ctx / 128K out |
| Strong | auto, gpt-5.3-codex-low, gpt-5.3-codex-low-fast, gpt-5.3-codex, gpt-5.3-codex-fast, gpt-5.3-codex-high, gpt-5.3-codex-high-fast, gpt-5.3-codex-xhigh, gpt-5.3-codex-xhigh-fast, gpt-5.2, cursor-grok-4.5-high, cursor-grok-4.5-high-fast, **composer-2.5** (default), claude-opus-5-thinking-high, claude-opus-5-thinking-high-fast, claude-opus-4-8-thinking-high, claude-opus-4-8-thinking-high-fast, gpt-5.6-sol-high, gpt-5.6-sol-high-fast, gpt-5.6-sol-xhigh, gpt-5.6-sol-xhigh-fast, gpt-5.5-high, gpt-5.5-high-fast, claude-fable-5-thinking-high, claude-fable-5-thinking-xhigh, gpt-5.4-high, gpt-5.4-high-fast, cursor-grok-4.5-low, cursor-grok-4.5-low-fast, cursor-grok-4.5-medium, cursor-grok-4.5-medium-fast, composer-2.5-fast, claude-opus-5-low, claude-opus-5-low-fast, claude-opus-5-medium, claude-opus-5-medium-fast, claude-opus-5-high, claude-opus-5-high-fast, claude-opus-5-thinking-low, claude-opus-5-thinking-low-fast, claude-opus-5-thinking-medium, claude-opus-5-thinking-medium-fast, claude-opus-5-thinking-xhigh, claude-opus-5-thinking-xhigh-fast, claude-opus-5-thinking-max, claude-opus-5-thinking-max-fast, claude-opus-4-8-low, claude-opus-4-8-low-fast, claude-opus-4-8-medium, claude-opus-4-8-medium-fast, claude-opus-4-8-high, claude-opus-4-8-high-fast, claude-opus-4-8-xhigh, claude-opus-4-8-xhigh-fast, claude-opus-4-8-max, claude-opus-4-8-max-fast, claude-opus-4-8-thinking-low, claude-opus-4-8-thinking-low-fast, claude-opus-4-8-thinking-medium, claude-opus-4-8-thinking-medium-fast, claude-opus-4-8-thinking-xhigh, claude-opus-4-8-thinking-xhigh-fast, claude-opus-4-8-thinking-max, claude-opus-4-8-thinking-max-fast, gpt-5.6-sol-none, gpt-5.6-sol-none-fast, gpt-5.6-sol-low, gpt-5.6-sol-low-fast, gpt-5.6-sol-medium, gpt-5.6-sol-medium-fast, gpt-5.6-sol-max, gpt-5.6-sol-max-fast, gpt-5.5-none, gpt-5.5-none-fast, gpt-5.5-low, gpt-5.5-low-fast, gpt-5.5-medium, gpt-5.5-medium-fast, gpt-5.5-extra-high, gpt-5.5-extra-high-fast, claude-fable-5-low, claude-fable-5-medium, claude-fable-5-high, claude-fable-5-xhigh, claude-fable-5-max, claude-fable-5-thinking-low, claude-fable-5-thinking-medium, claude-fable-5-thinking-max, claude-sonnet-5-low, **claude-sonnet-5-medium** (default), claude-sonnet-5-high, claude-sonnet-5-xhigh, claude-sonnet-5-max, claude-sonnet-5-thinking-low, claude-sonnet-5-thinking-medium, claude-sonnet-5-thinking-high, claude-sonnet-5-thinking-xhigh, claude-sonnet-5-thinking-max, claude-4.6-sonnet-medium, claude-4.6-sonnet-medium-thinking, claude-opus-4-7-low, claude-opus-4-7-low-fast, claude-opus-4-7-medium, claude-opus-4-7-medium-fast, claude-opus-4-7-high, claude-opus-4-7-high-fast, claude-opus-4-7-xhigh, claude-opus-4-7-xhigh-fast, claude-opus-4-7-max, claude-opus-4-7-max-fast, claude-opus-4-7-thinking-low, claude-opus-4-7-thinking-low-fast, claude-opus-4-7-thinking-medium, claude-opus-4-7-thinking-medium-fast, claude-opus-4-7-thinking-high, claude-opus-4-7-thinking-high-fast, claude-opus-4-7-thinking-xhigh, claude-opus-4-7-thinking-xhigh-fast, claude-opus-4-7-thinking-max, claude-opus-4-7-thinking-max-fast, gpt-5.4-low, gpt-5.4-medium, gpt-5.4-medium-fast, gpt-5.4-xhigh, gpt-5.4-xhigh-fast, claude-4.6-opus-high, claude-4.6-opus-max, claude-4.6-opus-high-thinking, claude-4.6-opus-max-thinking, claude-4.5-opus-high, claude-4.5-opus-high-thinking, gpt-5.2-low, gpt-5.2-low-fast, gpt-5.2-fast, gpt-5.2-high, gpt-5.2-high-fast, gpt-5.2-xhigh, gpt-5.2-xhigh-fast, gemini-3.1-pro | $1.25 / $6.00 | 128K ctx / 32K out |

Defaults: composer-2.5 (strong), claude-sonnet-5-medium (strong)

## Model Identifiers

Models are referenced as `provider/model_id`:

```
anthropic/claude-sonnet-4-6
openai/gpt-4.1
zai/glm-4.7
```

If the model name is unique across providers, the prefix can be omitted.

## Dynamic Providers

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

The `base` field specifies which built-in provider to inherit the model catalog from. Valid values: `anthropic`, `openai`, `google`, `copilot`, `ollama`, `llama-cpp`, `mistral`, `zai`, `deepseek`, `openrouter`, `synthetic`, `tensorx`, `opencode`, `devin`, `cursor`.

If your provider serves models not in the base catalog, add a `models` subcommand returning:

```json
[{"id": "my-model-v2", "tier": "strong", "context_window": 200000, "max_output_tokens": 16384}]
```

Only `id` is required. Optional fields: `tier` (default `medium`), `context_window` (128K), `max_output_tokens` (16K), `pricing` (`{input, output, cache_write, cache_read}`, all per 1M tokens), `supports_tool_examples` (defaults to the base provider's setting), `supports_thinking` (defaults to the base provider's setting), `supports_vision` (defaults to the base provider's setting; when false, image input and the `view_image` tool are disabled). The first model listed per tier is used for sub-agents. Without this subcommand, the base provider's models are used.

Dynamic provider models are namespaced as `{slug}/{model_id}` (e.g. `myproxy/claude-sonnet-4-6`).

### Script Name Rules

- Must start with a letter or digit
- Only letters, digits, underscores, and hyphens after that
- Can't reuse a built-in provider's slug
- Must be executable
