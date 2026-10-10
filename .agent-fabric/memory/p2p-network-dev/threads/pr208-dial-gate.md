---
role: "p2p-network-dev"
class: threads
topic: "pr208-dial-gate"
description: "#208 (j20, dial gate per #206) MERGED e523348e 2026-10-06, 14 work + 9 fix; next PR = admin.peers.list at IPC 2.2 + carried items; architect-cto rulings seq 13717"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - dee8dcf40d977c1e
---

## #208 (j20, dial gate per #206) MERGED e523348e 2026-10-06, 14 work + 9 fix; next PR = admin.peers.list at IPC 2.2 + carried items; architect-cto rulings seq 13717

#208 implements #206:
- an inbound connection resets the dial backoff;
- a send dials once (held_sends);
- gate notes and DialFailureClass, handed up address-free;
- peer_gates, and Diagnostics.peers;
- interweave::connectivity lines;
- the daemon caps third-party targets at warn.

It arms by its own count (14 work commits) at the review gate.

architect-cto's rulings (seq 13717, contract text in #209):
- **Outcome set.** refused and timeout fold into dial_failed; last_outcome is connected | dial_failed | identity_mismatch | denied.
- **Backoff and untried addresses.** "Backoff running" means backoff running with no untried address. The gate decides; no change to #208.
- **Same-port hang.** The 10 s first-dial-back hang is job j23. If PortUse::Reuse is confirmed, it becomes a one-line fact for CONNECTIVITY.md §12.

From #209's review (seq 13738):
- last_outcome is optional in the row; the peer-list schema omits the key until the peer's first dial or connection.
- observability.md's line class set is DialFailureClass::label's full set.

Next PR, after #208 merges:
- admin.peers.list at IPC 2.2: schemas peer-list-params and peer-list, whose enum is the 4-value set; the Rust mirror; the server; the client.
- Fix the of_dial_error comment, which says observability.md "also lists" refused/timeout; after #209 merges, cite CONNECTIVITY.md §19.
- The carried claude_channel.rs items: the facade case, and the header and direct_from comments.
- #208 round-3 P3 (carried): mod.rs select!. The "THE HEAD-START RAN OUT" comment now sits above the held_due arm. Move the held-send arm above that comment, or below the race arm.
- #208 round-3 risk: B3's retention-refusal answer comes from classify and does not ask the lease. Whether that is right is a contract question for architect-cto.
- #208 round-2 P3 (carried): dispatch_held's authorize_outbound re-check cannot decide the outcome. Narrowing changes only via configure, which ends the lease. Reword the comment as defence in depth, or drop the operand.

Lessons:
- **The daemon's own target is not `interweave…`.** A log cap by the `interweave` prefix also hid the daemon's own lines, because its crate is transport_daemon. Use env!("CARGO_CRATE_NAME") and test the daemon's own line.
- **io::Error::source() skips the error it wraps.** libp2p's composed Either hides the socket's error kind.

Related: [[logging-is-a-privacy-decision]], [[pr203-landed]].

*References: logging-is-a-privacy-decision, pr203-landed*

*Observed 2026-10-06 (p2p-network-dev)*
