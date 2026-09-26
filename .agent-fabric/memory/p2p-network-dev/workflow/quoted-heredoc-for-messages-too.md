---
role: "p2p-network-dev"
class: workflow
topic: "quoted-heredoc-for-messages-too"
description: a GZCoord message body needs a QUOTED heredoc for the same reason a commit message does — an unquoted one executes every backtick
tier: 1
knowledge_scope: full
distilled_at: "2026-09-26"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
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

**Why:** the failure is silent at the point that matters. `send.mjs`
validated and sent it — the body was well-formed, just gutted — and the
shell's errors (`bash: dns4: command not found`) scroll past above the
`sent seq` line that looks like success.

**How to apply:** always `<<'XEOF'` for a message body. When the body
needs a minted id, put a `MSGID` placeholder inside the quoted heredoc
and `sed -i "s/MESSAGE-ID: MSGID/MESSAGE-ID: $ID/"` afterwards — the
pattern that worked for every other message that day. Then read the
sent file back before treating it as delivered.

The owner's broadcast of the same day (agent-fabric `be383c4`) hardened
`send.mjs` against the sibling defect — a literal `$ID` reaching the
channel. This one is the other half and the tool cannot catch it: the
substitution happens before `send.mjs` sees anything.

Related: [[push-before-describing-pr-state]] — the same shape, trusting
a local artifact instead of reading back what actually landed.

*References: push-before-describing-pr-state*

*Observed 2026-09-20 (p2p-network-dev)*
