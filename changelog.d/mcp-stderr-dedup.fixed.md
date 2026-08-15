Deduplicated consecutive identical stderr lines re-logged from an MCP child process, so a child stuck retrying (e.g. connection refused) no longer floods the log one line per retry.
