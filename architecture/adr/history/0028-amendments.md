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

### Amendment 2026-10-08 — The ancestors of a private directory are judged with it

**Trigger.** p2p-network-dev's observation (GZCoord 01a119ce-b6fe, the
risk #192 carried at its line 69): the identity loader and every
private writer judge only the directory that holds the file —
`require_private_dir` (not a link, no group or other bit) and
`require_owned_private_dir_as` (the effective uid's) in
`crates/config/profile-config/src/persist.rs` — and no ancestor above
it. An account that can write any ancestor renames the private
directory away and puts its own in its place, mode and owner correct;
the loader then reads a key that account chose, or a writer puts the
key where that account reads it. IDENTITY.md promised owner-only
permissions and "filesystem/OS account protection at rest", and
neither it nor the threat model named the ancestors that protection
depends on. The same helpers serve the trust overlay and the profile
lock; the human store judges its directory through its lock.

**Decision.** The directory of every private file is judged with its
ancestors: every component of its canonical path, from its parent up
to `/`, is owned by root or by the daemon's effective uid and carries
no group- or other-write bit; a directory with the sticky bit set,
still root's or ours, is accepted whatever its write bits. A symbolic
link the configured path traverses is judged too: the link is owned by
root or ours and the directory holding it meets the same rule, its ancestors with it, since
whoever owns the link, or can write its directory, can repoint it as
surely as renaming the private directory; and the opens this note
lists below use the path that was resolved, not the configured text. The directory itself
keeps its stricter rule (not a link, no group or other bit, the
effective uid's). The configuration file's directory is judged the same
way — its ownership and write bits and its ancestors', not an owner-only
mode, since `config.yaml` is not secret — because `config.yaml` names
`identity.key_file` and `trust.allowed_peers`: an account that could
replace it would choose the key's directory (any accepted directory of
ours, another profile's among them) and admit its own peer, and
`ProfileConfig::load` opened it, at 67099004, with no judgement at all
(the re-review's finding). The file itself is opened without following a
final link and refused unless it is owned by root or ours with no
group- or other-write bit — readable by others is allowed, it is not
secret — since a group-writable file, a link of ours to a file another
account writes, or a file another account placed in a sticky
configuration directory meets the same reason (#224's review). The
resolved-path open holds for the key, read and write, the two private
writers (the overlay's write among them), the overlay's read, both
locks and `config.yaml`; the human store opens its file by its
configured path under the directory `HumanClientLock` judged, which
the carried item below covers. The walk runs once, in `require_private_dir` — the
`profile-config` helper the two private writers
(`write_private_atomic`, `create_private_exclusive`) call directly and
the identity loader and both locks reach through
`require_owned_private_dir_as` — on every call, a dozen `lstat` calls;
so the key on write and on load, the trust overlay through the
profile lock's judgement of the state directory before it is read, and
the human client's directory through `HumanClientLock`, get it with no
per-site switch. A failing ancestor or link is refused where the
directory's own rule refuses (at start for the key and the overlay),
naming the failing ancestor or link, its owner uid, its mode and the
rule it broke; a component that cannot be inspected is refused as
unresolved, not judged on the path's text (the stance
`LoadError::KeyFileUnresolved` already takes). No configuration
setting relaxes the rule.

**Why this shape.** The rule is not a check of the path at an instant;
it verifies the precondition under which the operating system makes
the swap impossible. In a sticky directory an entry is renamed or
unlinked only by the entry's owner, the directory's owner or root, so
an entry of ours under `/tmp` cannot be moved by another account — and
the ownership rule requires that sticky directory to be root's or ours,
else its owner could rename the entry. Root can do anything regardless;
any other owner of an ancestor can swap the directory whatever its
mode, so a non-root, non-self owner is refused even at `0755`. Links
are the one way an accepted canonical path could still be redirected,
which is why each traversed link and its directory are judged and the
resolved path is the one opened. When the walk passes, no account but
root and the daemon's can perform the rename or the repoint between
the check and the open, which is why no check-then-open race needs
answering.

**Alternatives rejected.** (b) No ancestor rule, the limit written into
IDENTITY.md and the threat model: leaves a promise the code cannot
keep, since "owner-only" says nothing an ancestor's owner cannot undo.
A configuration override that disables the walk: a knob that turns off
a precondition check is exactly what a hostile layout would ask for; a
refused layout is fixed with `chown`/`chmod`, which the refusal says.
Judging the canonical path alone: accepts a link another account owns
or can repoint, so it was widened to the links.

**Consequences.** Refused: a group-writable or shared home; a service
layout whose ancestor is owned by a deploy account other than the
daemon's (make it root's); a link on the path that another account
owns or whose directory another account can write. Unchanged: the
plain per-user layout (`/home` root's at `0755`, the home and
`~/.local/state` the user's), a container's root-owned tree, a sticky
`/tmp`. The rule speaks to local accounts: another host presenting the
daemon's uid over a network filesystem, or that filesystem's server,
is outside it, as disk theft is. Linux only — the effective uid is read
from `/proc`, so on every other target the writers now refuse as the
loader and locks did; they keep `UnsupportedPlatform`. A layout refused in the
field is reported to architect-cto as an observation and decides the
revisit; nothing is loosened ahead of one. Carried: the human store's
own directory helpers (`crates/human/store/src/store.rs`,
`create_private_dir` and `require_owner_only`) judge the directory
alone and reach no `profile-config` check; its directory is judged
when `HumanClientLock` is taken, before the store opens — routing those
helpers through the same funnel, and opening the store under the
directory as resolved, is rust-ui-dev's, named here.

**Propagation.** Security implications in the body; this note; the
log row; the digest entry. IDENTITY.md's at-rest sentence names the
rule it relies on; threat-model.md's private-key row cites it and a row
for the ancestor-or-link swap is added; configuration.md says which
layouts do not start. The code and its tests (a group-writable
ancestor refused, an other-writable non-sticky one refused, a sticky
world-writable one owned by root or us accepted, a sticky one owned by
a foreign uid refused, a foreign-uid owner refused at `0755`, a
root-owned `0755` one accepted, a link owned by us in a passing
directory with a passing target accepted, a link owned by a foreign uid
or held in a failing directory refused, an ancestor that cannot be
inspected refused as unresolved, a configuration file under a
foreign-writable or foreign-owned ancestor refused at load, a
group-writable `config.yaml` refused, a `config.yaml` that is a link
refused, the plain XDG layout as the control)
are p2p-network-dev's, on the same PR as this note.

**Closed 2026-10-08 (InterWeave #226, 36e6adc6, rust-ui-dev).** The carried item above is done: `HumanStore::open` checks the nearest existing ancestor with `profile-config`'s walk before creating anything, creates missing components one at a time, resolves a `..` after a missing component by text first, then judges the directory with `resolve_owned_private_dir` and opens the database and every companion (`-wal`, `-shm`, `-journal`, each checked as what is at its path, never followed) under the path that returns; a directory, ancestor or link that breaks the rule is refused as `StoreError::DirectoryNotPrivate`, naming it and the rule it broke, and a companion that is a link or not owner-only as `NotAFile` or `PermissionsTooOpen`. The body, the log row, the digest, the threat row and configuration.md read so.

### Amendment 2026-10-08 — The ancestor walk stops at the trust boundary the binding supplies

**Trigger.** rust-ui-dev's observation (GZCoord 01a11b1f-2246), while
routing the human store through the ancestor funnel on the desktop: the
2026-10-08 rule accepts a private directory only when every ancestor up
to `/` is owned by root or the effective uid with no group- or
other-write bit, and it binds every production binding, the Android
embedded runtime and its store included. On Android an app's private
directory sits under `/data` and `/data/data`, both `0771 system:system`
(AOSP `system/core/rootdir/init.rc`), reached through the `/data/user/0`
link — owned by uid 1000, not root and not the app's uid, group-
writable. The rule as written refuses every app-private directory
there: the human store, the trust overlay the embedded runtime keeps,
the profile lock. Not measured on a device (none attached; OEM layouts
may differ); the host-side store tests show the funnel refusing a
`0777` non-sticky ancestor, so `0771 system` is refused the same way.

**Decision.** The walk runs from the private directory's parent up to a
trust boundary the composition supplies to `profile-config`: the
directory above which the platform, not the account, owns the layout.
The boundary itself and everything below it are judged by the rule as
written (owner, write bits, sticky, links); nothing above it is. The
daemon and `transportctl` supply `/`, so on a Linux host nothing
changes. The Android embedded runtime supplies the app's own data
directory as the platform reports it (the parent of the files
directory the runtime is handed), never a hard-coded `/data/data/<pkg>`:
OEM layouts vary and are not ours to judge. Above that boundary the
platform owns and SELinux-confines the tree per app, and an app can
neither observe it reliably nor change it — the precondition the walk
verifies on a Linux host is the platform's guarantee there. The boundary is canonicalised once when it is
supplied and the walk stops at the canonical component equal to it —
the platform reports `/data/user/0/<pkg>` while the canonical path is
`/data/data/<pkg>` — and links traversed above it are not judged; links
on the path below the boundary are judged as before. A private
directory whose canonical path is not under the boundary at all — a
configured path elsewhere, or a link below the boundary resolving
outside it — is refused, naming the boundary: fail closed, never a walk
past it to `/`. The platform
creates the app's `files/`, `cache/` and `code_cache/` directories
group-writable (`0771`, the app's own gid; `ContextImpl` chmods them),
so a private directory under `files/` would still be refused for the
group-write bit: the embedded runtime therefore keeps its private
directories directly under the boundary, created by the runtime at
`0700`, never under those platform directories. Chmodding `files/` was
rejected (the platform may restore it); accepting group-write for the
effective gid was rejected (shared-uid packages hold the same gid, and
it would need its own justification). The `config.yaml` directory walk
takes the same boundary.

**Alternatives rejected.** Accepting uid 1000 (`system`) as a trusted
owner beside root: it would also need the group-write bit accepted for
gid 1000 and a table of modes OEMs may vary — the boundary says the same
without enumerating the platform. Leaving the rule and refusing on
Android until a measured exception exists: a rule the platform cannot
meet is a rule nobody runs, and the first Android package (plan §20) would be
built against it.

**Consequences.** No code change on a Linux host; the parameter in
`profile-config`'s resolve functions and the daemon's `/` are
p2p-network-dev's, landing with the Android binding's first package
(plan §20, Stage 17), not before — nothing ships refused today. The store side is
rust-ui-dev's. The refusal text names the boundary when the component
that broke the rule is the boundary itself. The threat model's residual
for the ancestor-swap row names the Android trust: what sits above the
boundary is the platform's to protect.

**Propagation.** Security implications in the body; this note; the log
row; the digest entry; configuration.md's private-directory paragraph
(the Android sentence); threat-model.md's ancestor-swap row (the
boundary in the control column, Android in the residual); IDENTITY.md's
at-rest sentence names the boundary.

### Amendment 2026-10-08 — A group of one is the owner's own

**Trigger.** p2p-network-dev-01's observation (GZCoord 01a11c0b): the
ancestor rule of the same morning refuses any group-writable ancestor
whoever is in the group. Under umask 002 — the user-private-group scheme, documented and
supported, whichever release's login applies it — directories a
user creates are `0775` in their own private group, of which they are
the only member, `~/.config` and `~/.local/state` among them; the
daemon, `transportctl` and the human client would refuse such a
profile, naming `~/.local/state` as "group- or other-writable and not
sticky", where no other account can write it. Measured: under umask
002, 56 tests at 36e6adc6 fail on tempfile's `0775` directories, 50
naming the rule; under 022 they pass. This host's shells run 022.

**Decision.** A group-write bit — on an ancestor, on a traversed link's
directory, or on `config.yaml` itself (one predicate for both clauses,
so they cannot diverge) — is accepted when the group is the PRIVATE GROUP OF THE DAEMON'S
EFFECTIVE USER — never of the directory's owner, since a root-owned
`0775` directory in group `root` would otherwise pass while accounts
with primary gid 0 exist: the group's name equals that user's name, both
read through the name service (the `getpwuid_r` / `getgrgid_r` family, NSS-
backed, through nix's `user` feature in safe Rust — never `/etc/group`
read by hand, which misses NSS sources), the group's member list is
empty, and the directory or file carries no extended access ACL: with a
POSIX ACL present the group bits are the ACL mask, not the owning
group's grant, and a named entry granting another account write hides
behind a group-write bit, so the presence of the `system.posix_acl_access`
attribute refuses the bit (the retired reviewer's thread on #231, judged
real). POSIX access ACLs are what is inspected — `ENODATA` and `ENOTSUP`
both read as no ACL, so a filesystem without ACL support is not refused
for that — and a network filesystem's own ACLs (NFSv4) are outside the
local-account threat, as the threat row says of network filesystems. A read that cannot be completed, or that answers no such user or
group, REFUSES, with the detail "group-writable; whether group <gid> is
the daemon user's private group could not be read". Other-write stays
refused unless sticky; the private directory itself stays owner-only
(`0700`, no bit for anyone): the predicate is about what others may
write above and around it. The rule's premise — no account but root and
ours can rename the directory — holds for a group of one with two
residuals the rule accepts as root's acts (the review of this note):
an account that holds the gid some other way root grants it — a
PRIMARY group set by `useradd -g`, which `gr_mem` never lists and no
name service enumerates reliably (sssd without enumeration), a group
password in `gshadow`, membership a later NSS source grants — so the
predicate does not try to see it; and a name service that answers falsely, trusted as root is.

**Alternatives rejected.** Leaving the rule and making the tests
independent of the umask: a supported setup could not start InterWeave,
and the refusal read as an attack where there was none. The primary-gid
test alone ("gid equals the owner's primary gid"): Debian's traditional
scheme gives every account primary gid 100 `users`, so a directory
every user can write would pass. Numeric gid = uid: `useradd` falls
back to the next free gid when the uid is taken, so the name equality
is the user-private-group convention and the thing to test.

**Consequences.** Code: p2p-network-dev's, on #228 or the next batch —
the predicate in `judge_ancestor`, `judge_link`'s directory and
`open_guarded_as`, a fake name service for the unit tests (a `0775`
directory in our private group accepted; a shared group refused naming
it; gid = uid with differing names refused; a name-service miss refused
as unreadable; a root-owned `0775` ancestor in group `root` refused,
naming the group — the test that pins the daemon's-user scoping against
a directory-owner reading; a `0775` directory in our private group with
an access ACL granting another account write refused, naming the ACL), the real lookups tested against `id -un` /
`id -gn` and a gid with no entry. The test suites set their tempdir modes explicitly
(a test that depends on the host's umask is fragile under either
ruling). Until the code lands, a user-private-group account under umask
002 is refused on main: a defect at writing, affecting no host here.

**Propagation.** The rule clause at every site (body, log row, digest,
IDENTITY.md, the threat row, configuration.md) and the `config.yaml`
file clause carry the predicate by reference; the predicate is stated
once, in Security implications.
