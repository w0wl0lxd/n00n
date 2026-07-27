"""Harbor agent wrapper for running n00n on Terminal-Bench 2.1."""

import json
import os
import shlex
import tempfile
from pathlib import Path

from harbor.agents.installed.base import BaseInstalledAgent, with_prompt_template
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext

AGENT_LOG_FILE = "n00n.txt"

_PROVIDER_API_KEY_OVERRIDES: dict[str, frozenset[str]] = {
    "devin": frozenset({"DEVIN_API_KEY", "WINDSURF_API_KEY"}),
    "copilot": frozenset({"GH_COPILOT_TOKEN", "GH_TOKEN"}),
    "google": frozenset({"GEMINI_API_KEY", "GOOGLE_API_KEY"}),
    "zai": frozenset({"ZHIPU_API_KEY"}),
}

# devin-real block-buffers stdout when run through a pipe; run it under a
# pseudo-tty so ACP traffic is line-buffered and n00n sees responses promptly.
_DEVIN_WRAPPER = """#!/usr/bin/env python3
import os
import pty
import select
import subprocess
import sys
import termios
import threading
import tty

REAL = "/opt/n00n/bin/devin-real"
LOG = "/tmp/devin-acp.log"


def main() -> int:
    # n00n already passes the "acp" subcommand; do not duplicate it.
    argv = [REAL, "--permission-mode", "dangerous"] + sys.argv[1:]
    log = open(LOG, "wb")
    master, slave = pty.openpty()
    tty.setraw(master, termios.TCSANOW)
    p = subprocess.Popen(argv, stdin=slave, stdout=slave, stderr=slave)
    os.close(slave)

    def forward_input():
        try:
            fd = sys.stdin.buffer.fileno()
            while True:
                data = os.read(fd, 4096)
                if not data:
                    break
                log.write(b"IN>> " + data)
                log.flush()
                os.write(master, data)
        except OSError:
            pass

    t = threading.Thread(target=forward_input)
    t.start()

    try:
        while True:
            r, _, _ = select.select([master], [], [], 0.1)
            if not r:
                if p.poll() is not None:
                    break
                continue
            data = os.read(master, 4096)
            if not data:
                break
            log.write(b"OUT<< " + data)
            log.flush()
            sys.stdout.buffer.write(data)
            sys.stdout.buffer.flush()
    except OSError:
        pass

    t.join()
    rc = p.wait()
    log.write(f"EXIT rc={rc}\\n".encode())
    log.close()
    return rc


if __name__ == "__main__":
    sys.exit(main())
"""


def _parse_stream_json(log_text: str) -> dict:
    """Parse n00n --verbose --output-format stream-json output.

    Returns the last result object plus the final model id.
    """
    result: dict = {}
    model = ""
    session_id = ""

    for line in log_text.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue

        msg_type = msg.get("type")
        if msg_type == "system":
            init = msg.get("init", msg)
            session_id = init.get("session_id", session_id)
            model = init.get("model", model)
        elif msg_type == "result":
            session_id = msg.get("session_id", session_id)
            result = msg

    if not result.get("session_id"):
        result["session_id"] = session_id
    if not result.get("model"):
        result["model"] = model
    return result


