# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
"""Stage 16 step 5 (plan §19): the bridge as a plugin in the installed
Claude Code, against two real daemons. SPIKE-001's drivers reused.

    python3 s16_run.py <run-name> [--wait S]

Two daemons on this host's private address, each the shipped
``human-desktop.yaml`` example with a static route to the other:

- A, whose ``claude`` endpoint the plugin's bridge leases inside an
  interactive Claude Code session started with the development-channel
  flag (SPIKE-001 facts 4-7), driven through a pseudo-terminal as
  ``run_tty.py`` drives one.
- B, whose ``claude`` endpoint a second ``claude-channel`` holds, driven
  by this script over its stdio: the far peer. It sends A's ``claude``
  a direct message, and records every line it receives, so the reply
  the session makes is seen arriving.

Raw output goes outside the tree (``spike_common.raw_dir``); the run is
distilled by ``extract.py`` into ``runs/<run-name>/``, the peer's record
included. EVIDENCE ONLY: nothing here is a test.
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
import socket
import subprocess
import tempfile
import time

from spike_common import CLAUDE, REPO, child_env, claude_version, distil, raw_dir

HERE = pathlib.Path(__file__).resolve().parent
ANSI = re.compile(rb"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07]*\x07|\x1b[()][0-9A-Za-z]")
STRANGER = "12D3KooWDpJ7As7BWAwRMfu1VU2WCqNjvq387JEYKDBj4kx6nXTN"
PROFILE = "human-desktop"
TOOL = "mcp__plugin_interweave_interweave__"
KEYS = "expect:trust this folder,wait:1,down,enter,expect:local development,wait:1,enter,wait:10"


def long_term_token():
    """This login's long-term Claude Code token (a setup-token), from the
    agent-fabric secrets file its shell profile sources -- the owner's
    ruling of 2026-10-06: the nested session authenticates with the
    login's own long-term token, never an interactive login. Only this one
    variable is passed, and only its NAME is recorded."""
    path = pathlib.Path.home() / ".config" / "agent-fabric" / "secrets.env"
    if not path.exists():
        raise SystemExit("no agent-fabric secrets file: run fabric-secrets sync")
    for line in path.read_text().splitlines():
        m = re.match(r"\s*(?:export\s+)?CLAUDE_CODE_OAUTH_TOKEN=(['\"]?)(.+)\1\s*$", line)
        if m:
            return {"CLAUDE_CODE_OAUTH_TOKEN": m.group(2)}
    raise SystemExit("the secrets file holds no CLAUDE_CODE_OAUTH_TOKEN")


def private_ipv4():
    out = subprocess.run(["ip", "-o", "-4", "addr", "show"], capture_output=True, text=True).stdout
    for addr in re.findall(r"inet (\d+\.\d+\.\d+\.\d+)/", out):
        a, b = (int(x) for x in addr.split(".")[:2])
        if a == 10 or (a == 172 and 16 <= b <= 31) or (a == 192 and b == 168):
            return addr
    raise SystemExit("no private IPv4 interface: the run needs one, loopback is refused (ADR-0052)")


def free_port(ip):
    with socket.socket() as s:
        s.bind((ip, 0))
        return s.getsockname()[1]


class Home:
    """One daemon's XDG tree, as the Rust harness builds one."""

    def __init__(self, base):
        self.base = pathlib.Path(base)
        for d in ("config", "data", "state", "cache", "run"):
            (self.base / d).mkdir(mode=0o700, parents=True, exist_ok=True)

    def env(self):
        return {f"XDG_{k}": str(self.base / v) for k, v in
                (("CONFIG_HOME", "config"), ("DATA_HOME", "data"), ("STATE_HOME", "state"),
                 ("CACHE_HOME", "cache"), ("RUNTIME_DIR", "run"))}

    def write_config(self, other, listen, route):
        raw = (REPO / "architecture/config/examples/human-desktop.yaml").read_text()
        raw = raw.replace("<PEER_A>", other).replace("/ip4/0.0.0.0/tcp/4001", listen)
        raw = re.sub(r"<[^>]*>", STRANGER, raw)
        if route:
            raw = raw.replace("discovery:\n  providers:\n",
                              "discovery:\n  providers:\n    - { type: static-bootstrap, enabled: true, "
                              f"priority: 5, config: {{ peers: [\"{route}\"] }} }}\n", 1)
        raw += "observability: { log_level: debug }\n"
        path = self.base / "config" / "interweave" / "profiles" / PROFILE / "config.yaml"
        path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        path.write_text(raw)

    def start(self, binary, log, *extra):
        return subprocess.Popen([str(binary), "--profile", PROFILE, *extra],
                                env={"PATH": os.environ["PATH"], **self.env()},
                                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                stderr=open(log, "w"))

    def ctl(self, ctl, *args):
        return subprocess.run([str(ctl), "--profile", PROFILE, *args], capture_output=True, text=True,
                              env={"PATH": os.environ["PATH"], **self.env()})

    def serving(self, ctl, seconds=30):
        deadline = time.time() + seconds
        while time.time() < deadline:
            out = self.ctl(ctl, "status", "--json")
            if out.returncode == 0:
                return json.loads(out.stdout)
            time.sleep(0.2)
        raise SystemExit(f"{self.base}: the daemon never served")

    def stop(self, ctl, proc):
        self.ctl(ctl, "shutdown")
        try:
            proc.wait(10)
        except subprocess.TimeoutExpired:
            proc.kill()


