---
role: "p2p-network-dev"
class: workflow
topic: "batch-5-agree-client-contract-first"
description: "Before building Stage 14 batch 5 (transport-client facade), agree its client-facing contract with rust-ui-dev-01; charter #79 (2026-10-02)"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 799a1e4e7204564e
---

## Before building Stage 14 batch 5 (transport-client facade), agree its client-facing contract with rust-ui-dev-01; charter #79 (2026-10-02)

The role's charter and brief changed on 2026-10-02 (agent-fabric #79, 108fdd7). The change binds batch 5 (crates/human/transport-client, the facade the human UI consumes):

- The UI client's agent is rust-ui-dev-01, which holds the interweave project. Agree with it the SMALLEST client-facing contract before code: reachable / connected / ready; transient vs terminal; what uncertainty may be shown (unknown shown as unknown, never false). Never hand it libp2p internal states as product meaning.
- The product guarantee is architect-cto's. If the mechanism cannot give it as specified, report what it can give and the alternatives. Never reinterpret the contract to fit the implementation.
- A delivery names: the commit/PR; the network behaviour; the invariants and approved contracts relied on; the evidence at each of the eight levels kept apart (pure state machine → real-peer conformance → real sockets → degraded network → bounds exercised → mixed versions → real caller boundary → unverified); the known limits; and what is another role's.

**Why:** the owner's 2026-10-02 assignment on how the role designs and verifies.
**How to apply:** open batch 5 with a contract proposal to rust-ui-dev-01 (keep a proposal apart from an approved contract), then build. First fold in the three #166 carries ([[pr166-armed-carry-to-batch-5]]).

### Agreed 2026-10-02 (relay seqs 10522 proposal, 10534 amendments, 10540 agreement; replay them for the full text)
- Outbound statuses: Sending{attempts,next_retry_at,last_problem}, Unconfirmed{next_retry_at,last_problem} (Timeout/CancellationRaced: may have accepted), NeedsAttention{problem} (no auto retry; stays pending till retry()/cancel()), Accepted{endpoint}, Published, Cancelled{may_have_reached}. send() errs with NO row on encode refusal or store refusal.
- Transient retry unbounded: 1 s doubling, 5 min cap, deterministic per-row jitter. On open every pending row reported once; former NeedsAttention attempted once.
- SendProblem: PeerUntrusted(UnauthorizedPeer,PeerUnknown), RouteUnavailable, NoNetworkPath, Busy, ServiceUnavailable(BackendUnavailable,ShuttingDown), Incompatible(ProtocolUnsupported,VersionIncompatible,ProtocolViolation), TooLarge, Internal; transient = NoNetworkPath, Busy, ServiceUnavailable, RouteUnavailable.
- SessionProblem: EndpointInUse | NotAvailableToThisClient(Unknown,Disabled,ClientKindDenied,CapabilityDenied) | Internal.
- Inbound drain(max): commit unread first; Received{row, origin: Direct{peer,endpoint asserted} | Channel{channel,publisher}, envelope: HumanChatV2}; store-UNIQUE duplicate not yielded; residual post-read dup is ui-model's; uncommittable -> not yielded + StorageDegraded; undecodable -> discarded, counted (undecodable_discarded), never stored (architect-cto seq 10528).
- SessionState: Ready{endpoint}, Reconnecting{attempt,next_at}, Refused{problem} (only reopen() leaves), StorageDegraded (lease released, joins suspended; recheck on tick 5 s->5 min, and callable), Closed. EndpointLeaseChanged while Ready -> Reconnecting. PeerDisconnected passes through.
- Connectivity via the admin Status connection: OnlineDirect/OnlineRelay/OnlinePartial/Offline/Unknown (never Unknown as Offline).
- Poll-driven tick(now), one caller monotonic ms clock; queue coalesced per row + session + connectivity, latest wins, never drops terminal/session.
- Retry transport MessageId: schema v6 pending_outbound.transport_message_id, 16 random bytes minted at commit-pending (architect-cto seq 10528). architect-cto supplies the HUMAN-CHAT.md text on the branch.

*References: pr166-armed-carry-to-batch-5*

*Observed 2026-10-01 (p2p-network-dev)*
