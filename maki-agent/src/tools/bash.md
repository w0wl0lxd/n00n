Execute a bash command.
Commands run in {cwd} by default.

- AVOID `cd <directory> && <command>` patterns. Use the `workdir` parameter to run in a different directory instead.
  <good-example>
  Use workdir="/foo/bar" with command: pytest tests
  </good-example>
  <bad-example>
  cd /foo/bar && pytest tests
  </bad-example>
- Use for git, builds, tests, and system commands only.
- Do NOT use bash to communicate text to the user - output text directly instead.
- Do NOT use for file operations (reading, writing, searching) - use specialized tools.
  - File search: Use Glob (NOT find or ls)
  - Content search: Use Grep (NOT grep or rg)
  - Read files: Use Read (NOT cat/head/tail)
  - Edit files: Use Edit or MultiEdit (NOT sed/awk)
  - Write files: Use Write (NOT echo >/cat <<EOF)
  - Communication: Output text directly (NOT echo/printf)
- When issuing multiple commands:
  - If independent, make multiple Bash tool calls in parallel.
  - If dependent, chain with '&&' in a single call.
- Provide a short `description` (3-5 words) of what the command does.
- Output truncated beyond 2000 lines or 50KB.
- Commands requiring interactive input (sudo, ssh password prompts) will fail immediately.
  Use non-interactive alternatives (e.g. `sudo -n`, `ssh -o BatchMode=yes`, `-y` flags).
