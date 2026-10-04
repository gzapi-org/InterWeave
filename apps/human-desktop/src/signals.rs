// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! SIGTERM and SIGINT end the window as closing it does: the session is
//! closed and the lease released, never the daemon. A second one while
//! that close is still running ends the process at once, as a second
//! Ctrl-C does anywhere: the daemon frees the lease when the socket
//! closes.

use std::sync::mpsc;

use tokio::signal::unix::{Signal, SignalKind, signal};

/// The exit status of a second signal: 128 plus its number, as a shell
/// reports a process a signal ended.
const fn forced(kind: SignalKind) -> i32 {
    128 + kind.as_raw_value()
}

/// The next delivery of `watched`, or never when it is not watched.
async fn next(watched: &mut Option<(SignalKind, Signal)>) -> SignalKind {
    match watched {
        Some((kind, stream)) => {
            stream.recv().await;
            *kind
        }
        None => std::future::pending().await,
    }
}

/// Install each of `kinds` with `install`: the streams that took, in
/// order, and a line naming each that did not. One failing never undoes
/// another -- a handler once installed stays, so it must be answered.
fn install_each<S>(
    kinds: [(SignalKind, &str); 2],
    mut install: impl FnMut(SignalKind) -> std::io::Result<S>,
) -> ([Option<(SignalKind, S)>; 2], Vec<String>) {
    let mut unwatched = Vec::new();
    let watched = kinds.map(|(kind, name)| match install(kind) {
        Ok(stream) => Some((kind, stream)),
        Err(e) => {
            unwatched.push(format!("{name}: {e}"));
            None
        }
    });
    (watched, unwatched)
}

/// What the caller is told: an error only when nothing is watched, so a
/// thread holding even one handler stays to answer it.
fn report(watching: usize, unwatched: Vec<String>) -> Result<Vec<String>, String> {
    if watching == 0 {
        Err(unwatched.join("; "))
    } else {
        Ok(unwatched)
    }
}

/// Call `then` once, from a thread of its own, when SIGTERM or SIGINT
/// arrives; end the process on the next one. Returns once the handlers
/// are installed, so a signal sent after it -- once the lease is asked
/// for, say -- is caught rather than given its default action. Each
/// signal is installed on its own: one that cannot be watched is
/// reported and the other still works, and an installed handler is
/// always answered. The thread installs them itself, so a thread that
/// could not start leaves the defaults.
pub(crate) fn on_terminate(then: impl FnOnce() + Send + 'static) {
    let (installed, wait) = mpsc::sync_channel::<Result<Vec<String>, String>>(1);
    let spawned = std::thread::Builder::new()
        .name("human-signals".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(e) => {
                    let _ = installed.send(Err(e.to_string()));
                    return;
                }
            };
            runtime.block_on(async {
                let ([mut term, mut int], unwatched) = install_each(
                    [
                        (SignalKind::terminate(), "SIGTERM"),
                        (SignalKind::interrupt(), "SIGINT"),
                    ],
                    signal,
                );
                let watching = usize::from(term.is_some()) + usize::from(int.is_some());
                let reported = report(watching, unwatched);
                let nothing = reported.is_err();
                let _ = installed.send(reported);
                if nothing {
                    return;
                }
                tokio::select! {
                    _ = next(&mut term) => {}
                    _ = next(&mut int) => {}
                }
                then();
                let second = tokio::select! {
                    kind = next(&mut term) => kind,
                    kind = next(&mut int) => kind,
                };
                std::process::exit(forced(second));
            });
        });
    let outcome = match spawned {
        Ok(_) => wait
            .recv()
            .unwrap_or_else(|_| Err("the watching thread ended".to_owned())),
        Err(e) => Err(e.to_string()),
    };
    match outcome {
        Ok(unwatched) if unwatched.is_empty() => {}
        Ok(unwatched) => eprintln!(
            "human-desktop: not watched, close the window to stop: {}",
            unwatched.join("; ")
        ),
        Err(e) => {
            eprintln!("human-desktop: signals cannot be watched, close the window to stop: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::signal::unix::SignalKind;

    use super::{forced, install_each, report};

    const KINDS: [(SignalKind, &str); 2] = [
        (SignalKind::terminate(), "SIGTERM"),
        (SignalKind::interrupt(), "SIGINT"),
    ];

    #[test]
    fn a_signal_that_cannot_be_installed_leaves_the_other_watched_and_answered() {
        let ([term, int], unwatched) = install_each(KINDS, |kind| {
            if kind == SignalKind::interrupt() {
                Err(std::io::Error::other("refused"))
            } else {
                Ok(kind)
            }
        });
        assert!(term.is_some(), "SIGTERM is still watched");
        assert!(int.is_none());
        assert_eq!(unwatched, ["SIGINT: refused"]);
        assert_eq!(
            report(1, unwatched),
            Ok(vec!["SIGINT: refused".to_owned()]),
            "one watched: the thread stays to answer it, and the other is named"
        );
    }

    #[test]
    fn nothing_watched_is_an_error_and_the_defaults_stay() {
        let ([term, int], unwatched) =
            install_each(KINDS, |_| Err::<(), _>(std::io::Error::other("refused")));
        assert!(term.is_none() && int.is_none());
        assert_eq!(
            report(0, unwatched),
            Err("SIGTERM: refused; SIGINT: refused".to_owned())
        );
    }

    #[test]
    fn a_forced_exit_reports_the_signal_as_a_shell_does() {
        assert_eq!(forced(SignalKind::interrupt()), 130);
        assert_eq!(forced(SignalKind::terminate()), 143);
    }
}
