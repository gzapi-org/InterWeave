# transportctl

Offline/local administrative CLI for diagnostics, trust/config administration, identity recovery/verification, and service management. It never becomes a network peer on its own.

**Current status:** Stage 13, active workspace member. The admin commands speak to a running daemon over its admin socket: `status [--json]`, `endpoints list [--json]`, `endpoints revoke|enable|disable <endpoint>`, `endpoints default <endpoint>|--none`, `trust list [--json]`, `trust allow|revoke <peer>` (IPC 2.1's `admin.trust`, Stage 15), `peers list [--json]` (IPC 2.2's `admin.peers.list` under `admin.status`: the dial gate's state per allowlisted peer, never an address; a daemon older than 2.2 is told so before any request), `shutdown [--grace <ms>]` (no `--grace`: none sent, the daemon's default). `--json` prints the method's result object as the daemon sent it -- for `trust list`, one `ipc/trust-list` page per line. A trust command against a daemon older than 2.1 is refused before any request: the port comes back without `admin.trust`. When no daemon answers, the profile lock tells "no daemon is running" from "a daemon holds the lock but its socket does not answer".

The identity commands never touch a socket (ADR-0033, `IDENTITY-RECOVERY.md`):
- `identity backup [--to-file <new path>]` takes the profile lock, so it is refused while the daemon runs. It emits the recovery record to stdout only when stdout is a terminal, otherwise only to a new file created `0600`.
- `identity verify [--expected-peer-id <peer>]` is the drill. It takes no lock and touches no key, profile or socket.
- `identity restore --new | --replace --replacing <peer>` takes the lock before asking for the phrase. `--new` restores into a profile with no key. `--replace` restores only over the identity it names. The key written is loaded back and its `PeerId` checked before success is reported, and nothing but the key is written.

The phrase, or a recovery record, is read from stdin only: through `rpassword` with echo off on a terminal, otherwise from the pipe, bounded. Two limits of that crate are handled in `src/phrase.rs`:
- Its Ctrl-C raises `SIGINT` before it restores the terminal, so a handler is registered first.
- Its buffer leaves unzeroed fragments when it grows, a limit stated there. What it returns is held in `Zeroizing`.

Exit codes: 0 done, 1 refused (the reason, or the daemon's error code, printed), 2 a usage error, 3 the daemon unreachable. A recovery phrase is never an argument.
