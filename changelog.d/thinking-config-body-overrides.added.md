<!-- markdownlint-disable MD041 -->
Added per-model `thinking_dialect`, `thinking_fields`, and `body_override` config for dynamic providers (script `models`/`info`) and custom providers (`providers.toml`), letting a model declare where thinking values go in the request body and shape the body with `defaults`/`replace`/`filter` after the provider's own setup.
