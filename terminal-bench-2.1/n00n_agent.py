"""Harbor agent wrapper for running n00n on Terminal-Bench 2.1."""

import json
import shlex
import tempfile
from pathlib import Path

from harbor.agents.installed.base import BaseInstalledAgent, with_prompt_template
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext

AGENT_LOG_FILE = "n00n.txt"

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
        return "export PATH=/opt/n00n/bin:$PATH && n00n --version"

    async def install(self, environment: BaseEnvironment) -> None:
        # Upload the bundled n00n binary (glibc-linked with its own loader/libs).
        bundle_local = Path(__file__).with_name("n00n-bundle.tar.gz")
        await environment.upload_file(bundle_local, "/tmp/n00n-bundle.tar.gz")

        await self.exec_as_root(
            environment,
            command=(
                "mkdir -p /opt/n00n /opt/n00n/.config "
                "/opt/n00n/.local/share /opt/n00n/.cache "
                "&& tar -xzf /tmp/n00n-bundle.tar.gz -C /opt/n00n --strip-components=1 "
                "&& chmod -R a+rX /opt/n00n "
                "&& ln -sf /opt/n00n/n00n /usr/local/bin/n00n "
                "&& rm -f /tmp/n00n-bundle.tar.gz"
            ),
        )

        # devin-real block-buffers stdout when run through a pipe; replace the
        # bundled wrapper with one that runs devin-real under a pseudo-tty.
        with tempfile.NamedTemporaryFile(mode="w", suffix=".py", delete=False) as tmp:
            tmp.write(_DEVIN_WRAPPER)
            wrapper_path = Path(tmp.name)
        try:
            await environment.upload_file(wrapper_path, "/opt/n00n/bin/devin")
        finally:
            wrapper_path.unlink(missing_ok=True)

        await self.exec_as_root(
            environment,
            command="chmod +x /opt/n00n/bin/devin",
        )

        # The Devin CLI bundled with n00n stores Windsurf credentials in a TOML file.
        # Write that credential file so the isolated XDG dirs contain a valid API key
        # and server endpoints, matching what the Devin CLI expects when running ACP.
        api_key = self._get_env("WINDSURF_API_KEY") or ""
        if api_key:
            config_json = '{"shell":{"setup_complete":true}}'
            credentials_toml = (
                f"api_key = {json.dumps(api_key)}\n"
                f"windsurf_api_key = {json.dumps(api_key)}\n"
                'api_server_url = "https://server.codeium.com"\n'
                'devin_webapp_host = "app.devin.ai"\n'
                'devin_api_url = "https://api.devin.ai"\n'
            )

            files = {
                "/opt/n00n/.config/devin/config.json": config_json,
                "/opt/n00n/.local/share/devin/credentials.toml": credentials_toml,
            }
            for remote, content in files.items():
                with tempfile.NamedTemporaryFile(
                    mode="w", suffix=Path(remote).suffix, delete=False
                ) as tmp:
                    tmp.write(content)
                    tmp_path = Path(tmp.name)
                try:
                    await environment.upload_file(tmp_path, remote)
                finally:
                    tmp_path.unlink(missing_ok=True)

            await self.exec_as_root(
                environment,
                command=(
                    "mkdir -p /opt/n00n/.config/devin "
                    "/opt/n00n/.local/share/devin "
                    "&& chmod -R a+rwx /opt/n00n/.config "
                    "/opt/n00n/.local /opt/n00n/.cache "
                    "&& chmod 644 /opt/n00n/.local/share/devin/credentials.toml "
                    "&& chmod 644 /opt/n00n/.config/devin/config.json"
                ),
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
        if "/" not in model:
            model = f"devin/{model}"

        self._last_instruction = instruction
        escaped = shlex.quote(instruction)

        devin_model = model.split("/", 1)[1] if "/" in model else model
        env: dict[str, str] = {
            "DEVIN_MODEL": devin_model,
            "DEVIN_PERMISSION_MODE": "dangerous",
            "WINDSURF_API_KEY": self._get_env("WINDSURF_API_KEY") or "",
            "XDG_CONFIG_HOME": "/opt/n00n/.config",
            "XDG_DATA_HOME": "/opt/n00n/.local/share",
            "XDG_CACHE_HOME": "/opt/n00n/.cache",
        }
        if devin_api_key := self._get_env("DEVIN_API_KEY"):
            env["DEVIN_API_KEY"] = devin_api_key

        command = (
            f'export PATH="/opt/n00n/bin:$PATH"; '
            f"/opt/n00n/n00n --print --yolo --verbose "
            f"--output-format stream-json "
            f"--model {shlex.quote(model)} -- {escaped} "
            f"2>&1 | tee /logs/agent/{AGENT_LOG_FILE}"
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


# Harbor's -a import path uses the literal class name; provide a lowercase alias
# so `n00n_agent:n00nAgent` works alongside the conventional `N00nAgent`.
n00nAgent = N00nAgent
