---
role: "p2p-network-dev"
class: solution
topic: "dns-wrap-reshapes-every-dial-error"
description: "wrapping the base transport in libp2p-dns changes the error of EVERY dial, not only a name's — MultiaddrNotSupported stops arriving, so classify an undialable address by its own shape"
tier: 2
knowledge_scope: full
distilled_at: "2026-09-26"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - e3311b9996829b8d
---

## wrapping the base transport in libp2p-dns changes the error of EVERY dial, not only a name's — MultiaddrNotSupported stops arriving, so classify an undialable address by its own shape

Building the DNS transport (`.with_dns()`, `a8c73e6`, PR #111) broke
ADR-0010's address-versus-peer distinction for addresses that were
never names. CI caught it on a plain `/ip4/127.0.0.1/udp/1`:
`an_unsupported_local_address_is_reported_once_and_never_retried` saw
the address retried instead of dropped.

**Mechanism, read from the pinned crates.** `libp2p-dns 0.45.0`
`src/lib.rs:298` turns the inner transport's
`TransportError::MultiaddrNotSupported(a)` into its OWN
`dns::Error::MultiaddrNotSupported(a)`, collects it into
`dns::Error::Dial(vec![..])` and surfaces `TransportError::Other`; its
`do_dial` accepts every address and defers, so the outer variant never
appears. `libp2p-core 0.44.0` `transport/boxed.rs:173` then boxes it
with `io::Error::other`. The kind survives as a VALUE inside
`dns::Error<TInner::Error>` whose generic parameter is the builder's
authenticated, multiplexed transport — not nameable, so no downcast.

**The fix asks the address** (`c060ce8`,
`address_has_no_transport_in_this_build`): no `tcp` hop and no
`p2p-circuit` means no transport this builder composes can dial it. It
answers "certainly not" or "do not know", never "certainly yes", and
falls through to the variant match otherwise. The `p2p-circuit` arm is
load-bearing: a QUIC relay hop has no `tcp` component.

**Why:** error-text matching is ruled out here by name — see
[[error-text-matching-pins-the-public-type]].

**How to apply:** any time a transport wrapper is added or changed,
assume every error classifier that matches a `TransportError` variant
is now blind, and run the workspace tests (`cargo xtask ci`, never just
`checks`) before describing the change as done.

*References: error-text-matching-pins-the-public-type*

*Observed 2026-09-25 (p2p-network-dev)*
