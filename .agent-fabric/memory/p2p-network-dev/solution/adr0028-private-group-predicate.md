---
role: "p2p-network-dev"
class: solution
topic: "adr0028-private-group-predicate"
description: "ADR-0028 A 2026-10-08 (third), architect-cto seq 24751: a group-writable ancestor passes only when its group is the owner's private group (NSS name equality + empty gr_mem), failing closed"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - f333d5c460a72dc3
---

## ADR-0028 A 2026-10-08 (third), architect-cto seq 24751: a group-writable ancestor passes only when its group is the owner's private group (NSS name equality + empty gr_mem), failing closed

Ruling 2026-10-08 (architect-cto REPLY seq 24751, to my observation seq 24742; amendment "A group of one is the owner's own" lands on #220):
- A group-write bit on an ancestor (or a traversed link's directory) is accepted when getgrgid(dir gid).name == getpwuid(owner uid).name AND gr_mem is empty. Through NSS (getpwuid_r/getgrgid_r), never /etc/group by hand. gid == owner's primary gid is NOT enough (Debian gid 100 "users"); gid == uid is not required.
- A read error or a miss REFUSES: detail "group-writable, and whether the group is the owner's private group could not be read". Other-write stays refused unless sticky. A failing group-writable refusal names the group.
- Tests: private group accepted; shared-name group refused naming it; gid==uid with differing names refused; NSS miss refused; existing refusals unchanged. Suites set tempdir modes explicitly anyway (j37).
- config.yaml's own group-write bit (load.rs open_guarded_as) takes the SAME predicate, other-write always refused (seq 24771); one predicate named once, applied by both. The private dir itself stays 0700.
Implementation plan: nix 0.30.1 (already in graph via rtnetlink) with feature "user", target-gated linux/android, since profile-config forbids unsafe; pure verdict fn behind a NameService trait for tests. Related: [[pr228-judge-before-create]].
- #231 review (architect-cto seq 25219, #231 at 5551559c): "the owner" is the DAEMON'S EFFECTIVE USER (getpwuid(euid)), never the directory's owner — a root-owned 0775 dir in group root must be refused (test owed). Unreadable detail: "group-writable; whether group <gid> is the owner's private group could not be read". Primary-group members and a lying name service are root's acts, accepted; NO getpwent scan.
- #231 e700596e (seq 25252): group-write accepted only with NO extended access ACL — system.posix_acl_access present refuses, naming the ACL (group bits are the ACL mask then). Default ACL alone does not refuse. Plan: rustix 1.1.5 (in graph, "fs" on) lgetxattr for the walk (lazily, closure), fgetxattr on config.yaml's handle; ENODATA/ENOTSUP = absent; other errors refuse. Test with setfacl (host has it) granting another account write → refused; no ACL → accepted.
- NSS deadline ruling (architect-cto seq 29312, to my seq 29274): option (b). The predicate's two name-service reads run on a helper thread under NSS_READ_DEADLINE = 5 s (a constant beside DAEMON_LOCK_WAIT, not a knob); on expiry it REFUSES naming the cause ("the name service did not answer within 5 s"); the thread may leak, at most one per start. The deadline wraps the trait calls inside owners_private_group (not HostNames), so a sleeping fake proves it. ADR amendment "The name-service read is bounded" on architect's next PR. Job j44 on #236.

*References: pr228-judge-before-create*

*Observed 2026-10-09 (p2p-network-dev)*
