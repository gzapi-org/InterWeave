---
role: "p2p-network-dev"
class: workflow
topic: "prefer-python-over-shell"
description: "The owner asked (2026-09-30): use python instead of shell when possible -- for file edits, text processing and scripted checks"
tier: 1
knowledge_scope: full
distilled_at: "2026-10-05"
origin:
  - agent: "p2p-network-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 0356af506ec30867
---

## The owner asked (2026-09-30): use python instead of shell when possible -- for file edits, text processing and scripted checks

The owner, 2026-09-30, mid-B3: "memorize, use python instead of shell when possible".

**Why:** said after a session of sed one-liners, some of which missed text rustfmt had reflowed or
matched more lines than intended; python edits with an exact `old` string asserted to occur once fail
loudly instead (see [[a-cleanup-regex-spans-what-sits-before-its-anchor]]).

**How to apply:** file edits, text extraction, counting and scripted mutations go through a python
heredoc (exact replace, `assert s.count(old) == 1`), not sed/awk/grep pipelines. Shell stays for
what is a command by nature: git, cargo, gh, the fabric tools, and running the repository's own
scripts.

*References: a-cleanup-regex-spans-what-sits-before-its-anchor*

*Observed 2026-09-30 (p2p-network-dev)*
