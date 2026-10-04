# human-desktop

The first-party desktop human client (plan §18). It is a thin composition root: the root's logic is `crates/human/app-core`. This crate holds:
- the command line;
- the profile's paths and store;
- the facade's thread over the desktop IPC binding;
- the window's side.

It uses a data connection for messaging and a separate admin connection for settings (ADR-0040). It shares the profile's daemon with Claude. Closing it releases its endpoint lease, and never stops the daemon.

**Current status:** active workspace member since Stage 15 batch 2. The binary runs every start-up step up to the window, then stops: this build has no windowing backend, which batch 4 brings. The window's side is tested headless, with the real view on Slint's testing backend over the fake network.

## Start-up, in order

1. **`--profile <name>`**, required, as the daemon's is. The endpoint is the profile's entry that allows the `human-client` kind, and the channels are the profile's `channels.desired`. Both are configuration, never chosen in the window (architect-cto's Q8 ruling).
2. **Fonts.** `ui_slint::platform_check()`: without fontconfig no text can be shown, so the app stops with a message.
3. **Paths.** The profile's paths, refused when two of its directories coincide or nest.
4. **Single instance.** The lock in `<state>/human/`: two windows on one store would fight over the endpoint lease.
5. **Store.** `<state>/human/human.sqlite`. A store that cannot be used is classified by what a person can do:
   - recovery (corrupt, newer or a failed migration);
   - not private;
   - unavailable now.

   The app never renames, moves or deletes the file, and never touches the identity (Q4 ruling, STATE.md).
6. **Window.** Batch 4.

Each refusal exits with a sysexits(3) code (`run.rs`) and a message that carries no message content (RETENTION.md §8).

## Running

- **The facade's own thread.** The facade side runs on a thread of its own with a one-worker runtime. A session whose events go undrained loses its lease, so the facade turns every 100 ms whatever the window does. A store call that blocks that thread cannot starve the IPC reader's keepalive pings.
- **The window's side.** It is `App::pump`. It applies what the facade sent, renders, takes the view's events until its queue is empty, and hands the commands over.
- **No daemon.** Asked each second through the profile lock (`ProfileLock::is_held`). The view says no transport runs for this profile in place of "reconnecting", and starting it is the operator's (Q9 ruling). Two limits:
  - a held lock means a daemon process exists, not that its sockets are ready;
  - an answer the lock cannot give is never read as "no daemon".
- **Links.** Opened with `xdg-open`: one argument, no shell, and the link is never logged.

## What it does not prove

- A window, its event loop and platform focus: batch 4.
- The required desktop end-to-end cases of §18: batches 5 to 7.
- Windows and macOS: carried (§18).
- The single-instance lock's "unsupported platform" exit (EX_UNAVAILABLE): on Linux the lock reaches it only if `/proc/self/status` cannot be read, which a test cannot arrange, so the arm is untested.