class Peer:
    """The far peer: a claude-channel on B, spoken to over its stdio."""

    def __init__(self, binary, home, record):
        self.proc = subprocess.Popen([str(binary), "--profile", PROFILE, "--endpoint", "claude"],
                                     env={"PATH": os.environ["PATH"], **home.env()},
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.DEVNULL)
        self.record = open(record, "w")
        self.lines = []
        self.next_id = 0
        self.pending = b""

    def _read(self, until):
        """The next line, read from the raw fd and split here: select
        watches the fd, so a line left in a buffered reader would never
        wake it."""
        fd = self.proc.stdout.fileno()
        while b"\n" not in self.pending:
            left = until - time.time()
            if left <= 0:
                return None
            r, _, _ = select.select([fd], [], [], min(0.2, left))
            if r:
                chunk = os.read(fd, 65536)
                if not chunk:
                    return None
                self.pending += chunk
        raw, _, self.pending = self.pending.partition(b"\n")
        line = raw.decode() + "\n"
        self.record.write(line)
        self.record.flush()
        value = json.loads(line)
        self.lines.append(value)
        return value

    def tool(self, name, arguments, seconds=30):
        self.next_id += 1
        ident = self.next_id
        self.proc.stdin.write((json.dumps({"jsonrpc": "2.0", "id": ident, "method": "tools/call",
                                           "params": {"name": name, "arguments": arguments}}) + "\n").encode())
        self.proc.stdin.flush()
        until = time.time() + seconds
        while time.time() < until:
            value = self._read(until)
            if value is not None and value.get("id") == ident:
                return value["result"]["content"][0]["text"], value["result"].get("isError", False)
        raise SystemExit(f"the peer bridge did not answer {name}")

    def drain(self, seconds):
        until = time.time() + seconds
        while self._read(until) is not None:
            pass

    def received(self, text):
        return any(v.get("method") == "notifications/claude/channel" and v["params"]["content"] == text
                   for v in self.lines)

    def stop(self):
        if self.proc.poll() is None:
            self.proc.kill()
        self.proc.wait()
        self.record.close()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("name")
    ap.add_argument("--wait", type=float, default=90.0, help="seconds after the prompt")
    args = ap.parse_args()

    subprocess.run(["cargo", "build", "-q", "-p", "interweave-transport-daemon", "-p", "interweave-transportctl",
                    "-p", "interweave-claude-channel"], check=True, cwd=REPO)
    target = REPO / "target" / "debug"
    daemon, ctl, bridge = target / "transport-daemon", target / "transportctl", target / "claude-channel"

    out = raw_dir(args.name)
    nonce = secrets.token_hex(4)
    scratch = pathlib.Path(tempfile.mkdtemp(prefix="s16-"))
    ip = private_ipv4()
    a, b = Home(scratch / "a"), Home(scratch / "b")
    a_port, b_port = free_port(ip), free_port(ip)

    # Every process started from here is stopped, and the scratch tree --
    # both profiles' private keys -- removed, however the run ends.
    live = []
    try:
        # The keys first: each daemon creates its own, and says its PeerId.
        peers = {}
        for name, home, port in (("a", a, a_port), ("b", b, b_port)):
            home.write_config(STRANGER, f"/ip4/{ip}/tcp/{port}", None)
            proc = home.start(daemon, out / f"daemon-{name}-keygen.log", "--create-identity")
            live.append(proc)
            peers[name] = home.serving(ctl)["peer"]
            home.stop(ctl, proc)
        route = lambda port, peer: f"/ip4/{ip}/tcp/{port}/p2p/{peer}"
        a.write_config(peers["b"], f"/ip4/{ip}/tcp/{a_port}", route(b_port, peers["b"]))
        b.write_config(peers["a"], f"/ip4/{ip}/tcp/{b_port}", route(a_port, peers["a"]))
        a_proc = a.start(daemon, out / "daemon-a.log")
        live.append(a_proc)
        b_proc = b.start(daemon, out / "daemon-b.log")
        live.append(b_proc)
        a.serving(ctl)
        b.serving(ctl)
        peer = Peer(bridge, b, out / "peer.jsonl")
        live.append(peer)

        # The plugin as packaged, its bridge pointed at A.
        plug = scratch / "plugin" / "interweave"
        shutil.copytree(REPO / "packaging" / "claude-plugin" / "interweave", plug)
        (plug / "bin").mkdir()
        shutil.copy(bridge, plug / "bin" / "claude-channel")
        manifest = json.loads((plug / ".mcp.json").read_text())
        server = manifest["mcpServers"]["interweave"]
        server["args"] = ["--profile", PROFILE, "--endpoint", "claude"]
        server["env"] = a.env()
        (plug / ".mcp.json").write_text(json.dumps(manifest, indent=2))

        message = f"S16 {nonce}: hello from the peer on daemon B"
        prompt = (HERE / "prompt-s16.txt").read_text().strip().replace("<TOOL>", TOOL + "reply")
        version = claude_version()
        cwd = scratch / "cwd"
        cwd.mkdir()
        cmd = [CLAUDE, "--plugin-dir", str(plug), "--setting-sources", "local",
               "--allowedTools", TOOL + "reply", TOOL + "status",
               "--debug-file", str(out / "debug.txt"),
               "--dangerously-load-development-channels", "plugin:interweave@inline"]

        screen = bytearray()
        pid, fd = pty.fork()
        if pid:
            live.append(pid)
        if pid == 0:
            os.chdir(cwd)
            os.execvpe(cmd[0], cmd, child_env({"TERM": "xterm-256color", "COLUMNS": "120", "LINES": "40",
                                               **long_term_token()}))

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
        for step in KEYS.split(","):
            kind, _, arg = step.partition(":")
            if kind == "wait":
                pump(float(arg))
            elif kind == "expect":
                want = "".join(arg.split())
                deadline = time.time() + 60
                while want not in "".join(text().split()) and time.time() < deadline:
                    if not pump(0.5):
                        break
                steps.append({"expect": arg, "found": want in "".join(text().split()),
                              "at_s": round(time.time() - started, 1)})
            else:
                os.write(fd, {"down": b"\x1b[B", "enter": b"\r"}[kind])
                pump(0.3)

        # The far peer sends; the first exchange waits on the pair's
        # connection (the retry base), so it is tried until accepted.
        sent, deadline = None, time.time() + 120
        while time.time() < deadline:
            answer, error = peer.tool("send", {"peer": peers["a"], "endpoint": "claude", "content": message})
            if not error:
                sent = {"answer": answer, "at_s": round(time.time() - started, 1)}
                break
            pump(1)
        steps.append({"peer_send": sent})
        pump(8)
        os.write(fd, prompt.encode())
        pump(0.5)
        os.write(fd, b"\r")
        replied, deadline = False, time.time() + args.wait
        while time.time() < deadline and not replied:
            pump(2)
            peer.drain(0.5)
            replied = peer.received("ack")
        steps.append({"peer_received_reply": replied, "at_s": round(time.time() - started, 1)})
        # The reply arriving is mid-turn for the model: its read-back and the
        # tool's result follow. Let the turn finish before leaving.
        pump(30)

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
        live.remove(pid)
        (out / "screen.txt").write_text(text())
        peer.drain(1)
        peer.stop()
        live.remove(peer)
        a.stop(ctl, a_proc)
        b.stop(ctl, b_proc)

        slug = "-" + str(cwd).strip("/").replace("/", "-").replace("_", "-").replace(".", "-")
        found = sorted((pathlib.Path.home() / ".claude" / "projects").glob(slug + "*/*.jsonl"))
        if found:
            shutil.copy(found[-1], out / "session.jsonl")
        (out / "run.json").write_text(json.dumps({
            "claude_binary_version": version, "command": cmd, "keys": KEYS, "steps": steps,
            "nonce": nonce, "message": message, "prompt": prompt, "wait_status": status,
            "env_passed": sorted(child_env({"TERM": "", "COLUMNS": "", "LINES": "",
                                            "CLAUDE_CODE_OAUTH_TOKEN": ""})),
            "seconds": round(time.time() - started, 1),
            "session_transcript": str(found[-1]) if found else None,
        }, indent=2) + "\n")
    finally:
        for thing in reversed(live):
            if isinstance(thing, int):
                try:
                    os.kill(thing, 9)
                    os.waitpid(thing, 0)
                except (ProcessLookupError, ChildProcessError):
                    pass
            elif isinstance(thing, Peer):
                thing.stop()
            elif thing.poll() is None:
                thing.kill()
                thing.wait()
        shutil.rmtree(scratch, ignore_errors=True)
    distil(out, args.name)
    print(f"{args.name}: nonce={nonce} sent={bool(sent)} replied={replied} "
          f"transcript={'yes' if found else 'no'} binary={version} raw={out}")
    return 0 if sent and replied else 1


if __name__ == "__main__":
    raise SystemExit(main())
