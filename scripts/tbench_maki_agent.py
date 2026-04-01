"""
Harbor agent wrapper for running maki on Terminal-Bench.

Requires: uv tool install harbor

Setup:
    harbor dataset download terminal-bench/terminal-bench-2

Run a single task:
    MOUNTS='["/usr/local/bin/maki:/mnt/maki:ro", "~/.maki/auth:/mnt/maki-auth:ro", "~/.maki/providers:/mnt/maki-providers:ro"]'

    harbor run \
      -t terminal-bench/fix-git \
      -m anthropic/claude-sonnet-4-6 \
      --agent-import-path tbench_maki_agent:MakiAgent \
      --mounts-json "$MOUNTS" \
      -n 1 -y

Run the full suite:
    harbor run \
      -d terminal-bench/terminal-bench-2 \
      -m anthropic/claude-sonnet-4-6 \
      --agent-import-path tbench_maki_agent:MakiAgent \
      --mounts-json "$MOUNTS" \
      -n 4 -y

Expand ~ in MOUNTS to your actual home directory if your shell does not
expand inside single quotes.
"""

import shlex

from harbor.agents.installed.base import BaseInstalledAgent, with_prompt_template  # ty: ignore[unresolved-import]
from harbor.environments.base import BaseEnvironment  # ty: ignore[unresolved-import]
from harbor.models.agent.context import AgentContext  # ty: ignore[unresolved-import]


class MakiAgent(BaseInstalledAgent):
    @staticmethod
    def name() -> str:
        return "maki"

    def get_version_command(self) -> str | None:
        return "maki --version"

    async def install(self, environment: BaseEnvironment) -> None:
        await self.exec_as_root(
            environment,
            command="cp /mnt/maki /usr/local/bin/maki && chmod +x /usr/local/bin/maki && maki --version",
        )
        await self.exec_as_root(
            environment,
            command="mkdir -p /root/.maki/auth /root/.maki/providers && cp /mnt/maki-auth/* /root/.maki/auth/ && cp /mnt/maki-providers/* /root/.maki/providers/ && chmod +x /root/.maki/providers/*",
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

        escaped = shlex.quote(instruction)
        await self.exec_as_agent(
            environment,
            command=(
                f"maki --print --yolo --output-format json --model {self.model_name} "
                f"-- {escaped} 2>&1 </dev/null | tee /logs/agent/maki.txt"
            ),
        )

    def populate_context_post_run(self, context: AgentContext) -> None:
        pass
