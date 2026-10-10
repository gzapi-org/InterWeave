---
role: "p2p-network-dev"
class: threads
topic: "pr203-landed"
description: "#203 (Stage 16 steps 4+5) merged f7b2c46b 2026-10-06; carried P3 and risks for the next change in claude_channel.rs or s16_run.py"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 37076d29b4deb3c4
---

## #203 (Stage 16 steps 4+5) merged f7b2c46b 2026-10-06; carried P3 and risks for the next change in claude_channel.rs or s16_run.py

#203 merged as f7b2c46b at 2026-10-06T15:28Z. It holds the desktop-e2e `claude_channel.rs` cases (two daemons) and SPIKE-001's `s16_run.py`, with three host runs (`s16-host-run-3` is the end-to-end evidence). Count: 6 work commits and 5 review fixes. It was armed on the owner's word. With it, every implementation step of plan §19 is done; the close (step 6) is architect-cto's.

Carried:
- **P3:** the doc comment on `direct_from` (claude_channel.rs, about line 412) still says "from B's `human`"; the helper takes any session.
- **P3 (architect-cto, seq 13406):** claude_channel.rs:18-20 says the facade "adds the envelope, and the bridge forwards an envelope as text, unparsed". In fact the facade sets the HumanChatV2 media type and, over max_payload_bytes, compresses to `;ce=br`; the bridge DECODES that first (channel-core content.rs). The decode is unit-tested only, never across daemons. Amend the header on the next touch. If the owner holds the Stage 16 close for the facade proof, the test is one case: B sends a hand-built HumanChatV2 envelope, plain and `;ce=br`, and the notification carries the decoded text. The close records it as a deviation (#205, db3277b6).
- **Risk:** a pty child that dies at once may lose its traceback to EIO on the master. This is unproven; one deliberately failed run would settle it.
- **Risks carried from #199:**
  - the ipc-client writer-gone window;
  - the conformance suite does not pin `events(0)` on an ended session;
  - the bridge inherits the nested session's environment, its token included (not logged);
  - the recorded version comes from `claude --version`.
- **Lesson:** a desktop-e2e mutation must rebuild `interweave-claude-channel` first; the test runs the prebuilt binary. Related: [[stage-16-channel-core]].

**Status 2026-10-08:** both P3s and the cross-daemon HumanChatV2 plain + `;ce=br` test are DONE on main (53000a4f); the header and `direct_from` doc are already current. Still open from this list: the pty EIO risk and #199's risks.

*References: stage-16-channel-core*

*Observed 2026-10-08 (p2p-network-dev)*
