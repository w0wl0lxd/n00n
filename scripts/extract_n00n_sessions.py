#!/usr/bin/env python3
"""Extract and summarize n00n session events (zstd JSONL).

Replaces the stale tool_token_analysis.py path which expects plain JSON.
Handles the real storage format: each session is zstd-compressed JSONL
with multiple concatenated frames (n00n-storage/src/sessions.rs:1).

Usage:
  python3 scripts/extract_n00n_sessions.py [--latest N] [--grep ERROR]
  python3 scripts/extract_n00n_sessions.py --session CeBtkhCHt5GCb7Qbf5utf
  python3 scripts/extract_n00n_sessions.py --all --json > /tmp/events.json

Requires: zstd binary on PATH (written via n00n-storage), python3.
"""

import argparse
import json
import subprocess
import sys
from collections import Counter
from pathlib import Path

SESSION_DIR = Path.home() / ".local" / "state" / "n00n" / "sessions"


def decompress(path: Path) -> str:
    out = subprocess.run(
        ["zstd", "-d", "-c", str(path)], capture_output=True, check=False
    )
    if out.returncode != 0:
        return ""
    return out.stdout.decode("utf-8", errors="replace")


def parse(path: Path):
    txt = decompress(path)
    for line in txt.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            yield json.loads(line)
        except json.JSONDecodeError:
            continue


def summarize(path: Path):
    counts: Counter = Counter()
    tool_names: Counter = Counter()
    errors: list[str] = []
    for rec in parse(path):
        t = rec.get("t", "?")
        counts[t] += 1
        if t == "msg":
            for block in rec.get("d", {}).get("content", []):
                if block.get("type") == "tool_use":
                    tool_names[block.get("name", "?")] += 1
                if block.get("type") == "tool_result":
                    c = block.get("content", "")
                    low = c.lower()
                    if (
                        "error" in low
                        or "failed" in low
                        or "panic" in low
                        or "runtime error" in low
                    ):
                        errors.append(c[:800])
    return counts, tool_names, errors


def main() -> int:
    ap = argparse.ArgumentParser(description="Extract n00n session events")
    ap.add_argument("--session", help="Single session id (without .jsonl)")
    ap.add_argument("--all", action="store_true", help="Process all sessions")
    ap.add_argument(
        "--latest",
        type=int,
        default=5,
        help="Number of latest sessions to summarize (default 5)",
    )
    ap.add_argument(
        "--grep", help="Filter error lines containing substring (case-insensitive)"
    )
    ap.add_argument("--json", action="store_true", help="Emit raw JSON array to stdout")
    args = ap.parse_args()

    if args.session:
        paths = [SESSION_DIR / f"{args.session}.jsonl"]
    elif args.all:
        paths = sorted(SESSION_DIR.glob("*.jsonl"))
    else:
        paths = sorted(
            SESSION_DIR.glob("*.jsonl"), key=lambda p: p.stat().st_mtime, reverse=True
        )[: args.latest]

    if args.json:
        out = []
        for p in paths:
            for rec in parse(p):
                rec["_session"] = p.stem
                out.append(rec)
        json.dump(out, sys.stdout, ensure_ascii=False)
        sys.stdout.write("\n")
        return 0

    total = 0
    for path in paths:
        if not path.exists():
            print(f"missing: {path}", file=sys.stderr)
            continue
        counts, tools, errors = summarize(path)
        print(f"\n== {path.name}  size={path.stat().st_size} counts={dict(counts)} ==")
        if tools:
            print(f"  tools: {tools.most_common(10)}")
        if errors:
            filtered = [
                e for e in errors if not args.grep or args.grep.lower() in e.lower()
            ]
            print(f"  errors ({len(filtered)}/{len(errors)} shown):")
            for e in filtered[:10]:
                print(f"    - {e[:400].replace(chr(10), ' | ')}")
        total += 1
    print(f"\nSummarized {total} session(s) in {SESSION_DIR}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
