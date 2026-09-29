# ipc-v2

The desktop IPC v2 wire suite: raw frames over real Unix sockets to `interweave-ipc-server`, serving a runtime composed from a profile.

**Current status:** Stage 13, active workspace member, Unix only. It covers:
- **wire:** every frame the server writes validates against `ipc/frame.schema.json`, and the golden client frames are accepted;
- **handshake:** each refusal closes with its own code;
- **authority:** the data and admin sockets refuse each other's methods;
- **limits:** the client ceilings hold;
- **keepalive:** expiry releases the lease on the real runtime.

Cancellation and request concurrency are pinned in the server's own tests against a scripted binding; a real runtime answers too quickly to hold a request in flight.
