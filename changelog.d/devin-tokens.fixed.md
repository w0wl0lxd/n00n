Register `swe-1-7-max` and `swe-1-7-medium` as the canonical Devin model ids with `swe-1-7` and dot-prefixed variants (`swe-1.7`/`swe-1.7-max`/`swe-1.7-medium`) as aliases, and correct the `swe-1-7` family context window to `262_144` tokens.

Validate the `devin`/`devin2` `base_url` before using it for authentication, falling back to the configured API server when it is missing or is not an `http://`/`https://` URL. This fixes `failed to build auth request: invalid format` errors when a provider name like `devin2` is configured.

Map Devin gRPC `ModelUsageStats` to `TokenUsage` correctly, treating `input_tokens` as the total prompt and the cache fields as additive details. Invalid cache breakdowns are ignored instead of changing the meaning of `input_tokens` based on counter magnitudes.

Report the full resumed conversation size in the post-compaction `TurnComplete` event using the active model tokenizer, continuation prompt, and tool definitions, so the context meter no longer drops sharply after compaction.
