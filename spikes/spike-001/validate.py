# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
"""SPIKE-001: run ``claude plugin validate --strict`` on the spike's
plugin manifest twice -- as committed, and with ``author`` removed -- and
record both outputs as the run ``v1-validate``. No session is started.

    python3 validate.py
"""

import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parent


def main() -> int:
    raw_root = pathlib.Path(os.environ.get("SPIKE_RAW_DIR") or (pathlib.Path(tempfile.gettempdir()) / "spike-001-raw"))
    raw = raw_root / "v1-validate"
    if raw.exists():
        shutil.rmtree(raw)
    raw.mkdir(parents=True)
    version = subprocess.run(["claude", "--version"], capture_output=True, text=True).stdout.strip()
    report = []
    results = {}
    with tempfile.TemporaryDirectory(prefix="spike001-validate-") as scratch:
        for case in ("as-committed", "without-author"):
            plug = pathlib.Path(scratch) / case / "interweave-spike"
            shutil.copytree(HERE / "plugin" / "interweave-spike", plug)
            manifest = plug / ".claude-plugin" / "plugin.json"
            if case == "without-author":
                d = json.loads(manifest.read_text())
                d.pop("author", None)
                manifest.write_text(json.dumps(d, indent=2) + "\n")
            proc = subprocess.run(["claude", "plugin", "validate", "--strict", str(plug)],
                                  capture_output=True, text=True)
            results[case] = proc.returncode
            report += [f"--- {case}: exit {proc.returncode}", proc.stdout.strip(), proc.stderr.strip(), ""]
    (raw / "validate.txt").write_text("\n".join(report))
    (raw / "run.json").write_text(json.dumps({
        "claude_version": version,
        "command": ["claude", "plugin", "validate", "--strict", "<copy of plugin/interweave-spike>"],
        "exit_by_case": results,
    }, indent=2) + "\n")
    subprocess.run([sys.executable, str(HERE / "extract.py"), str(raw), str(HERE / "runs" / "v1-validate")], check=True)
    print(f"v1-validate: {results}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
