#!/usr/bin/env python3
"""Generate n00n-providers/src/providers/cursor_models.rs from cursor-agent --list-models.

Usage:
    cursor-agent --list-models > /tmp/cursor-models.txt
    python3 scripts/gen_cursor_models.py /tmp/cursor-models.txt

If no path is given, it reads from /tmp/cursor-models.txt.
"""

import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
OUT = REPO_ROOT / "n00n-providers" / "src" / "providers" / "cursor_models.rs"
MODELS_TXT = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("/tmp/cursor-models.txt")

# (input_price, output_price, tier, context_window, max_output_tokens)
BASE = {
    "gpt-5.4-nano": (0.20, 1.25, "Weak", 1_000_000, 128_000),
    "gpt-5.4-mini": (0.75, 4.50, "Weak", 1_000_000, 128_000),
    "gpt-5-mini": (0.25, 2.00, "Weak", 128_000, 32_768),
    "gpt-5.6-luna": (1.00, 6.00, "Weak", 1_000_000, 128_000),
    "gemini-3.6-flash": (1.50, 7.50, "Weak", 1_000_000, 128_000),
    "gemini-3-flash": (0.50, 3.00, "Weak", 1_000_000, 128_000),
    "gpt-5.6-terra": (2.50, 15.00, "Medium", 1_000_000, 128_000),
    "gemini-3.5-flash": (1.50, 9.00, "Medium", 1_000_000, 128_000),
    "gpt-5.1-codex-mini": (0.25, 2.00, "Weak", 272_000, 128_000),
    "gpt-5.1-codex": (1.25, 10.00, "Strong", 272_000, 128_000),
    "gpt-5.3-codex": (1.75, 14.00, "Strong", 272_000, 128_000),
    "gpt-5.2-codex": (1.75, 14.00, "Strong", 272_000, 128_000),
    "gpt-5.2": (1.75, 14.00, "Strong", 272_000, 128_000),
    "gpt-5.1": (1.25, 10.00, "Medium", 200_000, 128_000),
    "gpt-5.5": (5.00, 30.00, "Strong", 1_000_000, 128_000),
    "gpt-5.4": (2.50, 15.00, "Strong", 1_000_000, 128_000),
    "gpt-5.6-sol": (5.00, 30.00, "Strong", 1_000_000, 128_000),
    "composer-2.5": (0.50, 2.50, "Strong", 128_000, 32_768),
    "cursor-grok-4.5": (2.00, 6.00, "Strong", 128_000, 32_768),
    "claude-opus-5": (5.00, 25.00, "Strong", 1_000_000, 128_000),
    "claude-opus-4-8": (5.00, 25.00, "Strong", 1_000_000, 128_000),
    "claude-opus-4-7": (5.00, 25.00, "Strong", 1_000_000, 128_000),
    "claude-4.6-opus": (5.00, 25.00, "Strong", 1_000_000, 128_000),
    "claude-4.5-opus": (5.00, 25.00, "Strong", 200_000, 32_768),
    "claude-4.6-sonnet": (6.00, 22.50, "Strong", 1_000_000, 128_000),
    "claude-4.5-sonnet": (3.00, 15.00, "Medium", 200_000, 32_768),
    "claude-4-sonnet": (3.00, 15.00, "Medium", 200_000, 32_768),
    "claude-sonnet-5": (3.00, 15.00, "Strong", 1_000_000, 128_000),
    "claude-fable-5": (10.00, 50.00, "Strong", 1_000_000, 128_000),
    "gemini-3.1-pro": (2.00, 12.00, "Strong", 1_000_000, 128_000),
    "kimi-k2.7-code": (0.95, 4.00, "Medium", 200_000, 32_768),
    "glm-5.2": (1.40, 4.40, "Medium", 128_000, 32_768),
    "auto": (1.25, 6.00, "Strong", 128_000, 32_768),
}

# Sort longest first so the most specific base wins on prefix match.
BASE_KEYS = sorted(BASE.keys(), key=lambda k: (-len(k), k))

DEFAULTS = {
    "Weak": "gemini-3-flash",
    "Medium": "gpt-5.1",
    "Strong": "composer-2.5",
}


def classify(model_id: str):
    for key in BASE_KEYS:
        if model_id.startswith(key):
            return key, BASE[key]
    # Fallback for unknown future ids: try to infer family.
    if model_id.startswith("gpt-"):
        return None, (2.50, 15.00, "Strong", 1_000_000, 128_000)
    if model_id.startswith("claude-"):
        return None, (5.00, 25.00, "Strong", 1_000_000, 128_000)
    if model_id.startswith("gemini-"):
        return None, (1.50, 9.00, "Medium", 1_000_000, 128_000)
    return None, (2.50, 15.00, "Strong", 128_000, 32_768)


def family(model_id: str) -> str:
    if model_id.startswith("claude-"):
        return "Claude"
    if model_id.startswith("gpt-"):
        return "Gpt"
    if model_id.startswith("gemini-"):
        return "Gemini"
    if model_id.startswith("glm-"):
        return "Glm"
    return "Generic"


def main():
    text = MODELS_TXT.read_text()
    lines = text.splitlines()
    entries = []
    pattern = re.compile(r"^([\w.-]+)\s+-\s+(.+)$")
    for line in lines:
        m = pattern.match(line)
        if not m:
            continue
        model_id = m.group(1)
        # Skip the tip/header line if it ever matches.
        if not model_id.replace("-", "").replace(".", "").replace("_", "").isalnum():
            continue
        base, (inp, out, tier, ctx, max_out) = classify(model_id)
        is_default = any(model_id == v for v in DEFAULTS.values())
        entries.append((model_id, inp, out, tier, ctx, max_out, is_default))

    out = ["pub(crate) const MODELS: &[crate::model::ModelEntry] = &[\n"]
    for model_id, inp, out_price, tier, ctx, max_out, is_default in entries:
        out.append("    crate::model::ModelEntry {\n")
        out.append(f'        prefixes: &["{model_id}"],\n')
        out.append(f"        tier: crate::model::ModelTier::{tier},\n")
        out.append(f"        family: crate::model::ModelFamily::{family(model_id)},\n")
        out.append("        vision: false,\n")
        out.append(f"        default: {'true' if is_default else 'false'},\n")
        out.append("        pricing: crate::model::ModelPricing {\n")
        out.append(f"            input: {inp},\n")
        out.append(f"            output: {out_price},\n")
        out.append("            cache_write: 0.0,\n")
        out.append("            cache_read: 0.0,\n")
        out.append("            fast: None,\n")
        out.append("        },\n")
        out.append(f"        max_output_tokens: {max_out:_},\n")
        out.append(f"        context_window: {ctx:_},\n")
        out.append("    },\n")
    out.append("];\n")

    OUT.write_text("".join(out))
    print(f"wrote {len(entries)} model entries to {OUT}")


if __name__ == "__main__":
    main()
