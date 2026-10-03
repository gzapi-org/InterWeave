# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
"""SPIKE-001: what every driver shares, so isolation and placement are
decided once.

- ``CLAUDE``: the Claude Code binary every run uses -- ``$SPIKE_CLAUDE``,
  an explicit build, or ``claude``. Several builds live side by side and
  the default one updates itself, so a campaign names one.
- ``child_env``: a launched process gets an ALLOW-LISTED environment
  with the auto-updater off -- never the launching session's tokens,
  fabric markers or model overrides.
- ``raw_dir``: raw output goes OUTSIDE the repository, always.
- ``session_version``: the version a run's session announced to the stub
  (``initialize`` ``clientInfo``), which is the version the run measured.
"""

import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parent.parent
CLAUDE = os.environ.get("SPIKE_CLAUDE") or "claude"
ENV_ALLOW = ("HOME", "PATH", "LANG", "LC_ALL", "LC_CTYPE", "USER", "LOGNAME",
             "SHELL", "TMPDIR", "XDG_RUNTIME_DIR")


def child_env(extra):
    env = {k: v for k, v in os.environ.items() if k in ENV_ALLOW}
    # A build that updates itself mid-campaign changes what is measured.
    env["DISABLE_AUTOUPDATER"] = "1"
    env.update(extra)
    return env


def claude_version():
    out = subprocess.run([CLAUDE, "--version"], capture_output=True, text=True, env=child_env({}))
    return out.stdout.strip().split(" ")[0]


def raw_dir(name):
    """Where a run's RAW output goes. The raw debug log, transcript and
    screen carry host paths and account detail; only extract.py's
    distillation is committed."""
    root = pathlib.Path(os.environ.get("SPIKE_RAW_DIR") or (pathlib.Path(tempfile.gettempdir()) / "spike-001-raw"))
    resolved = root.resolve()
    if resolved == REPO or REPO in resolved.parents:
        raise SystemExit("SPIKE_RAW_DIR must be outside the repository")
    d = resolved / name
    if d.exists():
        shutil.rmtree(d)
    d.mkdir(parents=True)
    return d


def session_version(stub_log):
    """The Claude Code version the session announced in `initialize`."""
    for line in pathlib.Path(stub_log).read_text().splitlines():
        body = json.loads(line)["body"]
        if isinstance(body, dict) and body.get("method") == "initialize":
            return body["params"]["clientInfo"]["version"]
    return None


def distil(raw, name):
    subprocess.run([sys.executable, str(HERE / "extract.py"), str(raw), str(HERE / "runs" / name)], check=True)
