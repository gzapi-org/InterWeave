// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! `claude-channel --profile <name> --endpoint <id> --delivery push|pull`:
//! started by an MCP host over stdio (`plugin/LIFECYCLE.md` §Startup) --
//! Claude Code with `push`, any other host with `pull`.
//!
//! The profile name, the endpoint and the delivery mode are the host's
//! non-secret configuration (the plugin's `.mcp.json` arguments). From the name it derives the
//! daemon's data socket and reads the profile's desired channels for
//! `status`; it connects to nothing but the data socket.
//!
//! **The first signal is final.** Claude Code stops the bridge with
//! SIGINT, then SIGTERM about 100 ms later, then SIGKILL (SPIKE-001
//! fact 18). The bridge exits on the first, doing nothing it could not
//! finish: its lease and joins end with its IPC connection, which is the
//! daemon's act.

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    unix::main()
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!(
        "claude-channel: the transport daemon's sockets are Unix sockets; this platform has none"
    );
    std::process::ExitCode::FAILURE
}

#[cfg(unix)]
mod unix {
    use std::process::ExitCode;

    use interweave_claude_channel::serve::{CLIENT_KIND, Config, Env, serve};
    use interweave_claude_channel_core::Delivery;
    use interweave_ipc_client::{IpcBinding, SocketPaths};
    use interweave_profile_config::{LoadError, ProfileConfig, ProfilePaths, XdgRoots};
    use interweave_transport_api::EndpointId;
    use tokio::io::BufReader;

    const USAGE: &str =
        "usage: claude-channel --profile <name> --endpoint <id> --delivery push|pull";

    pub(crate) fn main() -> ExitCode {
        match run() {
            Ok(()) => ExitCode::SUCCESS,
            Err(refusal) => {
                eprintln!("claude-channel: {refusal}");
                ExitCode::FAILURE
            }
        }
    }

    fn run() -> Result<(), String> {
        let (profile, endpoint, delivery) = arguments(std::env::args().skip(1))?;
        let roots = XdgRoots::from_env().map_err(|e| format!("the XDG directories: {e}"))?;
        let paths = ProfilePaths::resolve(&profile, &roots)
            .map_err(|e| format!("the profile's paths: {e}"))?;
        let sockets = SocketPaths {
            data: paths
                .data_socket()
                .map_err(|e| format!("the data socket: {e}"))?,
            // Required by the binding's shape; the bridge is generic over
            // the data side alone and never opens it.
            admin: paths
                .admin_socket()
                .map_err(|e| format!("the admin socket path: {e}"))?,
        };
        let desired_channels = ProfileConfig::load(&paths)
            .map(|config| config.channels.desired)
            .map_err(|e| load_error_class(&e).to_owned());
        let config = Config {
            endpoint,
            desired_channels,
            delivery,
        };
        let env = Env {
            now_ms: Box::new(wall_ms),
            entropy: Box::new(rand::random),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .map_err(|e| format!("the runtime: {e}"))?;
        let ended = runtime.block_on(async {
            let binding = IpcBinding::new(sockets, CLIENT_KIND);
            let served = serve(
                binding,
                config,
                env,
                BufReader::new(tokio::io::stdin()),
                tokio::io::stdout(),
            );
            tokio::select! {
                served = served => served.map_err(|e| format!("stdio: {e}")),
                () = first_signal() => Ok(()),
            }
        });
        // NOT a drop: dropping the runtime waits for its blocking pool,
        // and stdin's read there cannot be cancelled -- with the host never
        // closing stdin, the first signal would not end the process
        // (`the_first_sigint_ends_the_process_with_stdin_held_open`).
        runtime.shutdown_background();
        ended
    }

    /// `--profile <name> --endpoint <id> --delivery push|pull`, in any
    /// order, nothing else. `--delivery` is REQUIRED, with no default: a
    /// host does not declare whether it takes Claude Code's channel push,
    /// so a missing flag never puts the one harness that does into pull
    /// (architect-cto's ruling, relay seq 18691).
    fn arguments(
        mut args: impl Iterator<Item = String>,
    ) -> Result<(String, EndpointId, Delivery), String> {
        let (mut profile, mut endpoint, mut delivery) = (None, None, None);
        while let Some(flag) = args.next() {
            let value = args.next().ok_or_else(|| USAGE.to_owned())?;
            match flag.as_str() {
                "--profile" if profile.is_none() => profile = Some(value),
                "--endpoint" if endpoint.is_none() => {
                    endpoint =
                        Some(EndpointId::parse(value).map_err(|e| format!("--endpoint: {e}"))?);
                }
                "--delivery" if delivery.is_none() => {
                    delivery = Some(
                        Delivery::parse(&value)
                            .ok_or_else(|| format!("--delivery is push or pull, not {value:?}"))?,
                    );
                }
                _ => return Err(USAGE.to_owned()),
            }
        }
        match (profile, endpoint, delivery) {
            (Some(profile), Some(endpoint), Some(delivery)) => Ok((profile, endpoint, delivery)),
            _ => Err(USAGE.to_owned()),
        }
    }

    /// The class of a profile load's failure, which `status` reports in
    /// place of the desired channels.
    fn load_error_class(error: &LoadError) -> &'static str {
        match error {
            LoadError::Read(_) => "Read",
            LoadError::TooLarge { .. } => "TooLarge",
            LoadError::Parse(_) => "Parse",
            LoadError::NameMismatch { .. } => "NameMismatch",
            LoadError::Invalid(_) => "Invalid",
            LoadError::KeyFileInHumanDir { .. } => "KeyFileInHumanDir",
            LoadError::KeyFileUnresolved { .. } => "KeyFileUnresolved",
        }
    }

