Execute a bash command.
Commands run in {cwd} by default.

- Use `workdir` param instead of `cd <dir> && <cmd>` patterns.
- For git, builds, tests, and system commands only. NOT for file ops.
- Do NOT use to communicate text to the user.
- Chain dependent commands with `&&`. Use batch for independent ones.
- Provide a short `description` (3-5 words).
- Output truncated beyond 2000 lines or 50KB.
- Interactive commands (sudo, ssh prompts) fail immediately.
