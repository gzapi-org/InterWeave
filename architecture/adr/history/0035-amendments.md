# ADR-0035 — amendment history

### Amendment 2026-09-09 — Selection precedence binds relay only

The Decision's infrastructure-selection paragraph said, of relays and probe servers together:

> Identify-learned relay/probe candidates require explicit opt-in (`use_authorized_identify_* = true`), and static candidates have selection precedence until they cannot meet the configured target.

The opt-in half stands for both. The **selection-precedence** half is amended to bind relay only, because for AutoNAT it cannot be implemented and an unimplementable clause in an accepted decision is worse than none — the next reader takes it for a rule something enforces.

The standard-v1 AutoNAT v2 client chooses its probe server itself, uniformly at random among the connections this profile DIALLED whose remote advertised the dial-request protocol, and its entire configuration surface is a candidate ceiling and a probe interval. There is no hook to rank servers, prefer a source, or veto a pick. Nor can the outbound dial gate stand in for one: an AutoNAT probe is a request over an already-open connection, so the client emits no dial for the gate to judge. What is left that a profile can decide is **which servers it connects to at all**, and that is what static configuration and `use_authorized_identify_servers` govern for AutoNAT under this amendment.

`transport/libp2p/AUTONAT.md` §3 was amended the same day and carries the measurement, the consequence a reader gives up (a data-plane peer this profile dialled that also advertises the server protocol becomes an eligible observer), and a related gap in §6 that this amendment does not settle.

This is a narrowing of scope rather than a reversal: the decision that configured infrastructure is preferred is unchanged, and unchanged in full for relay. A reader who followed the old sentence for relay is still right; one who followed it for AutoNAT was owed a rule that no shipped code could keep.
