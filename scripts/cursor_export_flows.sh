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
    req = flow.request.content or b""
    resp = b""
    if flow.response is not None and flow.response.content is not None:
        resp = flow.response.content
    meta = (
        f"url={url}\n"
        f"method={flow.request.method}\n"
        f"status={flow.response.status_code if flow.response else 'none'}\n"
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
  2>&1 | tee "$OUT/export.log" | tail -40

echo
echo "Exports in $OUT:"
ls -la "$OUT" | head -40

echo
echo "Summary:"
for m in "$OUT"/*.meta.txt; do
  [[ -f "$m" ]] || continue
  echo "$(basename "$m"): $(tr '\n' ' ' <"$m")"
done
empty=0
total=0
for m in "$OUT"/*.meta.txt; do
  [[ -f "$m" ]] || continue
  total=$((total + 1))
  if grep -q 'req_len=0' "$m" && grep -q 'resp_len=0' "$m"; then
    empty=$((empty + 1))
  fi
done
echo "$total flows, $empty with both bodies empty"
if [[ "$total" -eq 0 ]]; then
  echo "No interesting flows. Re-run capture with the updated scripts/cursor_capture.sh"
fi
