# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
"""SPIKE-001: an INTERACTIVE Claude Code session against the stub, driven
through a pseudo-terminal, for what a non-interactive run cannot show:
the development-channel confirmation screen and, behind it, channel
delivery.

    python3 run_tty.py <run-name> [--delay-ms N] [--keys <spec>] [--wait S]

``--keys`` is a comma-separated script of steps, each one of:
  wait:<seconds>      sleep
  expect:<text>       wait (up to 60 s) until the screen text contains it
  send:<text>         type the text
  prompt              type the --prompt-file's text
  enter / down / up / esc / ctrl-c
Writes raw output outside the tree like run.py, plus ``screen.txt``
(everything the terminal showed, ANSI stripped) and ``session.jsonl``
(the session's own transcript from ``~/.claude/projects``, which records
what was injected into the conversation), then distils it into
``runs/<run-name>/``. EVIDENCE ONLY.
"""

import argparse
import json
import os
import pathlib
import pty
import re
import secrets
import select
import shutil
import subprocess
import tempfile
import time

HERE = pathlib.Path(__file__).resolve().parent


from spike_common import CLAUDE, child_env, claude_version, distil, raw_dir, session_version

ANSI = re.compile(rb"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07]*\x07|\x1b[()][0-9A-Za-z]|\x1b[=>]")
KEYS = {"enter": b"\r", "down": b"\x1b[B", "up": b"\x1b[A", "esc": b"\x1b", "ctrl-c": b"\x03"}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("name")
    ap.add_argument("--delay-ms", type=int, default=0)
    ap.add_argument("--exit-after-ms", type=int)
    ap.add_argument("--protocol", default="echo")
    ap.add_argument("--keys", default="wait:12")
    ap.add_argument("--no-dev-flag", action="store_true")
    ap.add_argument("--prompt-file", help="text the `prompt` step types")
    ap.add_argument("--plugin", action="store_true", help="load plugin/interweave-spike with --plugin-dir instead of a bare --mcp-config server")
    ap.add_argument("--channel-spec", default="server:spike001", help="the value given to --dangerously-load-development-channels")
    args = ap.parse_args()

    target = pathlib.Path(os.environ.get("CARGO_TARGET_DIR", HERE / "stub" / "target"))
    subprocess.run(["cargo", "build", "--release", "--locked", "-j", "2"], cwd=HERE / "stub",
                   check=True, env={**os.environ, "CARGO_TARGET_DIR": str(target)})
    binary = target / "release" / "spike-001-stub"
    out = raw_dir(args.name)
    nonce = secrets.token_hex(4)

    scratch = tempfile.mkdtemp(prefix="spike001tty-")
    mcp = {"mcpServers": {"spike001": {"command": str(binary), "args": [], "env": {
        "SPIKE_LOG": str(out / "stub.jsonl"), "SPIKE_NONCE": nonce,
        "SPIKE_PROTOCOL": args.protocol, "SPIKE_DELAY_MS": str(args.delay_ms),
        **({"SPIKE_EXIT_AFTER_MS": str(args.exit_after_ms)} if args.exit_after_ms else {})}}}}
    (pathlib.Path(scratch) / "mcp.json").write_text(json.dumps(mcp, indent=2))
    if args.plugin:
        plug = pathlib.Path(scratch) / "plugin" / "interweave-spike"
        shutil.copytree(HERE / "plugin" / "interweave-spike", plug)
        (plug / "bin").mkdir()
        shutil.copy(binary, plug / "bin" / "spike-001-stub")
        manifest = json.loads((plug / ".mcp.json").read_text())
        manifest["mcpServers"]["spike001"]["env"].update(mcp["mcpServers"]["spike001"]["env"])
        (plug / ".mcp.json").write_text(json.dumps(manifest, indent=2))
        source = ["--plugin-dir", str(plug)]
    else:
        source = ["--mcp-config", "mcp.json", "--strict-mcp-config"]
    version = claude_version()
    cmd = [CLAUDE] + source + [
           "--setting-sources", "local",
           "--allowedTools", "mcp__spike001__reply", "mcp__spike001__status",
           "--debug-file", str(out / "debug.txt")]
    if not args.no_dev_flag:
        cmd += ["--dangerously-load-development-channels", args.channel_spec]

    screen = bytearray()
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(scratch)
        # A fresh top-level session: inherited CLAUDE* markers would make it
        # a child that saves no transcript, and the transcript is the
        # evidence. Its login comes from the credentials file, not env.
        env = child_env({"TERM": "xterm-256color", "COLUMNS": "120", "LINES": "40"})
        os.execvpe(cmd[0], cmd, env)

    def pump(seconds):
        end = time.time() + seconds
        while time.time() < end:
            r, _, _ = select.select([fd], [], [], 0.1)
            if r:
                try:
                    chunk = os.read(fd, 65536)
                except OSError:
                    return False
                if not chunk:
                    return False
                screen.extend(chunk)
        return True

    def text():
        return ANSI.sub(b"", bytes(screen)).decode("utf-8", "replace")

    started = time.time()
    steps = []
    for step in [s for s in args.keys.split(",") if s]:
        kind, _, arg = step.partition(":")
        if kind == "wait":
            pump(float(arg))
        elif kind == "expect":
            # The TUI positions text with cursor moves rather than spaces, so
            # the captured text is compared with all whitespace removed.
            want = "".join(arg.split())
            seen = lambda: want in "".join(text().split())
            deadline = time.time() + 60
            while not seen() and time.time() < deadline:
                if not pump(0.5):
                    break
            steps.append({"expect": arg, "found": seen(), "at_s": round(time.time() - started, 1)})
        elif kind == "prompt":
            os.write(fd, pathlib.Path(args.prompt_file).read_text().strip().encode())
            pump(0.5)
        elif kind == "send":
            os.write(fd, arg.encode())
            pump(0.3)
        else:
            os.write(fd, KEYS[kind])
            pump(0.3)
    # Leave: /exit, then make sure.
    try:
        os.write(fd, b"/exit\r")
    except OSError:
        pass
    pump(5)
    try:
        os.kill(pid, 15)
    except ProcessLookupError:
        pass
    _, status = os.waitpid(pid, 0)
    (out / "screen.txt").write_text(text())

    slug = "-" + scratch.strip("/").replace("/", "-").replace("_", "-").replace(".", "-")
    projects = pathlib.Path.home() / ".claude" / "projects"
    found = sorted(projects.glob(slug + "*/*.jsonl"))
    if found:
        shutil.copy(found[-1], out / "session.jsonl")
    (out / "run.json").write_text(json.dumps({
        "claude_binary_version": version,
        "claude_version": session_version(out / "stub.jsonl"),
        "command": cmd, "keys": args.keys, "steps": steps, "nonce": nonce,
        "prompt": pathlib.Path(args.prompt_file).read_text().strip() if args.prompt_file else None,
        "delay_ms": args.delay_ms, "exit_after_ms": args.exit_after_ms, "stub_protocol": args.protocol,
        "dev_flag": not args.no_dev_flag, "wait_status": status,
        "env_passed": sorted(child_env({"TERM": "", "COLUMNS": "", "LINES": ""})),
        "seconds": round(time.time() - started, 1),
        "session_transcript": str(found[-1]) if found else None,
    }, indent=2) + "\n")
    shutil.rmtree(scratch, ignore_errors=True)
    distil(out, args.name)
    seen = session_version(out / "stub.jsonl")
    print(f"{args.name}: nonce={nonce} transcript={'yes' if found else 'no'} binary={version} session={seen} raw={out}")
    if seen != version:
        raise SystemExit(f"{args.name}: the session ran {seen}, not the binary's {version}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
