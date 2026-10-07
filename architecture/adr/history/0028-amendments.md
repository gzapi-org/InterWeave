# ADR-0028 — amendment history

### Amendment 2026-09-28 — The profile lock lives in the state directory and is released, never unlinked; stale sockets go only under the lock; admin endpoint mutations are a runtime overlay

**Trigger.** Stage 13 (plan §16 (5)–(6)) builds the daemon and the
offline identity commands. The Decision said "runtime socket/lock" while
`configuration.md` filed lock metadata under mutable state, and nothing
said how a lock is taken, where a stale socket may be removed, or whether
an administrative endpoint change is configuration.

**What changed.** The lock lives in the state directory — reachable
offline, which the identity commands need — and is an exclusive
advisory whole-file lock (`std::fs::File::try_lock`) held for the
daemon's lifetime and by `transportctl identity backup`/`restore`, so a
running daemon makes restore fail at once; it is released and never
unlinked, because unlinking a flock file lets two processes lock
different inodes. Stale sockets are removed only by the lock holder and
only when the path is a socket owned by the daemon's uid; anything else
is fatal (failure-model.md's IPC bind security failure). Endpoint
enable/disable/default over `admin.endpoints.*` is a runtime overlay:
never written to `config.yaml`, lost on restart, reported as
`persisted: false`.

**Not changed.** The directory classes, the no-secrets rule, the
never-auto-regenerate rule for the key.

### Amendment 2026-10-07 — Admin trust changes are a persisted overlay in the state directory

**Trigger.** rust-ui-dev measured on the Stage 16 human client that a
peer revoked with `admin.trust.set` was trusted again after the daemon
restarted: trust administration (ADR-0037, A 2026-10-03) had been built
on the endpoint overlay's rule — runtime only, `persisted: false` — and
LOCAL-IPC.md said so "until the owner decides persistence". The owner
decided on 2026-10-07: persist the deltas in the daemon's state (option 1
of the four architect-cto offered: state overlay; the daemon writing
`config.yaml`; the client re-applying its changes; leave as is).

**What changed.** The Decision gains the persisted-overlay paragraph:
`<state>/trust-overlay.json`, owner-only and refused when not the
daemon's own or readable or writable by anyone but its owner; two lists, `added` (beyond
`config.yaml`'s `trust.allowed_peers`) and `revoked` (configured peers
revoked); the effective allowlist (configured ∪ added) ∖ revoked, derived
before the first connection is admitted, fatal above `MAX_ALLOWED_PEERS`
rather than truncated; the overlay normalised at load against the
configuration the daemon started with (a peer in both `added` and the
configuration leaves `added`; a peer in `revoked` but not configured
leaves `revoked`; the file rewritten when that changed anything), so an
operator's `config.yaml` edit between restarts is never undone by a stale
entry — the supplier review found that without the normalisation a peer
both configured and `added` would be answered `ok` on a revoke and stay
trusted; four set moves on the normalised lists; the file written whole
and renamed into place before the policy is published to the runtime and
before the set is answered, so a failed write changes nothing (audit
outcome `unwritten`), and a publish that fails after the write, or a directory sync that
fails after the rename, restores the previous overlay before the failed
answer — and when the restore fails too the set, an allow as much as a
revocation, is left ahead of the runtime and takes effect at the next
start, answered `Internal`, audited `failed`, logged (two filesystem
failures in a row; stopping the daemon instead was rejected); the store is a port the
composition takes at construction, supplied by the daemon and by the
embedded runtime alike;
`config.yaml` never written; a present overlay that does not parse,
names a peer in both lists, is not the daemon's own or is readable or writable by
anyone but its owner fatal, never skipped; a failed normalisation rewrite at load
fatal, since a stale entry left on disk could undo the operator's next
`config.yaml` edit. The overlay is durable authorisation in its own backup class:
backed up with the profile, never deleted to reset. `admin.trust.list`
reports `persisted: true` and a `source` of `configured | administered`
per row on a connection that negotiated IPC 2.3 or later; below it the
2.1 row shape is unchanged, because `LOCAL-IPC.md` §Version negotiation
makes a change to a closed shape a major once the first production build
spoke 2.0 and `trust-list` is `active` — so ADR-0017 is amended the same
day to let a closed result shape widen behind a new minor while the old
shape is served below it, and the row does that (`ipc/trust-list` 1.1.0:
`persisted` a boolean, `source` optional, with the minor that serves
each); the Rust mirror follows in the implementing PR. The Alternatives name the three rejected routes; the
Consequences and Security implications say the allowlist restarts as the
operator left it and what the file is.

**Not changed.** Endpoint enable/disable/default stays the runtime
overlay of 2026-09-28; persisting it is the carried item of plan §20 and
may reuse the file shape. The no-secrets rule: a PeerId is not a secret.
The never-auto-regenerate rule for the key.

**Where it lands.** The daemon's overlay store, IPC 2.3 and the Rust mirror are
p2p-network-dev's; the human client's trust copy and its settings read
are rust-ui-dev's ("until the transport daemon restarts" goes, and the
human client negotiates 2.3 so it never reads the stale 2.1 row); one
integration PR, p2p-network-dev integrating.

