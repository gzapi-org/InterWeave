# StaticBootstrapDiscovery

Purpose: configured reachability entry points.

Example hint:

```text
/dns4/bootstrap.example.net/tcp/4001/p2p/<PeerId>
```

## Semantics

Static peers are candidate addresses with configured provenance. They are **not**:

- identity authorities;
- trust roots;
- membership servers;
- coordinators;
- brokers;
- message stores;
- channel owners;
- required permanent infrastructure after peers learn alternative reachability.

A configured bootstrap PeerId still requires an explicit trust rule before ConnectionManager may establish/retain an ordinary v1 data-plane connection. Configuration does not grant trust.

## DNS ownership

`StaticBootstrapDiscovery` validates and emits configured `/dns4`/`/dns6` multiaddresses **without eagerly resolving them**. Its health covers configuration parsing, provider lifecycle, and its ability to emit configured candidate observations.

DNS resolution occurs when the libp2p/ConnectionManager dial path consumes the multiaddress. DNS lookup failures are therefore **dial/connection diagnostics**, not discovery-provider health failures. ConnectionManager applies its normal bounded retry/backoff policy. This separation avoids making the provider claim visibility into failures that occur only during dialing.

**Built 2026-09-20 — the paragraph above holds.** Two dated paragraphs stood here from 2026-09-19 recording that the build did not honour it: the Swarm was built `with_tcp` alone, a `/dns4` or `/dns6` dial failed `MultiaddrNotSupported`, which `dialing.rs` classifies as structural, so `record_permanent_failure` forgot the address instead of retrying it, and `profile-config` refused a configured `dns4`/`dns6` host (`AddressHostNotBuilt`) as the repository's rule for a capability the build omits. They said the change that builds the DNS transport into the Swarm deletes them, and it has: the builder is `with_tcp(...)` then `.with_dns()` (`crates/transport/libp2p/src/runtime/mod.rs`), wrapping the base transport in `libp2p_dns::tokio::Transport::system`; the `dns` feature is on the root manifest's list and `profile-config`'s `DIALABLE_HOST_PROTOCOLS` carries `dns4` and `dns6`, held together by `tools/checks/check_dialable_hosts.sh`; and `crates/transport/libp2p/tests/dns_transport.rs` is the test the Stage 12 precondition named — it starts the real runtime, dials a `/dns4` name under RFC 6761's never-resolving `.invalid` and asserts positively on the resolver diagnostic, because with `.with_dns()` removed the same test fails (measured, both strings recorded beside the assertion). A `/dns4` dial therefore no longer produces `MultiaddrNotSupported` — `attempt_is_structural` is unchanged and correct, and is simply never reached for a name — so a lookup failure is an ordinary dial diagnostic the ConnectionManager retries under its bounded policy, which is what ADR-0010 and the paragraph above say. The refusal is lifted: a configured `dns4` bootstrap peer, static server or static relay validates, and the six shipped examples that name `/dns4` hosts no longer draw it (whether each validates now turns only on the providers it enables). **One deployment-visible change rides with it**: `Transport::system` reads the host's resolver configuration at construction, so a host with none fails to START with a named transport error (`SubstrateError::Transport`), where before it started and resolved nothing — the louder failure, and the right one for a capability a profile may now name; an operator reads it at startup, not at the first dial. `AddressHostNotBuilt` stays in the vocabulary with no constructible input — every host the grammar accepts is now dialable — dormant for the next host the vocabulary admits before its transport, which is the sequence `dns4` has just completed; the three assertions that rested on it were removed rather than re-pinned through the grammar, whose refusal answers a different question. NOT MEASURED: a successful resolution end to end. The test proves the transport is built by naming the failure a resolver produces; nothing composes this provider before Stage 12, so no configured name is dialled today.

## Configuration

Default max entries: 64. Invalid PeerId/multiaddress syntax fails config validation. A valid DNS multiaddress whose hostname later fails to resolve does not invalidate the provider configuration; the dial attempt reports `PeerUnreachable`/address-resolution diagnostics as appropriate. That is the target; what a build without the `dns` feature does instead, and the precondition that holds until it has one, is §DNS ownership.

### The accepted address vocabulary

"Invalid multiaddress syntax" above needs a vocabulary to be checkable, and this is it. A configured entry is `/<host>/<value>/<transport>/<port>/p2p/<PeerId>`, where:

- **host** is one of `ip4`, `ip6`, `dns4`, `dns6` — a build without the `dns` feature cannot dial a `dns4`/`dns6` host and refuses one at validation (`AddressHostNotBuilt`; §DNS ownership);
- **transport** is `tcp` — the only transport the substrate builds;
- **port** is `0..=65535`;
- an `ip4` value is a dotted quad, an `ip6` value is hexadecimal, and a DNS name is **not resolved** (see above).

This is a decision, not a description of what a multiaddr parser happens to accept, and it is deliberately narrower than the general multiaddr grammar: a profile naming a transport this build cannot dial is a configuration error an operator should read at validation, not a dial failure later. The set widens in the change that adds the transport — a new listen or dial capability and the configuration that may name it belong in the same commit.

It is spelled out in `interweave-profile-config` rather than delegated to the backend's parser. A configuration crate that pulled in a networking stack to name four protocols would invert the layering `crates/api` exists to hold, and would make the accepted set a property of a dependency rather than of this document.
