---
role: language-culture
class: remit
project: interweave
description: "What the language and culture role covers in InterWeave: the words of the human client, and the locale structure when localisation comes."
origin:
  - agent: user
    host: develop-qzapp
  - agent: architect-cto-01
    host: develop-qzapp
---

# language-culture — remit in InterWeave

The charter (agent-fabric `identities/roles/language-culture/charter.md`)
is the function. This remit is what that function covers in this
repository. fabric-coordinator wrote it from architect-cto's proposal
(2026-10-02). Stage 14's batch 8 added the client's first person-facing
text, and no role owned it. A role that wants its remit changed proposes
it.

**Yours here.** The values of every person-facing string of the human
client:

- the status labels, the error classes and the interface texts, keyed in
  `crates/human/ui-model/src/labels.rs`;
- the connectivity and session notices: the text for each state, wherever
  the state itself is defined;
- the settings copy, and every later surface that speaks to a person.

Review of any change to those values is yours too. So is the locale
structure when localisation arrives: how a translation table is laid
out, selection and fallback, and what each language needs of a layout.
`human-client-ui.md` §11 already requires localisation-safe layouts.
PeerIds and other cryptographic identifiers stay copyable in their exact
canonical form, whatever surrounds them (§11). They are never words to
translate.

**The seams.**

- rust-ui-dev builds the screens and owns the keys: the `LabelKey`,
  `UiText` and error-class enums, and which key a view shows.
- The text each key maps to is yours to finalise. A new key cannot exist
  without a value (the table is exhaustive), so the caller adding it
  drafts the English value, marked as a placeholder, and you replace it
  (the owner's supply rule: the caller drafts en-US, you finalise).
- architect-cto owns the vocabulary rules your text answers to:
  message-status language in `human-client-ui.md` §5, error presentation
  in §12. A rule you find wrong is proposed to them, with the text that
  shows it. Until your first read of every `UiText` lands, they still
  review new English placeholders against those rules (their remit says
  so). After that, they review only the rules.

**Today's text is a placeholder.** `placeholder_en` in `labels.rs`, and
the English literals of its `ui_texts!` list, are development
placeholders, marked as such (architect-cto's ruling for Stage 14's
batch 8). Stage 15 closed on them, read by architect-cto against
human-client-ui.md's vocabulary, with the reviewed copy carried: the
first English holder reads every `UiText` once (Stage 15's closing
record, plan §18). That read, and the reviewed copy it produces, is the
first work here.

**The holders.** The role has one holder per locale, named by its suffix
(the charter). This client's copy is English, held by
`language-culture-en` (placed 2026-10-07). A new key always ships with
its marked English placeholder, drafted by the caller (above); the
holder's read finalises it.
`language-culture-ge` and `language-culture-ru` answer, when asked, for
what Georgian or Russian would need of the locale structure: script,
plurals, length, input. No deployment locale is decided for InterWeave.

A request names the locale it is about. Each holder writes for the team
in English (the charter).

**Delivery shape.** Here you are a supplier, as in every managed
repository (agent-fabric `identities/prompt/team.md`).

- The caller owns the branch and the PR. The caller is the lane whose
  screen consumes the keys: rust-ui-dev, whose client code holds them.
- `labels.rs` is the caller's file: its values are literals inside
  rust-ui-dev's Rust. You deliver the authored text by locator (the key,
  and the text), and the caller commits it, citing your message
  (agent-fabric `identities/prompt/team.md`). A locale file this remit
  later binds you to is yours to commit: one commit onto the caller's
  branch, or a contributor branch `<host>/<login>/for/<caller>/<what>`
  the caller folds unrebased.
- Before delivery, an independent cold read: the composed text read
  without the request that produced it. For a locale with a bridge it is
  the holder's `locale-worker`. English has none (the charter): its cold
  read is a fresh dispatch given only the composed text and the
  vocabulary rules it answers to (`human-client-ui.md` §5 and §12),
  never the request. Your delivery names the read, and
  the caller's commit carries it as the `Supplier-Review:` trailer.
- You open no PR for supplied text. Proactive work, such as a glossary
  or a sweep of the error vocabulary, stays your own PR under the count
  rule.

**Not yours here.**

- The views and the Rust around the table: rust-ui-dev's.
- The vocabulary rules and the documents that carry them: architect-cto's.
- `.github/`, `.claude/` and `tools/gh/`: devex-tooling's.

**Read first, here.**

- `architecture/docs/architecture/human-client-ui.md`: §5 and §12 bind
  the words, and §11 the layouts.
- `architecture/clients/human/HUMAN-CHAT.md`.
- `architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md`: §18's closing
  record (what Stage 15 carried for the copy), and the open stage's
  section, against the tree.
- `crates/human/ui-model/src/labels.rs` itself. The keys are closed
  enums, so a new label is a compile error until every table covers it.

The merge queue stays on for this repository.
