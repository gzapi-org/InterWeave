# desktop-e2e

Real desktop process tests using daemon, human client harness, Claude bridge harness and transportctl; lifecycle/restart/admin split/packaging behaviors.

**Current status:** Stage 13, active workspace member, covering the daemon's share of plan §16's required suite: `tests/daemon.rs` starts `transport-daemon` from a profile file alone, each in its own XDG tree, and drives it over its sockets (lock conflict, `kill -9`, stale and foreign socket paths, 0700/0600, a missing key, restart epochs, the admin/data split, two shipped desktop-example daemons exchanging direct and broadcast over IPC with no payload in the log). The binary is the workspace's own build: a workspace test run builds it; a missing binary fails the suite by name. `transportctl` joins with its batch.

See [`architecture/docs/architecture/testing.md`](../../architecture/docs/architecture/testing.md) for normative scenarios and exit criteria.
