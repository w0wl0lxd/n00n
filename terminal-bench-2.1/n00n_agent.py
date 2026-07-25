"""Harbor agent wrapper for running n00n on Terminal-Bench 2.1."""

import json
import shlex
from pathlib import Path

from harbor.agents.installed.base import BaseInstalledAgent, with_prompt_template
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext

AGENT_LOG_FILE = "n00n.txt"


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


class n00nAgent(BaseInstalledAgent):
    """Runs n00n in headless --print mode inside a Harbor environment."""

    _last_instruction: str = ""

    @staticmethod
    def name() -> str:
        return "n00n"

    def get_version_command(self) -> str | None:
        return "export PATH=/opt/n00n/bin:$PATH && devin --version && n00n --version"

    async def install(self, environment: BaseEnvironment) -> None:
        # Upload the bundled n00n binary (glibc-linked with its own loader/libs).
        bundle_local = Path(__file__).with_name("n00n-bundle.tar.gz")
        await environment.upload_file(bundle_local, "/tmp/n00n-bundle.tar.gz")

        await self.exec_as_root(
            environment,
            command=(
                "mkdir -p /opt/n00n "
                "&& tar -xzf /tmp/n00n-bundle.tar.gz -C /opt/n00n --strip-components=1 "
                "&& chmod -R a+rX /opt/n00n "
                "&& ln -sf /opt/n00n/n00n /usr/local/bin/n00n "
                "&& rm -f /tmp/n00n-bundle.tar.gz"
            ),
        )

        # The n00n-bundle already contains a static-pie Devin CLI at /opt/n00n/bin/devin.

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
        env = {
            "DEVIN_API_KEY": self._get_env("DEVIN_API_KEY") or "",
            "DEVIN_MODEL": devin_model,
            "DEVIN_PERMISSION_MODE": "dangerous",
            "WINDSURF_API_KEY": self._get_env("WINDSURF_API_KEY") or "",
        }

        command = (
            f'export PATH="/opt/n00n/bin:$PATH"; '
            f"/opt/n00n/n00n --print --yolo --verbose --output-format stream-json "
            f"--model {shlex.quote(model)} -- {escaped} 2>&1 | tee /logs/agent/{AGENT_LOG_FILE}"
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
