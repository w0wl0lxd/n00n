Fix auto-compaction to handle `server_is_overloaded` errors, pre-truncate history to fit within the model's context window before streaming, and raise the default PreCompact hook timeout to 60s.
