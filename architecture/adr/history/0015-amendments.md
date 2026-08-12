# ADR-0015 — amendment history

### Amendment 2026-08-12 — Android embeds the runtime instead of running a daemon

ADR-0041 selected an Android-specific deployment binding: the first-party Android app embeds the same Rust `TransportRuntime` inside a foreground-service host rather than launching a standalone daemon. The Decision section is amended to state that binding directly.

This is **not** a second networking architecture. PeerId ownership, trust, discovery, endpoints, Kademlia, and connectivity semantics are identical to the daemon deployment. The amendment exists because a standalone desktop-style daemon is a poor Android lifecycle and process primitive, not because the decision's substance changed — desktop, server, and Claude integrations retain the external daemon.

The text arrived as a trailing `## Android amendment` section, a convention predating ADR-0048. It is now folded into the Decision, where a reader needs it, and recorded here.
