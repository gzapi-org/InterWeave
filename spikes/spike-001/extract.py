# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
"""SPIKE-001: distil a run's raw output into the evidence that is
committed, and nothing else.

    python3 extract.py runs/<run-name> [<raw-dir>]

The raw files a run writes -- Claude Code's debug log, the session
transcript, the terminal screen -- carry host paths, account and
telemetry detail this public repository must not hold. This keeps:

  stub.jsonl     the stub's own record (it knows no host detail)
  evidence.md    from run.json: the command (paths replaced), the knobs
                 and the exit; from the debug log: only the lines about
                 the stub's MCP server and channels; from the session
                 transcript: the channel messages as injected, the
                 model's text replies and the tool calls; from the
                 screen: the development-channel warning, if shown.

and moves the raw files to <raw-dir> (default: the scratch directory
named by SPIKE_RAW_DIR), out of the tree. EVIDENCE ONLY.
"""

import json
import os
import pathlib
import re
import shutil
import sys

RAW = ("debug.txt", "session.jsonl", "screen.txt", "transcript.txt", "stderr.txt", "run.json")
DEBUG_KEEP = re.compile(r'MCP server "(?:spike001|plugin:interweave-spike:spike001)"|\[channel\]|Channel notifications')


def redact(text: str) -> str:
    text = text.replace(str(pathlib.Path.home()), "$HOME")
    text = re.sub(r"/var/tmp/[^\s\"']*", "$SCRATCH", text)
    text = re.sub(r"/tmp/[^\s\"']*", "$SCRATCH", text)
    text = re.sub(r"\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b", "<uuid>", text)
    return text


def main() -> int:
    run = pathlib.Path(sys.argv[1])
    raw_root = pathlib.Path(sys.argv[2] if len(sys.argv) > 2 else os.environ["SPIKE_RAW_DIR"])
    raw = raw_root / run.name
    raw.mkdir(parents=True, exist_ok=True)
    lines = [f"# {run.name}", ""]

    meta = json.loads((run / "run.json").read_text())
    shown = {k: v for k, v in meta.items() if k not in ("session_transcript",)}
    lines += ["## Run", "", "```json", redact(json.dumps(shown, indent=2)), "```", ""]

    if (run / "debug.txt").exists():
        kept = [redact(l.rstrip()) for l in (run / "debug.txt").read_text().splitlines() if DEBUG_KEEP.search(l)]
        lines += ["## Claude Code debug log: the stub's server and channels only", "", "```text", *kept, "```", ""]

    if (run / "screen.txt").exists():
        flat = "".join((run / "screen.txt").read_text().split())
        i = flat.find("WARNING:Loadingdevelopmentchannels")
        if i >= 0:
            j = flat.find("Esctocancel", i)
            lines += ["## Terminal: the development-channel warning (whitespace removed by capture)", "",
                      "```text", flat[i:j + len("Esctocancel")], "```", ""]

    if (run / "session.jsonl").exists():
        lines += ["## Session transcript: injected channel messages, replies and tool calls", ""]
        for l in (run / "session.jsonl").read_text().splitlines():
            d = json.loads(l)
            if d.get("type") not in ("user", "assistant"):
                continue
            content = (d.get("message") or {}).get("content")
            items = content if isinstance(content, list) else [{"type": "text", "text": content}]
            for item in items:
                kind = item.get("type")
                if kind == "text" and item.get("text"):
                    who = "injected/user" if d["type"] == "user" else "model"
                    lines += [f"**{who}:**", "", "```text", redact(item["text"]), "```", ""]
                elif kind == "tool_use":
                    lines += [f"**model tool call:** `{item['name']}` {json.dumps(item.get('input'))}", ""]
                elif kind == "tool_result":
                    body = item.get("content")
                    lines += ["**tool result:**", "", "```text", redact(json.dumps(body)), "```", ""]

    if (run / "transcript.txt").exists() and not (run / "session.jsonl").exists():
        out = (run / "transcript.txt").read_text()
        kept = []
        for l in out.splitlines():
            try:
                d = json.loads(l)
            except ValueError:
                kept = None
                break
            # stream-json: the server list from init and the model's text;
            # not the account's rate limits, usage or session tool list.
            if d.get("type") == "system" and d.get("subtype") == "init":
                kept.append("init mcp_servers: " + json.dumps(d.get("mcp_servers")))
            elif d.get("type") == "assistant":
                for item in d["message"].get("content", []):
                    if item.get("type") == "text":
                        kept.append(item["text"])
        body = out if kept is None else "\n\n".join(kept)
        lines += ["## Model output", "", "```text", redact(body), "```", ""]

    (run / "evidence.md").write_text("\n".join(lines))
    for name in RAW:
        if (run / name).exists():
            shutil.move(str(run / name), str(raw / name))
    print(f"{run.name}: evidence.md written, raw moved to {raw}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
