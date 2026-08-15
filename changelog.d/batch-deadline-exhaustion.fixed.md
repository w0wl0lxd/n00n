Fixed `batch` tool calls under a near-exhausted subagent deadline getting killed mid-flight by the watchdog interrupt; children now settle with a clear "insufficient time remaining" error instead.
