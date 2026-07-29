"""Live Cursor Connect capture addon for mitmproxy.

Forces streamed bodies to be retained, logs progress, and writes raw bodies
as soon as each flow completes (so a Ctrl-C still leaves usable artifacts).
"""

from __future__ import annotations

import hashlib
import os
import re
from datetime import datetime, timezone
from pathlib import Path

DUMP_DIR = Path(os.environ.get("N00N_CURSOR_CAPTURE_DIR", ".")).resolve()
BODIES = DUMP_DIR / "bodies"
LIVE_LOG = DUMP_DIR / "live.log"
SUMMARY = DUMP_DIR / "summary.tsv"

INTERESTING = re.compile(
    r"(cursor\.sh|AgentService|ServerConfigService|GetUsableModels|/Run)",
    re.IGNORECASE,
)

_idx = 0


def _ts() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%fZ")


def _log(msg: str) -> None:
    line = f"{_ts()} {msg}"
    print(line, flush=True)
    LIVE_LOG.parent.mkdir(parents=True, exist_ok=True)
    with LIVE_LOG.open("a", encoding="utf-8") as fh:
        fh.write(line + "\n")


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()[:16]


def load(_loader):
    BODIES.mkdir(parents=True, exist_ok=True)
    if not SUMMARY.exists():
        SUMMARY.write_text(
            "idx\tstatus\tmethod\thost\tpath\treq_len\tresp_len\treq_sha\tresp_sha\tcontent_type\n",
            encoding="utf-8",
        )
    _log(f"capture addon ready dump_dir={DUMP_DIR}")


def requestheaders(flow):
    host = flow.request.pretty_host or ""
    if "cursor.sh" not in host:
        return
    # Stream so long-lived AgentService/Run is not buffered before upstream.
    flow.request.stream = True
    _log(f"→ {flow.request.method} {flow.request.pretty_url}")


def responseheaders(flow):
    host = flow.request.pretty_host or ""
    if "cursor.sh" not in host:
        return
    if flow.response is not None:
        flow.response.stream = True
        _log(
            f"← {flow.response.status_code} {flow.request.pretty_url} "
            f"ct={flow.response.headers.get('content-type', '')}"
        )


def response(flow):
    global _idx
    url = flow.request.pretty_url
    host = flow.request.pretty_host or ""
    if not INTERESTING.search(url) and "cursor.sh" not in host:
        return

    req = flow.request.content or b""
    resp = b""
    if flow.response is not None and flow.response.content is not None:
        resp = flow.response.content

    _idx += 1
    path = flow.request.path.strip("/").replace("/", "_")[:80] or "root"
    safe_host = host.replace(":", "_")
    base = BODIES / f"{_idx:03d}_{safe_host}_{path}"
    status = flow.response.status_code if flow.response is not None else 0
    req_ct = flow.request.headers.get("content-type", "")
    resp_ct = (
        flow.response.headers.get("content-type", "")
        if flow.response is not None
        else ""
    )

    meta = (
        f"url={url}\n"
        f"method={flow.request.method}\n"
        f"status={status}\n"
        f"http_version={flow.request.http_version}\n"
        f"req_ct={req_ct}\n"
        f"resp_ct={resp_ct}\n"
        f"req_len={len(req)}\n"
        f"resp_len={len(resp)}\n"
        f"req_sha256_16={_sha256(req)}\n"
        f"resp_sha256_16={_sha256(resp)}\n"
        f"streamed_req={getattr(flow.request, 'stream', None)}\n"
    )
    base.with_suffix(".meta.txt").write_text(meta, encoding="utf-8")
    base.with_suffix(".req.bin").write_bytes(req)
    base.with_suffix(".resp.bin").write_bytes(resp)

    with SUMMARY.open("a", encoding="utf-8") as fh:
        fh.write(
            f"{_idx}\t{status}\t{flow.request.method}\t{host}\t{flow.request.path}\t"
            f"{len(req)}\t{len(resp)}\t{_sha256(req)}\t{_sha256(resp)}\t{resp_ct or req_ct}\n"
        )

    empty = " EMPTY_BODY" if len(req) == 0 and len(resp) == 0 else ""
    run_mark = " ★RUN" if "/Run" in flow.request.path else ""
    _log(
        f"saved {base.name} status={status} req={len(req)} resp={len(resp)}"
        f"{empty}{run_mark}"
    )


def error(flow):
    err = getattr(flow, "error", None)
    _log(f"ERROR {flow.request.pretty_url if flow.request else '?'} {err}")
