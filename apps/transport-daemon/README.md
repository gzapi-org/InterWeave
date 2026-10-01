# transport-daemon

Desktop/server composition root for one profile-scoped `TransportRuntime`, discovery providers, libp2p backend, and IPC v2 data/admin acceptors.

**Current status:** Stage 13, active workspace member. `transport-daemon --profile <name> [--create-identity]` runs one profile in the foreground, in `architecture/docs/architecture/lifecycle.md`'s order: load and validate the profile (an `embedded-android` one is refused), take the profile lock, load the identity key (a missing one is fatal unless `--create-identity` and no key file exists), clear this user's stale sockets as the lock's holder and bind both owner-only sockets, start the composed runtime, serve IPC. SIGINT, SIGTERM or an admin port's `admin.shutdown` stops it: the server (every lease released), the runtime, both sockets unlinked, the lock released and never unlinked. Logging goes to stderr at the profile's `observability.log_level` and no other; payload bytes are never logged.

Exit codes: 0 a clean stop, 1 a refused start or a failed run (the reason printed), 2 a usage error. The profile is required: nothing names a default one.

`tests/desktop-e2e` runs the binary as a process.
