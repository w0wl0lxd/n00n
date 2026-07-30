Add native Devin provider implementation using Connect protocol over gRPC-Web.

The new implementation:
- Reads credentials from `~/.local/share/devin/credentials.toml` or `WINDSURF_API_KEY`/`DEVIN_API_KEY` env
- Implements `GetUserJwt` exchange to obtain user JWT
- Implements `GetChatMessageRequest` encoder and `GetChatMessageResponse` decoder using hand-rolled protobuf
- Implements streaming response parser for Connect frames with gzip decompression
- Emits ProviderEvents: `TextDelta`, `ThinkingDelta`, `ToolUseStart`/`ToolUseDelta`/`ToolUseEnd`, `Done`/`Error`
- Supports tool definitions in requests and tool-call streaming in responses
- Adds a full Devin model catalog from the live CLI model list
- Resolves display model ids to wire uids via `GetCliModelConfigs`
- Allows provider-only model specs like `n00n -m devin` by selecting the provider default

Replaces the ACP-based `devin` provider with a native HTTP implementation that calls Devin's gRPC-Web API directly.
