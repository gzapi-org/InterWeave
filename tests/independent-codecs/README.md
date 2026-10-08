# independent-codecs

Stage 17's independent codecs (plan §20 (c); `architecture/docs/architecture/testing.md` §Compatibility fixtures, A 2026-10-08). There is one test-only encoder/decoder per frozen wire shape, written from the contract text and from nothing in this repository's code:

| shape | module | contract text |
|---|---|---|
| `DirectMessageV2` request frame | `direct_v2` | `transport/libp2p/DIRECT.md` §Request |
| `BroadcastMessageV1` envelope | `broadcast_v1` | `transport/libp2p/PUBSUB.md` |
| `DirectContentFingerprintV1` | `fingerprint` | `contracts/ENDPOINTS.md` |
| IPC v2 frame and envelope | `ipc_v2` (body JSON: `json`) | `contracts/LOCAL-IPC.md` §Framing, §Message classes; schemas `ipc/frame`, `ipc/hello`, `ipc/hello-response`, `ipc/close` |
| `AcceptedV2` / `RejectedV2` | — | **blocked**: `DIRECT.md` §Response states no byte layout. The draft text and frozen vector are on this branch for architect-cto's spec review. The codec will be written from that text once it lands. |

**Independent means no path package in the graph.** `tools/checks/check_independent_codecs.sh` refuses any path package in this crate's dependency graph, at any depth, with dev-dependencies included at the first hop. That covers a workspace member, `tests/support` and a vendored `third_party/` tree. The layering checks follow no dev-dependency, so they could not hold this.

**What the tests here prove.** `tests/frozen_vectors.rs` decodes every vector in the frozen fixture files for these shapes. It checks each decoded field against the vector's declared ones and re-encodes byte-equal. `tests/refusals.rs` checks every contract bound beside a positive control.

**What they do not prove.** Agreement with production is proved in `tests/interoperability`, which captures production-encoded frames, decodes them here, and feeds this crate's encodings to production. Where the two disagree, the contract text is the oracle. A disagreement is a contract finding for architect-cto, never a test to bend.

The IPC codec holds the envelope: the class, its allowed and required top-level members, and the frame schema's own rules. A request's params and an event's data pass through as JSON. They are the method and event catalogue's, owned by `ipc/request` and `ipc/event`.

The JSON reader and writer are hand-written. The goldens freeze bodies in key order, and `preserve_order` is enabled nowhere in the workspace.
