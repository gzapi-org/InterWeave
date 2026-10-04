# human-desktop

The first-party desktop human client (plan §18). It is a thin composition root: the root's logic is `crates/human/app-core`. This crate holds:
- the command line;
- the profile's paths and store;
- the facade's thread over the desktop IPC binding;
- the window's side.

It uses a data connection for messaging and a separate admin connection for settings (ADR-0040). It shares the profile's daemon with Claude. Closing it releases its endpoint lease, and never stops the daemon.

**Current status:** active workspace member since Stage 15 batch 2. Since batch 4 it opens its window: winit with the software renderer and the platform accessibility adapter (`ui-slint`'s `desktop` feature). The window's side is tested headless, with the real view on Slint's testing backend over the fake network, and the shipped binary against a real daemon on a display (`tests/desktop-e2e/tests/human_lifecycle.rs`).

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
6. **Window.** It opens on the display and runs until the person closes it or SIGTERM/SIGINT arrives; then the session is closed, releasing the lease, and the app exits 0.

Each refusal exits with a sysexits(3) code (`run.rs`) and a message that carries no message content (RETENTION.md §8).

## Running

- **The facade's own thread.** The facade side runs on a thread of its own with a one-worker runtime. A session whose events go undrained loses its lease, so the facade turns every 100 ms whatever the window does. A store call that blocks that thread cannot starve the IPC reader's keepalive pings.
- **The window's side.** It is `App::pump`. It applies what the facade sent, renders, takes the view's events until its queue is empty, and hands the commands over.
- **No daemon.** Asked each second through the profile lock (`ProfileLock::is_held`). The view says no transport runs for this profile in place of "reconnecting", and starting it is the operator's (Q9 ruling). Two limits:
  - a held lock means a daemon process exists, not that its sockets are ready;
  - an answer the lock cannot give is never read as "no daemon".
- **Links.** Opened with `xdg-open`: one argument, no shell, and the link is never logged.

## Measured

The CPU cost of software rendering is measured here, as plan §18 (14) asks. The window was shown with no daemon (the no-daemon notice on screen), on a debug build, on an X display. Over 30 s idle the process used 0.37% of one core (`/proc/<pid>/stat`, utime+stime) and held 47 MB resident; that includes the facade's 100 ms turn and the 1 s daemon probe. Not measured yet: the cost while redrawing a long conversation or a resize, and a release build.

## What it does not prove

- That window focus, read from winit, gates a read on a real display: the view's half is tested, and the person-input cases are batches 5 to 7.
- The Slint attribution: the badge on the download page at Stage 19 (architect-cto's ruling), not a screen here.
- A visible scrollbar on a long conversation or conversation list: both scroll by wheel and touch, and keyboard focus brings a row into view.
- The required desktop end-to-end cases of §18: batches 5 to 7.
- Windows and macOS: carried (§18).
- The single-instance lock's "unsupported platform" exit (EX_UNAVAILABLE): on Linux the lock reaches it only if `/proc/self/status` cannot be read, which a test cannot arrange, so the arm is untested.
