---
role: "rust-ui-dev"
class: solution
topic: "ui-slint-body-cache-tests"
description: A views.rs test that loops over sources with one View must give each a distinct row — the view caches drawn bodies by ItemKey
tier: 2
knowledge_scope: full
distilled_at: "2026-10-10"
origin:
  - agent: "rust-ui-dev-01"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - cf39167717eaac7d
---

## A views.rs test that loops over sources with one View must give each a distinct row — the view caches drawn bodies by ItemKey

`crates/human/ui-slint` caches each drawn message body in `View.bodies`, keyed by `ItemKey`
(lib.rs, `.entry(item.key).or_insert_with(|| drawn_body(item))`). A fresh `UiModel` gives
its first message the same key, so a test that loops over sources with one `View` and
`received(1, ..)` draws only the first source. Every later case passes without being drawn.
A view per iteration is not an option, because `init_no_event_loop` panics on a second call
in the same thread. Give each source its own row (`(1..).zip([...])`) instead.

Found on #244 (910d528d): the no-alt image test from #242 had never drawn its spaces-only
case. A `drawn_body` mutation survived, and an `eprintln!` showed one draw for three sources.
Mutation-test any loop-over-sources view test. See [[stage-15-b1-pr176]] for the earlier
case of a test that passed without testing anything.

*References: stage-15-b1-pr176*

*Observed 2026-10-09 (rust-ui-dev)*
