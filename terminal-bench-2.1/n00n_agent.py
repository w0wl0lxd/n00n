"""Harbor agent wrapper for running n00n on Terminal-Bench 2.1."""

import asyncio
import json
import os
import shlex
import tempfile
import time
from pathlib import Path
from typing import Any

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
# Do not persist raw ACP traffic inside the wrapper — prompts and tool results
# are untrusted and may contain secrets (AGENTS.md trust boundary).
_DEVIN_WRAPPER = """#!/usr/bin/env python3
import os
import pty
import select
import signal
import subprocess
import sys
import termios
import threading
import tty

REAL = "/opt/n00n/bin/devin-real"


def _info(msg: str) -> None:
    print(f"[devin-wrapper] {msg}", file=sys.stderr, flush=True)


def main() -> int:
    _info("starting devin acp wrapper")

    try:
        master, slave = pty.openpty()
    except OSError as exc:
        _info(f"pty.openpty failed: {exc}")
        return 1

    try:
        tty.setraw(slave, termios.TCSANOW)
    except termios.error as exc:
        _info(f"tty.setraw failed: {exc}")

    # n00n already passes the "acp" subcommand; do not duplicate it.
    argv = [REAL, "--permission-mode", "dangerous"] + sys.argv[1:]
    try:
        p = subprocess.Popen(argv, stdin=slave, stdout=slave, stderr=slave)
    except OSError as exc:
        _info(f"failed to spawn {argv}: {exc}")
        os.close(slave)
        os.close(master)
        return 1

    os.close(slave)
    stop = threading.Event()
    proc: list[subprocess.Popen] = [p]

    def cleanup(signum=None, frame=None):
        _info(f"cleanup triggered (signum={signum})")
        stop.set()
        if proc[0].poll() is None:
            proc[0].terminate()
            try:
                proc[0].wait(timeout=2)
            except subprocess.TimeoutExpired:
                _info("devin-real did not terminate, killing")
                proc[0].kill()
                proc[0].wait()

    signal.signal(signal.SIGTERM, cleanup)
    signal.signal(signal.SIGINT, cleanup)

    def forward_input():
        try:
            fd = sys.stdin.buffer.fileno()
            while not stop.is_set():
                ready, _, _ = select.select([fd], [], [], 0.1)
                if not ready:
                    continue
                data = os.read(fd, 4096)
                if not data:
                    break
                os.write(master, data)
        except OSError as exc:
            _info(f"forward_input error: {exc}")

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
            sys.stdout.buffer.write(data)
            sys.stdout.buffer.flush()
    except OSError as exc:
        _info(f"output loop error: {exc}")
    finally:
        cleanup()

    t.join(timeout=2)
    if p.poll() is None:
        _info("devin-real still running after cleanup")
        p.kill()
        p.wait()
    return p.poll() if p.poll() is not None else 0


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
        # Prefer the wrapper script found on PATH (either /opt/n00n/n00n or a
        # /mnt/n00n copy in /usr/local/bin).
        return "n00n --version"

    _UPLOAD_TIMEOUT_SEC = 120
    _INSTALL_TIMEOUT_SEC = 180
    _SETUP_TIMEOUT_SEC = 60

    async def _timed_upload(
        self,
        environment: BaseEnvironment,
        source: Path,
        target: str,
        label: str,
    ) -> None:
        start = time.monotonic()
        size = source.stat().st_size if source.exists() else 0
        try:
            await asyncio.wait_for(
                environment.upload_file(source, target),
                timeout=self._UPLOAD_TIMEOUT_SEC,
            )
        except asyncio.TimeoutError as exc:
            self.logger.error(f"{label} upload timed out", extra={"size": size})
            raise RuntimeError(f"{label} upload timed out") from exc
        elapsed = time.monotonic() - start
        self.logger.info(
            f"{label} upload complete",
            extra={"size": size, "duration_sec": round(elapsed, 3)},
        )

    async def _timed_exec(
        self,
        method,
        environment: BaseEnvironment,
        command: str,
        timeout_sec: int | None,
        label: str,
    ) -> Any:
        start = time.monotonic()
        try:
            result = await method(environment, command=command, timeout_sec=timeout_sec)
        finally:
            elapsed = time.monotonic() - start
            self.logger.info(
                f"{label} exec finished",
                extra={"duration_sec": round(elapsed, 3)},
            )
        return result

    async def _upload_text(
        self, environment: BaseEnvironment, content: str, remote_path: str
    ) -> None:
        """Upload a small text blob to a remote path."""
        with tempfile.NamedTemporaryFile(mode="w", suffix=".tmp", delete=False) as tmp:
            tmp.write(content)
            tmp_path = Path(tmp.name)
        try:
            await environment.upload_file(tmp_path, remote_path)
        finally:
            tmp_path.unlink(missing_ok=True)

    def _is_devin(self) -> bool:
        provider = self._parsed_model_provider or ""
        return provider == "devin" or (self.model_name or "").startswith("devin/")

    def _is_provider_api_key(self, key: str) -> bool:
        """Check if an environment variable key is a provider API key
        for the current model."""
        provider = (self._parsed_model_provider or "").lower()
        if not provider:
            return False

        key_upper = key.upper()
        if key_upper in _PROVIDER_API_KEY_OVERRIDES.get(provider, frozenset()):
            return True

        prefix = provider.replace("-", "_").upper() + "_"
        if key_upper.startswith(prefix) and key_upper.endswith(
            ("_API_KEY", "_AUTH_TOKEN", "_TOKEN")
        ):
            return True

        return False

    async def install(self, environment: BaseEnvironment) -> None:
        install_start = time.monotonic()
        self.logger.info("n00n install started")

        # Probe for an already-installed n00n binary on PATH.
        probe = await self._timed_exec(
            self.exec_as_root,
            environment,
            command="command -v n00n >/dev/null 2>&1 && n00n --version",
            timeout_sec=self._SETUP_TIMEOUT_SEC,
            label="probe installed n00n",
        )

        if probe.return_code != 0:
            # No n00n in PATH: try a mounted binary first.
            mount_check = await self._timed_exec(
                self.exec_as_root,
                environment,
                command="test -f /mnt/n00n",
                timeout_sec=self._SETUP_TIMEOUT_SEC,
                label="probe mounted n00n",
            )
            if mount_check.return_code == 0:
                await self._timed_exec(
                    self.exec_as_root,
                    environment,
                    command=(
                        "cp /mnt/n00n /usr/local/bin/n00n && "
                        "chmod +x /usr/local/bin/n00n"
                    ),
                    timeout_sec=self._INSTALL_TIMEOUT_SEC,
                    label="copy mounted n00n",
                )

                auth_check = await self._timed_exec(
                    self.exec_as_root,
                    environment,
                    command="test -d /mnt/n00n-auth",
                    timeout_sec=self._SETUP_TIMEOUT_SEC,
                    label="probe mounted auth",
                )
                if auth_check.return_code == 0:
                    await self._timed_exec(
                        self.exec_as_root,
                        environment,
                        command=(
                            "if [ -d /mnt/n00n-auth ] && [ -n "
                            '"$(ls -A /mnt/n00n-auth 2>/dev/null)" ]; then '
                            "mkdir -p /root/.n00n/auth && "
                            "cp -r /mnt/n00n-auth/. /root/.n00n/auth/; "
                            "fi"
                        ),
                        timeout_sec=self._INSTALL_TIMEOUT_SEC,
                        label="copy mounted auth",
                    )

                providers_check = await self._timed_exec(
                    self.exec_as_root,
                    environment,
                    command="test -d /mnt/n00n-providers",
                    timeout_sec=self._SETUP_TIMEOUT_SEC,
                    label="probe mounted providers",
                )
                if providers_check.return_code == 0:
                    await self._timed_exec(
                        self.exec_as_root,
                        environment,
                        command=(
                            "if [ -d /mnt/n00n-providers ] && [ -n "
                            '"$(ls -A /mnt/n00n-providers 2>/dev/null)" ]; then '
                            "mkdir -p /root/.n00n/providers && "
                            "cp -r /mnt/n00n-providers/. /root/.n00n/providers/ && "
                            "chmod -R +x /root/.n00n/providers; "
                            "fi"
                        ),
                        timeout_sec=self._INSTALL_TIMEOUT_SEC,
                        label="copy mounted providers",
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
                await self._timed_upload(
                    environment, bundle_local, "/tmp/n00n-bundle.tar.gz", label="n00n bundle"
                )
                await self._timed_exec(
                    self.exec_as_root,
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
                    timeout_sec=self._INSTALL_TIMEOUT_SEC,
                    label="extract bundle",
                )

        # Ensure isolated XDG state exists and is writable for any user.
        await self._timed_exec(
            self.exec_as_root,
            environment,
            command=(
                "mkdir -p /opt/n00n/.config /opt/n00n/.local/share "
                "/opt/n00n/.cache "
                "&& chmod -R a+rwX,+t /opt/n00n/.config /opt/n00n/.local "
                "/opt/n00n/.cache"
            ),
            timeout_sec=self._SETUP_TIMEOUT_SEC,
            label="prepare XDG dirs",
        )

        # Only replace the bundled /opt/n00n/bin/devin when the matching
        # devin-real binary was unpacked beside it. System /mnt installs do not
        # provide that path, and uploading a wrapper that points at a missing
        # REAL executable would break Devin jobs that prefer /opt/n00n/bin.
        real_check = await self._timed_exec(
            self.exec_as_root,
            environment,
            command="test -x /opt/n00n/bin/devin-real",
            timeout_sec=self._SETUP_TIMEOUT_SEC,
            label="probe devin-real",
        )
        if real_check.return_code == 0:
            self.logger.info("uploading devin pty wrapper")
            with tempfile.NamedTemporaryFile(mode="w", suffix=".py", delete=False) as tmp:
                tmp.write(_DEVIN_WRAPPER)
                wrapper_path = Path(tmp.name)
            try:
                await self._timed_upload(
                    environment, wrapper_path, "/opt/n00n/bin/devin", label="devin wrapper"
                )
            finally:
                wrapper_path.unlink(missing_ok=True)
            await self._timed_exec(
                self.exec_as_root,
                environment,
                command="chmod +x /opt/n00n/bin/devin",
                timeout_sec=self._SETUP_TIMEOUT_SEC,
                label="chmod wrapper",
            )

        # Devin needs a credentials file and a minimal config so it never prompts.
        windsurf_key = self._get_env("WINDSURF_API_KEY")
        if self._is_devin() and windsurf_key:
            await self._write_devin_config(environment, windsurf_key)

        elapsed = time.monotonic() - install_start
        self.logger.info(
            "n00n install complete",
            extra={"duration_sec": round(elapsed, 3)},
        )

    async def _write_devin_config(
        self, environment: BaseEnvironment, windsurf_api_key: str
    ) -> None:
        config_dir = "/opt/n00n/.config/devin"
        data_dir = "/opt/n00n/.local/share/devin"
        await self._timed_exec(
            self.exec_as_root,
            environment,
            command=f"mkdir -p {config_dir} {data_dir}",
            timeout_sec=self._SETUP_TIMEOUT_SEC,
            label="prepare devin config dirs",
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
        await self._upload_text(environment, credentials_toml, f"{data_dir}/credentials.toml")

        await self._timed_exec(
            self.exec_as_root,
            environment,
            command=(
                f"chmod -R a+rwX,+t {config_dir} {data_dir} "
                f"&& chmod 644 {data_dir}/credentials.toml "
                f"&& chmod 644 {config_dir}/config.json"
            ),
            timeout_sec=self._SETUP_TIMEOUT_SEC,
            label="set devin config permissions",
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
        self.logger.info(
            "n00n run configured",
            extra={
                "model": model,
                "devin_model": devin_model,
                "instruction_chars": len(instruction),
            },
        )

        # Build secrets dict: provider-specific keys from os.environ
        # + explicit extra_env keys.
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

        # Base environment for the n00n process.  PATH is ordered so the
        # bundled /opt/n00n wrapper is preferred when present, falling back to
        # /usr/local/bin (e.g. a /mnt/n00n install).
        env: dict[str, str] = {
            "PATH": "/opt/n00n:/opt/n00n/bin:/usr/local/bin:/usr/bin:/bin",
            "XDG_CONFIG_HOME": "/opt/n00n/.config",
            "XDG_DATA_HOME": "/opt/n00n/.local/share",
            "XDG_CACHE_HOME": "/opt/n00n/.cache",
            # Keep provider logs at warning level so raw ACP traffic is not
            # persisted to the sandbox log via tee.
            "RUST_LOG": "warn,n00n=info",
            "RUST_BACKTRACE": "1",
        }

        extra_path = self.extra_env.get("PATH", "")
        if extra_path:
            env["PATH"] = f"{env['PATH']}:{extra_path}"

        if self._is_devin():
            env["DEVIN_PERMISSION_MODE"] = "dangerous"
            if devin_model:
                env["DEVIN_MODEL"] = devin_model

        # Merge provider API keys and tokens into the process environment.
        env.update(secrets)

        command = (
            f"n00n --print --yolo --verbose "
            f"--output-format stream-json "
            f"--model {shlex.quote(model)} -- {escaped} "
            f"2>&1 | tee /logs/agent/{AGENT_LOG_FILE}"
        )

        run_start = time.monotonic()
        self.logger.info("n00n run started", extra={"model": model})
        try:
            await self.exec_as_agent(environment, command=command, env=env)
        finally:
            elapsed = time.monotonic() - run_start
            self.logger.info(
                "n00n run finished",
                extra={"model": model, "duration_sec": round(elapsed, 3)},
            )

    def populate_context_post_run(self, context: AgentContext) -> None:
        log_path = self.logs_dir / AGENT_LOG_FILE
        if not log_path.exists():
            self.logger.warning(
                "n00n output log not found", extra={"log_path": str(log_path)}
            )
            return

        log_text = log_path.read_text(encoding="utf-8", errors="replace")
        if not log_text.strip():
            self.logger.warning("n00n output log is empty")
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
        self.logger.info(
            "n00n post-run summary parsed",
            extra={
                "session_id": context.metadata["session_id"],
                "model": context.metadata["model"],
                "duration_ms": context.metadata["duration_ms"],
                "num_turns": context.metadata["num_turns"],
                "is_error": context.metadata["is_error"],
                "n_input_tokens": context.n_input_tokens,
                "n_output_tokens": context.n_output_tokens,
                "cost_usd": context.cost_usd,
            },
        )


# Harbor's -a import path uses the literal class name; provide a lowercase alias
# so `n00n_agent:n00nAgent` works alongside the conventional `N00nAgent`.
n00nAgent = N00nAgent
