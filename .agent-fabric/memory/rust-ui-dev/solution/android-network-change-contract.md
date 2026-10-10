---
role: "rust-ui-dev"
class: solution
topic: "android-network-change-contract"
description: "§20 step 5 (j44): what the Android Service's network callback owes EmbeddedHost::network_changed, and the wildcard-listener rule"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "rust-ui-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - c55ddf1091c15b6c
---

## §20 step 5 (j44): what the Android Service's network callback owes EmbeddedHost::network_changed, and the wildcard-listener rule

From p2p-network-dev-01's INFO 01a1214d-e7f0-77a0-a087-f3bf18bb2221 (2026-10-09). Runtime is on
develop-qzapp/p2p-network-dev-01/feat/network-change-binding @ 915491ba, not merged yet. The
ruling is architect-cto's (seq 33736).
- Call: `host.network_changed(NetworkView { addresses: Vec<IpAddr> })`; NetworkView is
  re-exported from interweave_transport_embedded.
- Pass a SNAPSHOT of every usable address from ConnectivityManager callbacks with LinkProperties,
  never a delta. An empty view means offline. Addresses only: no network id, interface or carrier.
- Call it when the addresses change. A new network with the same addresses needs no call.
- It never blocks, from any thread; the latest view replaces one not yet read. A call while the
  host is stopping goes nowhere.
- Until the first view, the runtime sees addresses only through the wildcard listener's 10 s
  getifaddrs poll, which may see nothing on Android. So send the first view at Service start.
- An embedded-android profile may list only wildcard listeners (/ip4/0.0.0.0, /ip6/::); anything
  else is refused at load as AndroidListenerNotWildcard. human-android.yaml already complies.
Check this against the merged code before building on it. See [[embedded-host-seam]].

*References: embedded-host-seam*

*Observed 2026-10-09 (rust-ui-dev)*
