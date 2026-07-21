+++
title = "ACP"
weight = 9
[extra]
group = "Reference"
+++

# ACP (Agent Client Protocol)

Run n00n inside your editor. `n00n acp` starts an [ACP](https://agentclientprotocol.com/) server over stdio, so any ACP-capable editor (like [Zed](https://zed.dev/)) can drive n00n as its coding agent.

```bash
n00n acp
```

## Zed setup

Add n00n as a custom agent in Zed's `settings.json`:

```json
"agent_servers": {
  "n00n": {
    "default_config_options": {
      "model": "deepseek/deepseek-v4-flash"
    },
    "type": "custom",
    "command": "n00n",
    "args": ["acp"],
    "env": {}
  }
}
```

The `model` value is a `provider/model-id` spec, same format as `n00n --model`.

## What works

- **Sessions persist.** Loading a session replays the full conversation in the editor, so you can resume where you left off.
- **Model switching.** Pick a model from the editor's dropdown, mid-session. All configured providers show up.
- **Modes.** Switch between build (full access) and plan (read-only) from the editor.
- **Permissions.** Tool permission prompts appear in the editor: allow or reject, once or always.
- **Live tool calls.** Tool progress streams as it happens, including sub-agents and batched calls.
- **Images and context.** Prompts can include images and editor-attached files.

Authentication, providers, and permissions come from your normal n00n config. Set up [providers](/docs/providers/) first and ACP sessions just work.
