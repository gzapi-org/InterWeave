// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The shipped desktop client, as a process, for the human cases of this
//! suite (rust-ui-dev's, plan section 18; architect-cto's Q10 ruling).
//! `common/` stays the daemon's harness; this module only adds the app.

use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use interweave_ipc_client::IpcBinding;
use interweave_local_client_api::{
    AdminBinding as _, AdminCapability, AdminPort as _, LeaseRecord,
};

use crate::common::{Home, PATIENCE, human, workspace_binary};

/// The workspace's own `human-desktop`, beside this test's build.
pub(crate) fn app_binary() -> PathBuf {
    workspace_binary("human-desktop", "interweave-human-desktop")
}

/// The display a window opens on. Required, not skipped: a test that
/// passes because it could not open a window proves nothing. CI provides
/// one (devex-tooling's display for these cases).
pub(crate) fn display() -> String {
    std::env::var("DISPLAY").unwrap_or_else(|_| {
        panic!("DISPLAY is not set: the desktop client's window cases need a display")
    })
}

/// A running desktop client.
pub(crate) struct App {
    pub(crate) child: Child,
    log: PathBuf,
}

impl App {
    /// What the client wrote to stderr so far.
    pub(crate) fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// Send SIGTERM and wait for the exit.
    pub(crate) fn terminate(&mut self) -> ExitStatus {
        self.signal("TERM")
    }

    /// Send `name` (`TERM`, `INT`) and wait for the client to exit.
    pub(crate) fn signal(&mut self, name: &str) -> ExitStatus {
        self.send(name);
        self.exit_within(PATIENCE)
    }

    /// Send `name` and return at once.
    pub(crate) fn send(&self, name: &str) {
        signal(self.child.id(), name);
    }

    /// Its exit, which must come within `patience`.
    pub(crate) fn exit_within(&mut self, patience: Duration) -> ExitStatus {
        let deadline = Instant::now() + patience;
        loop {
            if let Some(status) = self.child.try_wait().expect("a status") {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "the client exits: {}",
                self.log()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Kill it outright, as a crash or the OOM killer would: no handler
    /// runs, nothing is closed.
    pub(crate) fn kill(&mut self) -> ExitStatus {
        self.child.kill().expect("SIGKILL is delivered");
        self.child.wait().expect("a status")
    }

    /// Whether it is still running.
    pub(crate) fn running(&mut self) -> bool {
        self.child.try_wait().expect("a status").is_none()
    }

    /// For a failure message: how the client ended, if it has, then its
    /// log -- an early exit and a crash read alike in the log alone.
    pub(crate) fn report(&mut self) -> String {
        let status = match self.child.try_wait() {
            Ok(Some(status)) => format!("exited: {status}"),
            Ok(None) => "still running".to_owned(),
            Err(e) => format!("status unknown: {e}"),
        };
        format!("{status}\n{}", self.log())
    }
}

impl Drop for App {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Send signal `name` to process `pid`.
pub(crate) fn signal(pid: u32, name: &str) {
    let sent = Command::new("kill")
        .args([&format!("-{name}"), &pid.to_string()])
        .status()
        .expect("kill runs");
    assert!(sent.success(), "SIG{name} was delivered");
}

/// Start the client for `home`'s profile on the display.
pub(crate) fn start(home: &Home) -> App {
    start_with(home, &[])
}

/// [`start`], with `extra` after the profile on the command line.
pub(crate) fn start_with(home: &Home, extra: &[&str]) -> App {
    let log = home.root.path().join("human-desktop.log");
    let stderr = std::fs::File::create(&log).expect("a log file");
    let env = |p: &std::path::Path| p.as_os_str().to_owned();
    let mut command = Command::new(app_binary());
    command.env_clear();
    // What the display and the accessibility stack need from the session
    // that started the test, the profile's own XDG tree aside: the X
    // server's cookie, and the session bus the AT-SPI bus is found
    // through. Without the bus address the adapter looks in the
    // profile's runtime directory, finds nothing and exports no tree,
    // silently.
    for name in [
        "XAUTHORITY",
        "DBUS_SESSION_BUS_ADDRESS",
        "AT_SPI_BUS_ADDRESS",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let child = command
        .args(["--profile", home.paths.profile()])
        .args(extra)
        .env("HOME", home.root.path())
        .env("XDG_CONFIG_HOME", env(&home.roots.config_home))
        .env("XDG_DATA_HOME", env(&home.roots.data_home))
        .env("XDG_STATE_HOME", env(&home.roots.state_home))
        .env("XDG_CACHE_HOME", env(&home.roots.cache_home))
        .env(
            "XDG_RUNTIME_DIR",
            env(home.roots.runtime_dir.as_deref().expect("a runtime dir")),
        )
        .env("DISPLAY", display())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .expect("the client starts");
    App { child, log }
}

/// The `human` endpoint's live lease on `home`'s daemon.
pub(crate) async fn human_lease(binding: &IpcBinding) -> Option<LeaseRecord> {
    let admin = binding
        .admin([AdminCapability::Endpoints].into())
        .await
        .expect("an admin port");
    let views = admin.leases().await.expect("the leases");
    views
        .into_iter()
        .find(|v| v.endpoint == human())
        .and_then(|v| v.lease)
}

/// Wait until the `human` lease is held (`true`) or free (`false`).
pub(crate) async fn until_lease(binding: &IpcBinding, held: bool, app: &App) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while human_lease(binding).await.is_some() != held {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the human lease never became {}: {}",
            if held { "held" } else { "free" },
            app.log()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
