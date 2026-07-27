Fix hang when models emit only reasoning without final text.

- Add `thinking: Arc<AsyncMutex<String>>` field to Devin's `DevinInner`
- Capture thinking deltas in `handle_session_update` and append to `thinking` buffer
- Build assistant message with both `Thinking` and `Text` content blocks
- Remove invalid `reasoning_content` field from OpenAI compat message conversion
- Add nudge logic in agent when assistant produces only reasoning without text
- Add system prompt instruction to always end with a user-facing final answer
