Add display names and pricing for Devin ACP private models.

- Add `name: Option<String>` field to `ModelInfo` for human-readable model names
- Set `info.name` from Devin's `SessionConfigSelectOption.name` in `list_models`
- Add heuristics for `MODEL_PRIVATE_*` models:
  - Context window: 200k for Claude 4.5 family, 400k for GPT-5.1 family
  - Max output tokens: 64k for Claude 4.5, 128k for GPT-5.1
  - Pricing from Devin docs (e.g., Claude Haiku 4.5: $1/$5 per million tokens)
  - Mark all `MODEL_PRIVATE_*` as non-free and promo/preview
- Update model picker UI to display discovered names instead of raw IDs
- Update all `ModelInfo` constructors across the workspace to include `name: None`
