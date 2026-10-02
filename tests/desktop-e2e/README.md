# desktop-e2e

Real desktop process tests using daemon, human client harness, Claude bridge harness and transportctl; lifecycle/restart/admin split/packaging behaviors.

**Current status:** Stage 13, active workspace member, covering the daemon's share of plan §16's required suite: `tests/daemon.rs` starts `transport-daemon` from a profile file alone, each in its own XDG tree, and drives it over its sockets (lock conflict, `kill -9`, stale and foreign socket paths, 0700/0600, a missing key, restart epochs, the admin/data split, two shipped desktop-example daemons exchanging direct and broadcast over IPC with no payload in the log). The binary is the workspace's own build: a workspace test run builds it; a missing binary fails the suite by name. `transportctl` joins with its batch.

Stage 14 adds `tests/human_chat.rs` (plan §17 (7)): two `human-desktop.yaml` daemons, each driven by the human client's own transport facade over `ipc-client` with its own store, exchange HumanChatV2 direct and broadcast, plain and brotli-compressed (`;ce=br`), in both directions. Every payload a daemon handed a client is captured as delivered and validated against `human-chat/envelope.schema.json`. The two suites share the harness in `tests/common/`: a daemon's XDG tree, the daemon and `transportctl` processes, and the shipped examples made concrete. The test does not prove a network other than one host's private address, a restart (Stage 15's process kill), or the `AcceptedV2` to unread-commit window, which is carried rather than closed.

See [`architecture/docs/architecture/testing.md`](../../architecture/docs/architecture/testing.md) for normative scenarios and exit criteria.
