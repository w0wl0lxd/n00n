#!/usr/bin/env bash
# Export + validate Cursor capture dumps.
# Usage: scripts/cursor_export_flows.sh spikes/cursor-capture-<stamp>
# Exit 0 only when at least one AgentService/Run flow has non-empty req or resp.
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
MITM="${ROOT}/.tools/mitm/bin/mitmdump"
DUMP_DIR="${1:-}"

die() {
  echo "error: $*" >&2
  exit 1
}

[[ -n $DUMP_DIR ]] || die "usage: $0 spikes/cursor-capture-<stamp>"
[[ -f "$DUMP_DIR/flows.mitm" ]] || die "missing $DUMP_DIR/flows.mitm"
[[ -x $MITM ]] || die "mitmdump not found — run: mise run mitm-setup"

OUT="$DUMP_DIR/export"
mkdir -p "$OUT"

# Prefer live bodies/ written by the capture addon; also re-export from flows.mitm.
if [[ -d "$DUMP_DIR/bodies" ]]; then
  cp -a "$DUMP_DIR/bodies/." "$OUT/" 2>/dev/null || true
fi

ADDON="$OUT/_export_addon.py"
cat >"$ADDON" <<'PY'
from pathlib import Path
import re

out_dir = Path(__file__).resolve().parent
idx = max(
    [int(p.name[:3]) for p in out_dir.glob("*.meta.txt") if p.name[:3].isdigit()]
    + [0]
)
INTERESTING = re.compile(
    r"(AgentService/(Run|GetUsableModels)|ServerConfigService/GetServerConfig|GetUsableModels)",
    re.I,
)

def response(flow):
    global idx
    url = flow.request.pretty_url
    host = flow.request.host or ""
    if not INTERESTING.search(url) and "agentn." not in host and "api5." not in host:
        return
    idx += 1
    path = flow.request.path.strip("/").replace("/", "_")[:80] or "root"
    safe_host = host.replace(":", "_")
    base = out_dir / f"{idx:03d}_{safe_host}_{path}"
    if base.with_suffix(".meta.txt").exists():
        return
    req = flow.request.content or b""
    resp = b""
    if flow.response is not None and flow.response.content is not None:
        resp = flow.response.content
    status = flow.response.status_code if flow.response is not None else "none"
    meta = (
        f"url={url}\n"
        f"method={flow.request.method}\n"
        f"status={status}\n"
        f"req_ct={flow.request.headers.get('content-type', '')}\n"
        f"resp_ct={flow.response.headers.get('content-type', '') if flow.response else ''}\n"
        f"req_len={len(req)}\n"
        f"resp_len={len(resp)}\n"
    )
    base.with_suffix(".meta.txt").write_text(meta, encoding="utf-8")
    base.with_suffix(".req.bin").write_bytes(req)
    base.with_suffix(".resp.bin").write_bytes(resp)
    print(f"exported {base.name} req={len(req)} resp={len(resp)}")
PY

echo "Reading $DUMP_DIR/flows.mitm ..."
"$MITM" -n -r "$DUMP_DIR/flows.mitm" -s "$ADDON" --set flow_detail=0 \
  --set store_streamed_bodies=true \
  2>&1 | tee "$OUT/export.log" | tail -60

echo
echo "=== export directory ==="
ls -la "$OUT" | head -50

echo
echo "=== per-flow summary ==="
run_ok=0
total=0
empty=0
for m in "$OUT"/*.meta.txt; do
  [[ -f $m ]] || continue
  total=$((total + 1))
  req_len=$(sed -n 's/^req_len=//p' "$m" | head -1)
  resp_len=$(sed -n 's/^resp_len=//p' "$m" | head -1)
  url=$(sed -n 's/^url=//p' "$m" | head -1)
  echo "$(basename "$m"): req=$req_len resp=$resp_len url=$url"
  if [[ ${req_len:-0} -eq 0 && ${resp_len:-0} -eq 0 ]]; then
    empty=$((empty + 1))
  fi
  if [[ $url == *"/AgentService/Run"* ]] && { [[ ${req_len:-0} -gt 0 ]] || [[ ${resp_len:-0} -gt 0 ]]; }; then
    run_ok=$((run_ok + 1))
  fi
done

echo
echo "totals: flows=$total empty_both=$empty run_with_body=$run_ok"

REPORT="$DUMP_DIR/VALIDATION.txt"
{
  echo "validated_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "flows=$total"
  echo "empty_both=$empty"
  echo "run_with_body=$run_ok"
  if [[ $run_ok -ge 1 ]]; then
    echo "result=PASS"
  else
    echo "result=FAIL"
    echo "reason=no AgentService/Run flow with non-empty body"
  fi
} | tee "$REPORT"

if [[ $run_ok -lt 1 ]]; then
  echo >&2
  echo "FAIL: need a non-empty AgentService/Run body." >&2
  echo "Re-run: scripts/cursor_capture.sh && scripts/cursor_agent_proxied.sh -p --model auto 'pong'" >&2
  exit 1
fi

echo "PASS: captured usable AgentService/Run bodies"
