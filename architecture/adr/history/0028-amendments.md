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
