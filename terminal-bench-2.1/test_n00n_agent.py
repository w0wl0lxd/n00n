import ast
import subprocess
import sys
import tempfile
from pathlib import Path


SOURCE = Path(__file__).with_name("n00n_agent.py")


def wrapper_source() -> str:
    tree = ast.parse(SOURCE.read_text(encoding="utf-8"))
    for node in tree.body:
        if isinstance(node, ast.Assign) and any(
            isinstance(target, ast.Name) and target.id == "_DEVIN_WRAPPER"
            for target in node.targets
        ):
            return ast.literal_eval(node.value)
    raise AssertionError("wrapper source not found")


def test_wrapper_does_not_persist_acp_transcript():
    source = wrapper_source()
    assert "/tmp/devin-acp.log" not in source
    assert "log.write" not in source


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
            returncode = process.wait(timeout=2)
            stdout = process.stdout.read() if process.stdout is not None else b""
        finally:
            if process.stdin is not None:
                process.stdin.close()
            if process.poll() is None:
                process.kill()
                process.wait()

        assert returncode == 0
        assert b"response" in stdout
