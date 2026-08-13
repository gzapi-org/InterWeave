# Bottom-up dependency-gated implementation order

**Status:** Accepted

## Context

The architecture contains a historical numbered roadmap that groups work by product/release concern: contracts, minimal libp2p, discovery, connection management, daemon/IPC, clients, security, operations, and mandatory Internet reachability.

That phase numbering is useful for scope accounting but is not a safe literal construction order. Several rust-libp2p behaviours can originate network activity autonomously once added to a Swarm. In particular, Kademlia queries, AutoNAT, Circuit Relay, and DCUtR can cause dials or connection changes. The accepted architecture requires every outbound dial, including behaviour-originated dials, to cross the root `DialAdmissionGate` and ConnectionManager policy boundary.

Likewise, desktop IPC and Android embedded bindings are intended to be bindings of the same `LocalDataSession` semantics rather than independently invented behavior. Human retention also needs durable-store semantics proven before network delivery relies on them.

A literal phase-by-phase build could therefore enable higher-risk behavior before the policy/state/storage layers that are supposed to constrain it exist.

## Decision

Implementation SHALL follow the dependency-gated construction order in [`roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md`](../roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md).

The canonical order is summarized as:

```text
foundation/fixtures
-> neutral contracts/config
-> pure policies/state machines
-> persistence
-> minimal authenticated libp2p
-> root connection/dial admission
-> direct v2
-> GossipSub
-> endpoint directory
-> non-Kademlia discovery
-> Kademlia
-> mandatory AutoNAT/Relay/DCUtR
-> TransportRuntime composition
-> daemon/IPC
-> human/Claude integrations
-> Android platform binding
-> full security gate
-> packaging/release
```

The existing numbered phase documents remain accepted as **scope/release phase descriptions**, but they do not override this dependency order.

A higher stage may not become functional until the required lower-stage contract/fixture/conformance gates pass.

Spikes are run just-in-time immediately before the implementation boundary they unlock. Validated spike behavior must be converted into permanent regression/conformance tests rather than remaining only in spike code or prose.

The root Cargo workspace is activated incrementally: only packages required for the active stage are added as members. The repository must not create all planned crates/manifests up front solely to satisfy the blueprint.

## Alternatives considered

### Implement in the historical numbered phase order

Rejected as a literal construction sequence because it can activate Kademlia or mandatory connectivity behavior before the root dial/admission boundary is fully implemented and tested.

### Build the complete libp2p Swarm first and retrofit policy later

Rejected. Autonomous behaviour-originated dials would exist before their trust/backoff/resource policy funnel, weakening the most important security ownership invariant.

### Build UI/clients first against ad-hoc mocks and reconcile contracts later

Rejected as the primary integration strategy. UI work may proceed in parallel against the frozen `LocalDataSession` contract, but production integration cannot invent alternate local/network semantics.

### Activate every planned Cargo member at project start

Rejected. It creates empty packages and dependency pressure without implementation value and makes layer-boundary violations easier.

## Consequences

Positive:

- policy/security boundaries exist before autonomous network behavior;
- each layer is independently testable;
- fixtures/contracts become executable early;
- persistence behavior is proven before delivery depends on it;
- desktop and Android can share LocalDataSession conformance;
- spikes are tied directly to decisions and regression tests;
- failures are localized to the lowest responsible layer.

Costs:

- the implementation order no longer visually matches historical phase numbering;
- some product-visible work appears later even when UI prototypes can be developed in parallel;
- teams must maintain explicit stage gates and dependency checks;
- certain crates are activated later than developers might prefer for convenience.

## Security implications

This order is itself a security control. It ensures:

- root dial admission exists before Kademlia/AutoNAT/Relay/DCUtR can dial;
- pre-Noise resource limits exist before Internet-facing roles are enabled;
- direct dedup/rate/admission state is proven before network request concurrency relies on it;
- LocalAdminPort/data authority semantics are proven before platform bindings expose administration;
- durable human-message behavior is proven before clients accept messages under the unread-survival guarantee.

Skipping lower-stage gates may create network behavior that violates accepted trust, backpressure, or persistence boundaries and is therefore not conformant.

## Operational implications

CI and release automation should expose stage-oriented commands/gates. The project can still run parallel workstreams after neutral contracts/persistence stabilize, but integration occurs only through accepted boundaries.

Milestones M1-M5 in the canonical plan become the main implementation progress checkpoints.

## Implementation implications

- Add crate/test manifests incrementally when their stage begins.
- `xtask` should eventually provide commands for fixture checks, dependency-boundary checks, conformance suites, and milestone gates.
- `tests/support` remains test-only.
- Production packages never depend on spike packages.
- The implementation plan and CI must preserve the distinction between historical release phases and canonical construction stages.

## Revisit conditions

Revisit if:

- rust-libp2p changes so autonomous behaviour dials can no longer be controlled at the accepted root boundary;
- a future backend has materially different dependency/security sequencing;
- empirical spike evidence proves a prerequisite is wrong;
- the project splits into separately versioned repositories that cannot share these stage gates.

A convenience preference or desire to start a higher-level feature earlier is not sufficient to remove the dependency gates.
