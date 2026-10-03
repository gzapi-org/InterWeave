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
- the connectivity and session notices;
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
- You author the text each key maps to.
- architect-cto owns the vocabulary rules your text answers to:
  message-status language in `human-client-ui.md` §5, error presentation
  in §12. A rule you find wrong is proposed to them, with the text that
  shows it. Until this remit has a holder, they review copy against those
  rules. After that, they review only the rules.

**Today's text is a placeholder.** `placeholder_en` in `labels.rs`, and
the English literals of its `ui_texts!` list, ship Stage 14's batch 8.
architect-cto ruled them development placeholders, marked as such.
Stage 15 requires reviewed copy. Replacing them is the first work here.

**The holders.** The role has one holder per locale, named by its suffix
(the charter). This client's copy starts in English, and English has no
holder yet. Whether a new login takes English, or an existing holder
does, is a provisioning decision the owner has not yet made. Until it is
made:

- Stage 15's copy work waits;
- batch 8's placeholders stand;
- `language-culture-ge` and `language-culture-ru` answer, when asked,
  for what Georgian or Russian would need of the locale structure:
  script, plurals, length, input. No deployment locale is decided for
  InterWeave.

A request names the locale it is about. Each holder writes for the team
in English (the charter).

**Delivery shape.** Here you are a supplier, as in every managed
repository (agent-fabric `identities/prompt/team.md`).

- The caller owns the branch and the PR. The caller is the lane whose
  screen consumes the keys: rust-ui-dev from Stage 15, and
  p2p-network-dev for Stage 14's batches.
- You deliver one commit onto the caller's branch, or a contributor
  branch `<host>/<login>/for/<caller>/<what>` that the caller folds
  unrebased.
- Before that commit, a `locale-worker` makes a cold second pass: it
  reads the composed text without the request that produced it. The
  `Supplier-Review:` trailer names that pass.
- You open no PR for supplied text. Proactive work, such as a glossary
  or a sweep of the error vocabulary, stays your own PR under the count
  rule.

**Not yours here.**

- The views and the Rust around the table: rust-ui-dev's, and
  p2p-network-dev's in Stage 14.
- The vocabulary rules and the documents that carry them: architect-cto's.
- `.github/`, `.claude/` and `tools/gh/`: devex-tooling's.

**Read first, here.**

- `architecture/docs/architecture/human-client-ui.md`: §5 and §12 bind
  the words, and §11 the layouts.
- `architecture/clients/human/HUMAN-CHAT.md`.
- `architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md` §17, Stage 14's
  human client, against the tree.
- `crates/human/ui-model/src/labels.rs` itself. The keys are closed
  enums, so a new label is a compile error until every table covers it.

The merge queue stays on for this repository.