    fn wall_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    }

    async fn first_signal() {
        use tokio::signal::unix::{SignalKind, signal};
        let (Ok(mut interrupt), Ok(mut terminate)) = (
            signal(SignalKind::interrupt()),
            signal(SignalKind::terminate()),
        ) else {
            // No handler: the default action still ends the process.
            return std::future::pending().await;
        };
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn args(list: &[&str]) -> Result<(String, EndpointId, Delivery), String> {
            arguments(list.iter().map(|s| (*s).to_owned()))
        }

        /// A profile, an endpoint and a delivery mode, in any order and
        /// nothing else; `--delivery` is required, with no default.
        #[test]
        fn the_arguments_are_a_profile_an_endpoint_and_a_delivery_mode() {
            let (profile, endpoint, delivery) = args(&[
                "--endpoint",
                "claude",
                "--delivery",
                "push",
                "--profile",
                "work",
            ])
            .expect("parsed");
            assert_eq!(profile, "work");
            assert_eq!(endpoint.as_str(), "claude");
            assert_eq!(delivery, Delivery::Push);
            let (.., pull) =
                args(&["--profile", "w", "--endpoint", "c", "--delivery", "pull"]).expect("parsed");
            assert_eq!(pull, Delivery::Pull);
            let missing =
                args(&["--profile", "work", "--endpoint", "claude"]).expect_err("refused");
            assert!(
                missing.contains("--delivery push|pull"),
                "names both: {missing}"
            );
            for bad in [
                &[][..],
                &["--profile", "work", "--delivery", "push"],
                &["--profile", "work", "--endpoint"],
                &[
                    "--profile",
                    "a",
                    "--profile",
                    "b",
                    "--endpoint",
                    "claude",
                    "--delivery",
                    "push",
                ],
                &[
                    "--profile",
                    "work",
                    "--endpoint",
                    "claude",
                    "--delivery",
                    "auto",
                ],
                &[
                    "--profile",
                    "w",
                    "--endpoint",
                    "c",
                    "--delivery",
                    "push",
                    "--delivery",
                    "pull",
                ],
                &[
                    "--profile",
                    "work",
                    "--endpoint",
                    "claude",
                    "--delivery",
                    "push",
                    "--admin",
                    "x",
                ],
                &["--profile", "work", "--endpoint", "", "--delivery", "push"],
            ] {
                assert!(args(bad).is_err(), "{bad:?}");
            }
        }
    }
}
