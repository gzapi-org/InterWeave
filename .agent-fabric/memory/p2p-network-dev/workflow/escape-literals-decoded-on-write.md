---
role: "p2p-network-dev"
class: workflow
topic: "escape-literals-decoded-on-write"
description: writing a test that needs a literal JSON \uXXXX escape (surrogate pair) in source — the write path decodes it into the raw character
tier: 1
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "p2p-network-dev-02"
    host: "develop-qzapp"
    project: interweave
    working_copy: InterWeave
derived_from:
  - d72a0292be1f7d9e
---

## writing a test that needs a literal JSON \uXXXX escape (surrogate pair) in source — the write path decodes it into the raw character

On 2026-10-08 a test meant to feed the JSON reader the ESCAPED surrogate pair (backslash-u d83d
backslash-u de00) landed on disk as the raw emoji twice — once written through a Python heredoc
(Python string escapes), once through the Edit tool itself — and a pair like backslash-u0041 landed as
"A". The test passed while the pairing arithmetic never ran; the blind review caught it (F5 on
InterWeave j3), and a mutation (`0x10000` constant) survived until the escapes were BUILT at runtime
from `char::from(0x5c)` + 'u' + hex digits, with an `is_ascii()` assertion that the escape stayed one.
**Why:** an unexercised branch looked tested; only a mutation shows it.
**How to apply:** never write a literal backslash-u escape into source through any tool; construct it
at runtime and assert the constructed text is ASCII. After writing such a test, `cat -A` the line or
mutate the branch it targets. See [[independent-codecs-lessons]].

*References: independent-codecs-lessons*

*Observed 2026-10-08 (p2p-network-dev)*
