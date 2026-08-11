# Explicit DiscoveryProvider interface

**Status:** Accepted

## Context

Discovery mechanisms are required to be addable, removable, composable, and replaceable independently from transport consumers.

## Decision

Define an event-stream-oriented `DiscoveryProvider` contract consumed only by DiscoveryManager. Providers emit normalized candidate PeerIds/addresses/provenance/expiry and health; they do not dial or grant trust.

## Alternatives considered

Hard-code mDNS/Kademlia in the Swarm; expose provider-specific APIs to Transport; dynamically loaded provider ABI from day one.

## Consequences

One trait is justified by real independent variation. It adds adapter/conformance work but prevents provider leakage into higher layers.

## Security implications

Provider output is untrusted reachability data. The contract explicitly prevents discovery from becoming authorization.

## Operational implications

Providers fail independently and expose health. Operators can disable problematic mechanisms without redesigning the runtime.

## Implementation implications

Use a stable contract crate without libp2p. Provider implementations adapt library-specific events into normalized events. Maintain a conformance suite.

## Revisit conditions

Revisit only if all real providers converge on one implementation with no independent lifecycle; otherwise preserve the interface.
