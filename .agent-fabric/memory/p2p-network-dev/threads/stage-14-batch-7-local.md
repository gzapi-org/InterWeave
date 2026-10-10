---
role: "p2p-network-dev"
class: threads
topic: "stage-14-batch-7-local"
description: "Stage 14 batch 7 (two-daemon HumanChatV2 proof) merged as #169 ca74f6cf; one comment-only P3 carried to batch 8"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 82fba712a5b6cdd7
---

## Stage 14 batch 7 (two-daemon HumanChatV2 proof) merged as #169 ca74f6cf; one comment-only P3 carried to batch 8

#169 merged as ca74f6cf on 2026-10-02. It was 4 work + 1 fix, armed on the owner's word. It holds tests/desktop-e2e/tests/common/mod.rs (the harness) and human_chat.rs. human_chat.rs covers:
- facade + store + chat-protocol + ui-model over ipc-client on each side, with two human-desktop.yaml daemons;
- direct and broadcast, plain and ;ce=br, in both directions;
- a Recording tap whose captured payloads validate against human-chat/envelope.schema.json;
- the ui-model leg: labels, author, and a focused MarkRead that empties the store.

That is exit gate (b)'s instance test. The envelope flip itself is the close's act.

Carried to batch 8 (ui-slint), from #169 re-review N1 (P3, comment only): human_chat.rs's probe comment "A probe that arrived is held to every assertion" overclaims. A late second probe is not inserted. Narrow it to "a probe that arrived by the time one had crossed each way", or insert late arrivals after the final pump.

Also landed, in #168 (034fa34d): the transportctl backup-test flake fix ("record" is a BIP39 word).

Gotcha: a HumanStore directly under a tempdir root fails PermissionsTooOpen. Use a subdirectory.

From Stage 15, human_chat.rs is rust-ui-dev's. common/ and daemon.rs stay mine.

*Observed 2026-10-02 (p2p-network-dev)*
