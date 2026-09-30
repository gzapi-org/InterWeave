# ipc-server

The desktop IPC v2 server: the data and admin Unix sockets, over the neutral local-session binding.

**Current status:** Stage 13, active workspace member. A library generic over `DataSessionBinding` and `AdminBinding` (`interweave-local-client-api`); it depends on nothing under `crates/transport/*` and on no libp2p crate, so it cannot hold a lease table or an event queue of its own. Unix only.

## The socket is the authority

The two listeners are kept apart from the bind onwards, and a connection is tagged by the socket that accepted it before its first byte is read (ADR-0037). The run directory is owner-only (0700, owned by this process's uid, never a link) and each socket is 0600.

## What it does not do

It never removes a path already at a socket's location: only the holder of the profile lock may unlink a stale socket, and the lock is the daemon's (plan §16 (6)).
