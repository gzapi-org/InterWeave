---
role: "p2p-network-dev"
class: workflow
topic: "quoted-heredoc-for-messages-too"
description: a GZCoord message body needs a QUOTED heredoc for the same reason a commit message does — an unquoted one executes every backtick
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 36c8a3f4f27ee0cc
  - 6ece2eea5f6570bb
---

## a GZCoord message body needs a QUOTED heredoc for the same reason a commit message does — an unquoted one executes every backtick

CLAUDE.md §9 requires a quoted heredoc (`<<'EOF'`) for commit messages
carrying shell metacharacters. **A GZCoord message body is the same
artifact class and I did not treat it as one** (2026-09-20, seq 3702):
`cat > msg.txt <<XEOF` with backtick-quoted identifiers throughout
executed each one as a command and substituted the empty string, so the
message reached the channel reading "the X feature", "wraps the base
transport in ", "A dial no longer answers ". Exactly the facts it
existed to carry.

**Why:** the failure is silent at the point that matters. `gzcoord-send`
validated and sent it — the body was well-formed, just gutted — and the
shell's errors (`bash: dns4: command not found`) scroll past above the
`sent seq` line that looks like success.

**How to apply:** always `<<'XEOF'` for a message body, and MINT THE ID
FIRST (fabric-coordinator, 2026-09-26; the gzcoord-send skill's step 2):
`ID=$(gzmsg new-id)`, then write the id line unquoted before the quoted
body (e.g. `{ printf 'MESSAGE-ID: %s\n' "$ID"; cat <<'XEOF' … XEOF; }`)
or write the file with the id already in it. A `MSGID` placeholder
sed-replaced afterwards never went out, but the command text the owner
reads showed the placeholder and looked like a message sent without an
id. Then read the sent file back before treating it as delivered.

The owner's broadcast of the same day (agent-fabric `be383c4`) hardened
`gzcoord-send` (its script, `send.mjs`) against the sibling defect — a literal `$ID` reaching the
channel. This one is the other half and the tool cannot catch it: the
substitution happens before `gzcoord-send` sees anything.

Related: [[push-before-describing-pr-state]] — the same shape, trusting
a local artifact instead of reading back what actually landed.

AGAIN 2026-09-30 (#154's PR body): an UNQUOTED `python3 - <<EOF` heredoc, used so `$S` would expand,
also command-substituted every backticked span in the embedded markdown -- it ran a full `cargo xtask ci`
and several "command not found"s, and left the body unedited. A script that carries prose (markdown,
messages, commit text) goes in a FILE written with the Write tool and is run by path; never an unquoted
heredoc, and never interpolate a shell variable into prose. Paths go in the script as literals.

THIRD TIME 2026-10-05 (#192's PR body): `python3 - <<PYEOF` again, unquoted so `$S` would expand, with
`` `cargo xtask test` `` in the markdown. It started a second full test run beside the running CI, under
its own Xvfb and session bus, and that bus started xdg-desktop-portal, xdg-document-portal,
xdg-permission-store and gnome-keyring through systemd --user. Those outlived the bus and had to be
killed by PID. The body then went out gutted. Recognising the rule did not stop it: the trigger is
writing `<<` followed by anything but a quote, in ANY command whose body holds prose. Before sending a
Bash call containing `<<`, check that the delimiter is quoted.

*References: push-before-describing-pr-state*

*Observed 2026-10-05 (p2p-network-dev)*
