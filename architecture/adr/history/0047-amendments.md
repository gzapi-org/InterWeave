# ADR-0047 — amendment history

### Amendment 2026-08-17 — HumanChat media type advanced to v=2

ADR-0050 superseded the HumanChatV1 envelope before any implementation existed, making `application/vnd.interweave-human-chat+json;v=2` (and its `;ce=br` compressed form) the implementation target. The canonical application/local identifier list read `application/vnd.interweave-human-chat+json;v=1`; a reader taking that list as current would implement the wrong version, so the entry is updated in place.

This amendment changes nothing this ADR decided: the `vnd.interweave-` vendor prefix, the display/machine namespace split, and every hash-participating identifier are untouched — the version parameter belongs to the HumanChat application protocol, and no frozen golden involves the HumanChat media type as anything but arbitrary sample bytes (the `fixtures/direct-v2/` vector that uses the v1 string as test input stays byte-identical for exactly that reason).
