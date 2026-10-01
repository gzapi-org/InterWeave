# transportctl

Offline/local administrative CLI for diagnostics, trust/config administration, identity recovery/verification, and service management. It never becomes a network peer on its own.

**Current status:** Stage 13, active workspace member. The admin commands speak to a running daemon over its admin socket: `status [--json]`, `endpoints list [--json]`, `endpoints revoke|enable|disable <endpoint>`, `endpoints default <endpoint>|--none`, `shutdown [--grace <ms>]` (no `--grace`: none sent, the daemon's default). `--json` prints the method's result object as the daemon sent it. When no daemon answers, the profile lock tells "no daemon is running" from "a daemon holds the lock but its socket does not answer".

Exit codes: 0 done, 1 refused (the daemon's error code printed), 2 a usage error, 3 the daemon unreachable. A recovery phrase is never an argument.
