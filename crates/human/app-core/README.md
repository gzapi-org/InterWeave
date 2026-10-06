# app-core

The human client's headless root (Stage 15, plan §18; architect-cto's Q1 ruling, relay seq 11163). It is what a composition root wires between the transport facade (`crates/human/transport-client`) and a view, with no toolkit, no runtime and no platform code. The desktop app wires it at Stage 15, and Android reuses it at Stage 17.

**Current status:** active workspace member since Stage 15 batch 1. It is tested over the fake network (`tests/local-client-fake`) with a real store and a scripted view.

## The two sides

- **`FacadeSide`** owns the facade, and through it the store. Each `turn(now)` ticks the facade, hands over what it received and reports its events. `execute(command, now)` carries out a `Command` against the facade or the store. Every command ends with `Update::Done`, or `Update::Failed` with a `Failure` class (never content): the copy is gone, no such row, storage unavailable, refused, or corrupt.
- **`ModelSide`** owns the `UiModel` and a view behind `Surface`. `apply(update)` applies what the facade side did. Each `turn()` renders once, then takes the view's events until a take comes back empty: the drain `ui-slint`'s queue bound depends on. It returns the commands the person's intents ask for.

What crosses between them (`Command`, `Update`, `Listing`) is plain values, so the two sides can run on different threads. On the desktop the facade side runs on its own runtime, because a session whose events go undrained loses its lease.

## Rules it keeps

- **A model change follows the store.** `Read`, `Kept` and `Unkept` are applied to the model only after the store call succeeded.
- **One action at a time.** A command is not sent again while the same one is in flight: one send per conversation, one MarkRead per row, and so on. A render can raise MarkRead again before the first is answered, and a person can press Send twice. `Done` or `Failed` releases it. Issuing a send records the press (`UiModel::send_pressed`), so its answer leaves a composer edited since untouched.
- **Keep keeps the copy it names.** The facade side holds the content a read or an unkeep handed back, keyed by that row, so a Keep, including a re-keep after Unkeep within the session, keeps exactly that copy. At most `READ_COPY_CAP` copies are held. Past that the oldest goes, a Keep of that message fails with `CopyGone` and nothing changed, and the model stops offering Keep for it (`UiModel::copy_gone`).
- **Trust is the settings view's.** `ViewEvent::Trust` is applied to the model's trust settings like a draft edit, and only the intents it returns -- a read on opening, a change on confirmation -- become `Command::ReadTrust` and `Command::SetTrust`. The daemon's answer comes back as `Update::TrustRead` or `Update::TrustSet`; a trust failure is the settings' to say, so its command still ends `Done` (`tests/root.rs`: `a_trust_change_reaches_the_daemon_only_once_confirmed`).
- **A failure is reported, not swallowed.** Every `Failed` command, and an unread relist that could not be read, becomes a `Problem` the root takes with `take_problems()` to log and show. At most `PROBLEM_CAP` wait; past that the oldest go and are counted.
- **A link never reaches the facade.** `OpenLink` goes to the `Opener`, and only for an allowlisted scheme, checked again here.
- **The store is listed at start.** Pending, unread and kept rows are decoded the way the facade decodes on receipt, and given to the model before anything else. Unread rows are listed again when the facade reports unread content it could not hand over. A row that cannot be decoded stays in the store, is never shown, and is counted (`ModelSide::hidden_rows`) so the root can say so.

## What it does not prove

- A real daemon, a real window or a real platform: those are the desktop end-to-end suite's (`tests/desktop-e2e`).
- How a problem or a hidden row is shown to a person: the desktop app (batch 2) owns that notice.