class N00nAgent(BaseInstalledAgent):
    """Runs n00n in headless --print mode inside a Harbor environment."""

    _last_instruction: str = ""

    @staticmethod
    def name() -> str:
        return "n00n"

    def get_version_command(self) -> str | None:
        return "n00n --version"

    async def install(self, environment: BaseEnvironment) -> None:
        # Probe for an already-installed n00n binary.
        probe = await environment.exec(
            command="command -v n00n >/dev/null 2>&1 && n00n --version",
            user="root",
        )
        if probe.return_code != 0:
            # No n00n in PATH: try a mounted binary first.
            mount_check = await environment.exec(
                command="test -f /mnt/n00n",
                user="root",
            )
            if mount_check.return_code == 0:
                await self.exec_as_root(
                    environment,
                    command=(
                        "cp /mnt/n00n /usr/local/bin/n00n && "
                        "chmod +x /usr/local/bin/n00n"
                    ),
                )

                auth_check = await environment.exec(
                    command="test -d /mnt/n00n-auth",
                    user="root",
                )
                if auth_check.return_code == 0:
                    await self.exec_as_root(
                        environment,
                        command=(
                            "if [ -d /mnt/n00n-auth ] && [ -n "
                            '"$(ls -A /mnt/n00n-auth 2>/dev/null)" ]; then '
                            "mkdir -p /root/.n00n/auth && "
                            "cp -r /mnt/n00n-auth/. /root/.n00n/auth/; "
                            "fi"
                        ),
                    )

                providers_check = await environment.exec(
                    command="test -d /mnt/n00n-providers",
                    user="root",
                )
                if providers_check.return_code == 0:
                    await self.exec_as_root(
                        environment,
                        command=(
                            "if [ -d /mnt/n00n-providers ] && [ -n "
                            '"$(ls -A /mnt/n00n-providers 2>/dev/null)" ]; then '
                            "mkdir -p /root/.n00n/providers && "
                            "cp -r /mnt/n00n-providers/. /root/.n00n/providers/ && "
                            "chmod -R +x /root/.n00n/providers; "
                            "fi"
                        ),
                    )
            else:
                # Fall back to the bundled tarball (e.g. Daytona/cloud envs).
                bundle_local = Path(__file__).with_name("n00n-bundle.tar.gz")
                if not bundle_local.exists():
                    raise FileNotFoundError(
                        f"n00n bundle not found at {bundle_local}. "
                        "Provide a mounted binary at /mnt/n00n, "
                        "or place n00n-bundle.tar.gz next to this script."
                    )
                await environment.upload_file(bundle_local, "/tmp/n00n-bundle.tar.gz")
                await self.exec_as_root(
                    environment,
                    command=(
                        "mkdir -p /opt/n00n /opt/n00n/.config "
                        "/opt/n00n/.local/share /opt/n00n/.cache "
                        "&& tar -xzf /tmp/n00n-bundle.tar.gz -C /opt/n00n "
                        "--strip-components=1 "
                        "&& chmod -R a+rX /opt/n00n "
                        # The bundled n00n wrapper computes DIR from $0, so a
                        # symlink in /usr/local/bin resolves to the wrong path.
                        "&& sed -i 's|^DIR=.*|DIR=/opt/n00n|' /opt/n00n/n00n "
                        "&& ln -sf /opt/n00n/n00n /usr/local/bin/n00n "
                        "&& rm -f /tmp/n00n-bundle.tar.gz"
                    ),
                )

        # devin-real block-buffers stdout when run through a pipe; replace the
        # bundled wrapper with one that runs devin-real under a pseudo-tty.
        await self._upload_text(environment, _DEVIN_WRAPPER, "/opt/n00n/bin/devin")
        await self.exec_as_root(
            environment,
            command="chmod +x /opt/n00n/bin/devin",
        )

        # Ensure isolated XDG state exists and is writable for any user.
        await self.exec_as_root(
            environment,
            command=(
                "mkdir -p /opt/n00n/.config /opt/n00n/.local/share "
                "/opt/n00n/.cache "
                "&& chmod -R a+rwX,+t /opt/n00n/.config /opt/n00n/.local "
                "/opt/n00n/.cache"
            ),
        )

        # Devin needs a credentials file and a minimal config so it never prompts.
        windsurf_key = self._get_env("WINDSURF_API_KEY")
        if self._is_devin() and windsurf_key:
            await self._write_devin_config(environment, windsurf_key)

    async def _write_devin_config(
        self, environment: BaseEnvironment, windsurf_api_key: str
    ) -> None:
        config_dir = "/opt/n00n/.config/devin"
        data_dir = "/opt/n00n/.local/share/devin"
        await self.exec_as_root(
            environment,
            command=f"mkdir -p {config_dir} {data_dir}",
        )

        config_json = '{"shell":{"setup_complete":true}}'
        credentials_toml = (
            f"api_key = {json.dumps(windsurf_api_key)}\n"
            f"windsurf_api_key = {json.dumps(windsurf_api_key)}\n"
            'api_server_url = "https://server.codeium.com"\n'
            'devin_webapp_host = "app.devin.ai"\n'
            'devin_api_url = "https://api.devin.ai"\n'
        )

        await self._upload_text(environment, config_json, f"{config_dir}/config.json")
        await self._upload_text(
            environment, credentials_toml, f"{data_dir}/credentials.toml"
        )

        await self.exec_as_root(
            environment,
            command=(
                f"chmod 644 {data_dir}/credentials.toml "
                f"&& chmod 644 {config_dir}/config.json"
            ),
        )

    def _is_devin(self) -> bool:
        provider = self._parsed_model_provider or ""
        return provider == "devin" or (self.model_name or "").startswith("devin/")

    async def _upload_text(
        self, environment: BaseEnvironment, content: str, remote_path: str
    ) -> None:
        with tempfile.NamedTemporaryFile(mode="w", suffix=".tmp", delete=False) as tmp:
            tmp.write(content)
            tmp_path = Path(tmp.name)
        try:
            await environment.upload_file(tmp_path, remote_path)
        finally:
            tmp_path.unlink(missing_ok=True)

    def _is_provider_api_key(self, key: str) -> bool:
        """Check if an environment variable key is a provider API key
        for the current model."""
        provider = (self._parsed_model_provider or "").lower()
        if not provider:
            return False

        key_upper = key.upper()
        # Check override set for this provider
        if key_upper in _PROVIDER_API_KEY_OVERRIDES.get(provider, frozenset()):
            return True

        # Check prefix match: PROVIDER_NAME_API_KEY,
        # PROVIDER_NAME_AUTH_TOKEN, PROVIDER_NAME_TOKEN
        prefix = provider.replace("-", "_").upper() + "_"
        if key_upper.startswith(prefix) and key_upper.endswith(
            ("_API_KEY", "_AUTH_TOKEN", "_TOKEN")
        ):
            return True

        return False

    async def _write_env_file(
        self,
        environment: BaseEnvironment,
        env_vars: dict[str, str],
    ) -> None:
        """Write environment variables to a file on the remote system."""
        remote_path = "/opt/n00n/.env"
        lines = [f"{key}={shlex.quote(value)}\n" for key, value in env_vars.items()]
        content = "".join(lines)
        await self._upload_text(environment, content, remote_path)
        await self.exec_as_root(
            environment,
            command=f"chmod 644 {remote_path}",
        )

    @with_prompt_template
    async def run(
        self,
        instruction: str,
        environment: BaseEnvironment,
        context: AgentContext,
    ) -> None:
        if not self.model_name:
            raise ValueError("Model is required. Pass -m to harbor run.")

        model = self.model_name
        self._last_instruction = instruction
        escaped = shlex.quote(instruction)

        # Build secrets dict: provider-specific keys from os.environ
        # + explicit extra_env keys
        secrets: dict[str, str] = {}
        for key, value in os.environ.items():
            if self._is_provider_api_key(key):
                secrets[key] = value
        secrets.update(
            {
                k: v
                for k, v in self.extra_env.items()
                if k.endswith(("_API_KEY", "_AUTH_TOKEN", "_TOKEN"))
            }
        )

        # Build non-secret env dict with base variables
        env: dict[str, str] = {
            "XDG_CONFIG_HOME": "/opt/n00n/.config",
            "XDG_DATA_HOME": "/opt/n00n/.local/share",
            "XDG_CACHE_HOME": "/opt/n00n/.cache",
            # /opt/n00n must precede /opt/n00n/bin so the n00n wrapper is
            # found before the n00n ELF binary (which relies on the wrapper's
            # bundled loader and libs).
            "PATH": "/opt/n00n:/opt/n00n/bin:/usr/local/bin:/usr/bin:/bin",
        }

        # Append extra PATH if provided
        extra_path = self.extra_env.get("PATH", "")
        if extra_path:
            env["PATH"] = f"{env['PATH']}:{extra_path}"

        # Set Devin CLI env hints for the acp subprocess.
        if self._is_devin():
            env["DEVIN_PERMISSION_MODE"] = "dangerous"
            devin_model = self.model_name
            if "/" in devin_model:
                devin_model = devin_model.split("/", 1)[1]
            if devin_model:
                env["DEVIN_MODEL"] = devin_model

        # Write secrets to file if any exist
        if secrets:
            await self._write_env_file(environment, secrets)

        # Build command with source prefix if secrets exist
        source_prefix = "set -a && . /opt/n00n/.env && set +a && " if secrets else ""
        command = (
            f"{source_prefix}n00n --print --exit-on-done --yolo --verbose "
            f"--output-format stream-json "
            f"--model {shlex.quote(model)} -- {escaped} 2>&1 | "
            f"tee /logs/agent/{AGENT_LOG_FILE}"
        )

        await self.exec_as_agent(environment, command=command, env=env)

    def populate_context_post_run(self, context: AgentContext) -> None:
        log_path = self.logs_dir / AGENT_LOG_FILE
        if not log_path.exists():
            return

        log_text = log_path.read_text(encoding="utf-8", errors="replace")
        if not log_text.strip():
            return

        result = _parse_stream_json(log_text)
        usage = result.get("usage", {}) or {}

        context.n_input_tokens = (
            usage.get("input_tokens", 0)
            + usage.get("cache_read_input_tokens", 0)
            + usage.get("cache_creation_input_tokens", 0)
        )
        context.n_cache_tokens = usage.get("cache_read_input_tokens", 0)
        context.n_output_tokens = usage.get("output_tokens", 0)
        context.cost_usd = result.get("total_cost_usd") or 0.0
        context.metadata = {
            "session_id": result.get("session_id"),
            "model": result.get("model"),
            "duration_ms": result.get("duration_ms"),
            "num_turns": result.get("num_turns"),
            "is_error": result.get("is_error", False),
        }


n00nAgent = N00nAgent
