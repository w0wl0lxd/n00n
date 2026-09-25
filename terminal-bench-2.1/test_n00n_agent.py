import ast
import asyncio
import importlib.util
import logging
import stat
import subprocess
import sys
import tempfile
import types
from pathlib import Path


SOURCE = Path(__file__).with_name("n00n_agent.py")
CONTAINER_PATHS = ("/opt/n00n", "/mnt/", "/usr/local/bin")
PATH_PROBE_COMMAND = "command -v n00n >/dev/null 2>&1 && n00n --version"
AUTH_FILE_NAME = "credentials.json"
PRIVATE_MODE_BITS = stat.S_IRWXG | stat.S_IRWXO


def wrapper_source() -> str:
    tree = ast.parse(SOURCE.read_text(encoding="utf-8"))
    for node in tree.body:
        if isinstance(node, ast.Assign) and any(
            isinstance(target, ast.Name) and target.id == "_DEVIN_WRAPPER"
            for target in node.targets
        ):
            return ast.literal_eval(node.value)
    raise AssertionError("wrapper source not found")


def _iter_wrapper_ast() -> ast.AST:
    source = wrapper_source()
    return ast.parse(source)


def test_wrapper_does_not_persist_acp_transcript():
    source = wrapper_source()
    assert "/tmp/devin-acp.log" not in source
    assert "log.write" not in source

    for node in ast.walk(_iter_wrapper_ast()):
        if not isinstance(node, ast.Call):
            continue
        if isinstance(node.func, ast.Name) and node.func.id == "open":
            raise AssertionError("wrapper calls open()")
        if isinstance(node.func, ast.Attribute) and node.func.attr == "open":
            raise AssertionError("wrapper calls .open()")
        if (
            isinstance(node.func, ast.Attribute)
            and node.func.attr == "write"
            and isinstance(node.func.value, ast.Name)
            and node.func.value.id not in ("sys", "os")
        ):
            raise AssertionError(f"wrapper calls {node.func.value.id}.write()")


def test_wrapper_exits_when_devin_exits_with_stdin_open():
    with tempfile.TemporaryDirectory() as directory:
        directory_path = Path(directory)
        fake_devin = directory_path / "devin-real"
        fake_devin.write_text(
            "#!/bin/sh\nprintf 'response\\n'\n",
            encoding="utf-8",
        )
        fake_devin.chmod(0o755)
        wrapper = directory_path / "devin"
        source = wrapper_source().replace(
            'REAL = "/opt/n00n/bin/devin-real"',
            f"REAL = {str(fake_devin)!r}",
        )
        wrapper.write_text(source, encoding="utf-8")

        process = subprocess.Popen(
            [sys.executable, str(wrapper)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            returncode = process.wait(timeout=30)
            stdout = process.stdout.read() if process.stdout is not None else b""
        finally:
            if process.stdin is not None:
                process.stdin.close()
            if process.poll() is None:
                process.kill()
                process.wait()

        assert returncode == 0
        assert b"response" in stdout


def _load_agent_module() -> types.ModuleType:
    stubs = {
        "harbor": types.ModuleType("harbor"),
        "harbor.agents": types.ModuleType("harbor.agents"),
        "harbor.agents.installed": types.ModuleType("harbor.agents.installed"),
        "harbor.agents.installed.base": types.ModuleType(
            "harbor.agents.installed.base"
        ),
        "harbor.environments": types.ModuleType("harbor.environments"),
        "harbor.environments.base": types.ModuleType("harbor.environments.base"),
        "harbor.models": types.ModuleType("harbor.models"),
        "harbor.models.agent": types.ModuleType("harbor.models.agent"),
        "harbor.models.agent.context": types.ModuleType("harbor.models.agent.context"),
    }
    stubs["harbor.agents.installed.base"].BaseInstalledAgent = object
    stubs["harbor.agents.installed.base"].with_prompt_template = lambda func: func
    stubs["harbor.environments.base"].BaseEnvironment = object
    stubs["harbor.models.agent.context"].AgentContext = object
    sys.modules.update(stubs)
    spec = importlib.util.spec_from_file_location("n00n_agent_under_test", SOURCE)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class _LocalContainer:
    """Runs install() shell commands against a temp dir standing in for /."""

    def __init__(self, root: Path) -> None:
        self.root = root

    def rewrite(self, command: str) -> str:
        for path in CONTAINER_PATHS:
            command = command.replace(path, f"{self.root}{path}")
        return command

    async def exec(self, environment, command: str, timeout_sec=None, **_kwargs):
        if command == PATH_PROBE_COMMAND:
            return types.SimpleNamespace(return_code=1)
        process = await asyncio.create_subprocess_exec(
            "sh",
            "-c",
            self.rewrite(command),
            stdout=asyncio.subprocess.DEVNULL,
            stderr=asyncio.subprocess.DEVNULL,
        )
        return types.SimpleNamespace(return_code=await process.wait())


def test_install_keeps_mounted_auth_private():
    module = _load_agent_module()
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        (root / "mnt" / "n00n-auth").mkdir(parents=True)
        (root / "mnt" / "n00n-auth" / AUTH_FILE_NAME).write_text("{}", encoding="utf-8")
        (root / "mnt" / "n00n").write_text("#!/bin/sh\n", encoding="utf-8")
        (root / "usr" / "local" / "bin").mkdir(parents=True)
        container = _LocalContainer(root)

        agent = object.__new__(module.N00nAgent)
        agent.logger = logging.getLogger("n00n-agent-test")
        agent.model_name = "anthropic/model"
        agent._parsed_model_provider = "anthropic"
        agent._get_env = lambda _key: None
        agent.exec_as_root = container.exec
        agent.exec_as_agent = container.exec
        asyncio.run(agent.install(environment=None))

        auth_dir = root / "opt" / "n00n" / ".local" / "state" / "n00n" / "auth"
        state_dir = root / "opt" / "n00n" / ".local" / "state"
        assert auth_dir.stat().st_mode & PRIVATE_MODE_BITS == 0
        assert (auth_dir / AUTH_FILE_NAME).stat().st_mode & PRIVATE_MODE_BITS == 0
        assert state_dir.stat().st_mode & stat.S_IWOTH
