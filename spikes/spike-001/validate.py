# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrea Benetton
"""SPIKE-001: run ``claude plugin validate --strict`` on the spike's
plugin manifest twice -- as committed, and with ``author`` removed -- and
record both outputs as the run ``v1-validate``. No session is started.

    python3 validate.py
"""

import json
import pathlib
import shutil
import subprocess
import tempfile

from spike_common import CLAUDE, HERE, child_env, claude_version, distil, raw_dir


def main() -> int:
    raw = raw_dir("v1-validate")
    version = claude_version()
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
            proc = subprocess.run([CLAUDE, "plugin", "validate", "--strict", str(plug)],
                                  capture_output=True, text=True, env=child_env({}))
            results[case] = proc.returncode
            report += [f"--- {case}: exit {proc.returncode}", proc.stdout.strip(), proc.stderr.strip(), ""]
    (raw / "validate.txt").write_text("\n".join(report))
    (raw / "run.json").write_text(json.dumps({
        "claude_version": version,
        "command": [CLAUDE, "plugin", "validate", "--strict", "<copy of plugin/interweave-spike>"],
        "exit_by_case": results,
    }, indent=2) + "\n")
    distil(raw, "v1-validate")
    print(f"v1-validate: {results}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
