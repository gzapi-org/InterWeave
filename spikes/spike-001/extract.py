# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
"""SPIKE-001: distil a run's raw output into the evidence that is
committed, and nothing else.

    python3 extract.py <raw-run-dir> <runs/run-name>

The drivers (run.py, run_tty.py, validate.py) write a run's raw output
outside this repository and call this. The raw files -- Claude Code's
debug log, the session transcript, the terminal screen -- carry host
paths, account and telemetry detail this public repository must not
hold. Written into ``runs/<run-name>/``:

  stub.jsonl    the stub's own record, with tool-use ids replaced
                (s16_run.py has no stub: the far peer's bridge record,
                peer.jsonl, goes into evidence.md instead)
  evidence.md   from run.json: the command and knobs (paths replaced);
                the model the session used; from the debug log, only the
                lines about the stub's own MCP server; from the screen,
                the development-channel warning if shown; from the
                session transcript, the injected channel messages, the
                server-instructions attachment, the model's text, tool
                calls and tool results.

EVIDENCE ONLY.
"""

import json
import pathlib
import re
import sys

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parent.parent
# Only the run's own server, under either name it gets: the stub's, or
# the Stage 16 bridge's (s16_run.py).
SERVERS = ("spike001", "plugin:interweave-spike:spike001", "interweave", "plugin:interweave:interweave")
DEBUG_KEEP = re.compile(r'MCP server "(?:' + "|".join(map(re.escape, SERVERS)) + r')"|\[channel\] (?:' + "|".join(map(re.escape, SERVERS)) + r')\b')


def redact(text: str) -> str:
    text = text.replace(str(REPO) + "/", "")
    text = text.replace(str(pathlib.Path.home()), "$HOME")
    text = re.sub(r"(?:/var)?/tmp/[^\s\"'\\]*", "$SCRATCH", text)
    text = re.sub(r"\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b", "<uuid>", text)
    text = re.sub(r"\btoolu_[A-Za-z0-9]+", "<tool_use_id>", text)
    text = re.sub(r"\bmsg_[A-Za-z0-9]+", "<message_id>", text)
    return text


def fenced(text: str) -> list:
    return ["```text", redact(text), "```", ""]


def session_evidence(path: pathlib.Path) -> tuple:
    lines, models = [], set()
    for l in path.read_text().splitlines():
        d = json.loads(l)
        if d.get("type") == "attachment":
            att = d.get("attachment") or {}
            if att.get("type") == "mcp_instructions_delta":
                # Only the stub's own entry: the same attachment lists the
                # instructions of every other server the account loads.
                ours = [(n, b) for n, b in zip(att.get("addedNames", []), att.get("addedBlocks", []))
                        if n in SERVERS]
                for name, block in ours:
                    lines += [f"**attachment, type `{att['type']}`, server `{name}`:**", "", *fenced(block)]
            continue
        if d.get("type") not in ("user", "assistant"):
            continue
        msg = d.get("message") or {}
        if msg.get("model"):
            models.add(msg["model"])
        content = msg.get("content")
        items = content if isinstance(content, list) else [{"type": "text", "text": content}]
        for item in items:
            kind = item.get("type")
            if kind == "text" and item.get("text"):
                who = "injected/user" if d["type"] == "user" else "model"
                lines += [f"**{who}:**", "", *fenced(item["text"])]
            elif kind == "tool_use":
                lines += [f"**model tool call:** `{item['name']}`", "", *fenced(json.dumps(item.get("input")))]
            elif kind == "tool_result":
                lines += ["**tool result:**", "", *fenced(json.dumps(item.get("content")))]
    return lines, models


def stream_json_evidence(text: str) -> tuple:
    kept, models = [], set()
    for l in text.splitlines():
        try:
            d = json.loads(l)
        except ValueError:
            return None, models
        # The server list from init and the model's text; not the account's
        # rate limits, usage or the session's tool list.
        if d.get("type") == "system" and d.get("subtype") == "init":
            kept.append("init mcp_servers: " + json.dumps(d.get("mcp_servers")))
        elif d.get("type") == "assistant":
            if d["message"].get("model"):
                models.add(d["message"]["model"])
            for item in d["message"].get("content", []):
                if item.get("type") == "text":
                    kept.append(item["text"])
    return kept, models


def main() -> int:
    raw, out = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
    out.mkdir(parents=True, exist_ok=True)
    lines = [f"# {out.name}", ""]
    models = set()

    meta = json.loads((raw / "run.json").read_text())
    shown = {k: v for k, v in meta.items() if k != "session_transcript"}
    lines += ["## Run", "", "```json", redact(json.dumps(shown, indent=2)), "```", ""]

    if (raw / "debug.txt").exists():
        kept = [redact(l.rstrip()) for l in (raw / "debug.txt").read_text().splitlines() if DEBUG_KEEP.search(l)]
        lines += ["## Claude Code debug log: the stub's own server only", "", "```text", *kept, "```", ""]

    if (raw / "screen.txt").exists():
        flat = "".join((raw / "screen.txt").read_text().split())
        i = flat.find("WARNING:Loadingdevelopmentchannels")
        if i >= 0:
            j = flat.find("Esctocancel", i)
            lines += ["## Terminal: the development-channel warning (whitespace removed by capture)", "",
                      *fenced(flat[i:j + len("Esctocancel")])]

    if (raw / "session.jsonl").exists():
        body, m = session_evidence(raw / "session.jsonl")
        models |= m
        lines += ["## Session transcript: injected messages, attachments, replies, tool calls", "", *body]
    elif (raw / "transcript.txt").exists():
        text = (raw / "transcript.txt").read_text()
        kept, m = stream_json_evidence(text)
        models |= m
        lines += ["## Model output", "", *fenced(text if kept is None else "\n\n".join(kept))]

    if (raw / "peer.jsonl").exists():
        lines += ["## The far peer's bridge (daemon B): every line it wrote", "",
                  *fenced((raw / "peer.jsonl").read_text())]

    if (raw / "validate.txt").exists():
        lines += ["## `claude plugin validate --strict` output", "", *fenced((raw / "validate.txt").read_text())]

    if models:
        lines[2:2] = ["Model: " + ", ".join(sorted(models)), ""]
    (out / "evidence.md").write_text("\n".join(lines))
    if (raw / "stub.jsonl").exists():
        (out / "stub.jsonl").write_text(redact((raw / "stub.jsonl").read_text()))
    print(f"{out.name}: evidence written")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
