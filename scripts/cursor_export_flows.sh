#!/usr/bin/env bash
# Export AgentService / interesting Cursor flows from a mitmproxy dump.
# Usage: scripts/cursor_export_flows.sh spikes/cursor-capture-<stamp>
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
MITM="${ROOT}/.tools/mitm/bin/mitmdump"
DUMP_DIR="${1:-}"

if [[ -z "$DUMP_DIR" || ! -f "$DUMP_DIR/flows.mitm" ]]; then
  echo "usage: $0 spikes/cursor-capture-<stamp>" >&2
  exit 1
fi
if [[ ! -x "$MITM" ]]; then
  echo "mitmdump not found. Run: mise run mitm-setup" >&2
  exit 1
fi

OUT="$DUMP_DIR/export"
mkdir -p "$OUT"

# Addon writes raw request/response bodies for agentn Run + unary RPCs.
ADDON="$OUT/_export_addon.py"
cat >"$ADDON" <<'PY'
from pathlib import Path
import re

out_dir = Path(__file__).resolve().parent
idx = 0

INTERESTING = re.compile(
    r"(AgentService/(Run|GetUsableModels)|ServerConfigService/GetServerConfig|GetUsableModels)",
    re.I,
)

def response(flow):
    global idx
    url = flow.request.pretty_url
    if not INTERESTING.search(url) and "agentn." not in url and "api5." not in url:
        return
    idx += 1
    host = flow.request.host.replace(":", "_")
    path = flow.request.path.strip("/").replace("/", "_")[:80]
    base = out_dir / f"{idx:03d}_{host}_{path}"
    meta = base.with_suffix(".meta.txt")
    meta.write_text(
        f"url={url}\n"
        f"method={flow.request.method}\n"
        f"status={flow.response.status_code if flow.response else 'none'}\n"
        f"req_ct={flow.request.headers.get('content-type','')}\n"
        f"resp_ct={flow.response.headers.get('content-type','') if flow.response else ''}\n"
        f"req_len={len(flow.request.content or b'')}\n"
        f"resp_len={len(flow.response.content or b'') if flow.response else 0}\n",
        encoding="utf-8",
    )
    (base.with_suffix(".req.bin")).write_bytes(flow.request.content or b"")
    if flow.response is not None:
        (base.with_suffix(".resp.bin")).write_bytes(flow.response.content or b"")
    print(f"exported {base.name} req={len(flow.request.content or b'')} resp={len(flow.response.content or b'') if flow.response else 0}")
PY

echo "Reading $DUMP_DIR/flows.mitm …”
"$MITM" -n -r "$DUMP_DIR/flows.mitm" -s "$ADDON" --set flow_detail=0 2>&1 | tee "$OUT/export.log" | tail -40

echo
echo "Exports in $OUT:"
ls -la "$OUT"/*.{meta.txt,req.bin,resp.bin} 2>/dev/null | head -40 || ls -la "$OUT" | head -40

# Summarize empty bodies (failed capture)
python3 - <<PY
from pathlib import Path
out = Path("$OUT")
metas = sorted(out.glob("*.meta.txt"))
if not metas:
    print("No interesting flows exported. Re-run capture with the updated script.")
    raise SystemExit(0)
empty = 0
for m in metas:
    text = m.read_text()
    if "req_len=0" in text and "resp_len=0" in text:
        empty += 1
    print(m.name, "→", text.strip().replace("\n", " | "))
print(f"\n{len(metas)} flows, {empty} with both bodies empty")
PY
