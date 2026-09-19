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

**Where the substrate stands, recorded 2026-09-19.** The paragraph above is the target, and the rule is ADR-0010's: dial-time DNS failure is a ConnectionManager diagnostic. The build does not honour it yet. libp2p's `dns` feature is not on the feature list (root `Cargo.toml`), so the Swarm is built `with_tcp` alone (plus the relay client's transport when one is configured), neither of which resolves a name, and a `/dns4` or `/dns6` address is never resolved: the dial fails `MultiaddrNotSupported`, which `crates/transport/libp2p/src/runtime/dialing.rs` classifies as structural — correctly, for a build that would fail the same address the same way on every retry — so `record_permanent_failure` removes the address from the book. There is no lookup diagnostic and no retry, and a reader who followed this section would expect a retryable diagnostic and get a forgotten address. What reaches that path today is an Identify-learned or `LearnAddress`-learned name; a configured bootstrap entry does not, because nothing composes this provider before Stage 12. The feature is bound to the same dependency decision as `mdns` — on `libp2p 0.56` it pulls `hickory-resolver` on the `hickory-proto` 0.25 line the RUSTSEC advisories name, and `libp2p 0.57` clears both — and the plan's Stage 11 obligations own that decision and its closure. This section is deliberately **not** narrowed to what a TCP-only build does: the target is the design, and the gap is stated here, where a reader meets it, until the change that enables `dns` deletes this paragraph and the next.

**Until then, a configured `dns4`/`dns6` host is to be refused at profile validation — the repository's own rule for a capability the build omits, and one the code has yet to conform to.** As of 2026-09-19 `profile-config` still accepts such a host (`ADDRESS_HOST_PROTOCOLS` and the address grammar accept a syntactically valid name, and its shipped-examples test filters only `DiscoveryProviderNotImplemented`); the refusal is the decision, the acceptance is the gap, and the conformance lands in `profile-config` beside this rule. `PROVIDER-CONTRACT.md` says a provider that configuration enables and the build omits is refused rather than started healthy, and `profile-config` applies it today (`DiscoveryProviderNotImplemented`, for an enabled `mdns` or `kademlia` entry). A name this build cannot resolve is the same case one field over, and the vocabulary below states the principle: a profile naming what this build cannot dial is a configuration error an operator reads at validation, not a dial failure later. The refusal lives in profile validation, not in this provider — the provider keeps emitting names unresolved, which is what its contract and its tests say — and it lifts in the same change that puts `dns` on the feature list; the set widens with the transport. What it costs the shipped examples is nothing new: six of the ten under `architecture/config/examples/` name `/dns4` hosts (`composite-discovery`, `human-android`, `human-desktop`, `internet-reachability`, `kademlia-enabled`, `remote-bootstrap`; none names `/dns6`), and every one of the six is already refused today for an enabled provider the build omits — a stage fact, not a bad profile, which `tests/shipped_examples.rs` filters out before asserting; this refusal joins that filter when it lands. The other four name no DNS host and are untouched by it. Nothing dials a configured name before Stage 12 either way; the plan's Stage 12 precondition says what composition may assume, and until the refusal is in the code that precondition is a review obligation rather than a mechanical one.

## Configuration

Default max entries: 64. Invalid PeerId/multiaddress syntax fails config validation. A valid DNS multiaddress whose hostname later fails to resolve does not invalidate the provider configuration; the dial attempt reports `PeerUnreachable`/address-resolution diagnostics as appropriate. That is the target; what a build without the `dns` feature does instead, and the precondition that holds until it has one, is §DNS ownership.

### The accepted address vocabulary

"Invalid multiaddress syntax" above needs a vocabulary to be checkable, and this is it. A configured entry is `/<host>/<value>/<transport>/<port>/p2p/<PeerId>`, where:

- **host** is one of `ip4`, `ip6`, `dns4`, `dns6` — a build without the `dns` feature cannot dial a `dns4`/`dns6` host and is to refuse one at validation; as of 2026-09-19 it still accepts one (§DNS ownership);
- **transport** is `tcp` — the only transport the substrate builds;
- **port** is `0..=65535`;
- an `ip4` value is a dotted quad, an `ip6` value is hexadecimal, and a DNS name is **not resolved** (see above).

This is a decision, not a description of what a multiaddr parser happens to accept, and it is deliberately narrower than the general multiaddr grammar: a profile naming a transport this build cannot dial is a configuration error an operator should read at validation, not a dial failure later. The set widens in the change that adds the transport — a new listen or dial capability and the configuration that may name it belong in the same commit.

It is spelled out in `interweave-profile-config` rather than delegated to the backend's parser. A configuration crate that pulled in a networking stack to name four protocols would invert the layering `crates/api` exists to hold, and would make the accepted set a property of a dependency rather than of this document.
