// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The Android side on a real device, driven over `adb` (architect-cto's
//! DECISION of 2026-10-10): the case runs in the app's instrumentation,
//! in the app's own process, over the binding of the runtime the app's
//! service started; this host drives the lifecycle and reads the result
//! back from the instrumentation's status (the seam agreed with
//! rust-ui-dev, 01a12771).
//!
//! What a device run is, so its evidence is read for what it is:
//! - **Every case is a fresh process.** `am instrument` starts the app's
//!   process for the run and the process ends with it, so the runtime
//!   starts for each case that needs one ([`cases::NEED_A_RUNTIME`]).
//!   [`restart`](AdbDevice::restart) is therefore the next case, and a
//!   stop is the platform's force-stop; the app's graceful stop is the
//!   app's own platform test, not this.
//! - **The network is adb's tunnel.** The relay and the desktop listen on
//!   this host's loopback, and [`reach`](AdbDevice::reach) reverses each
//!   port, so the phone's sockets reach the host over the USB/TCP link
//!   adb holds, at the same loopback address -- real sockets on the
//!   phone's runtime, not a radio network, no NAT.
//! - **The identity must outlive the process.** A profile's `PeerId` is
//!   known before the first start and kept across restarts;
//!   [`AdbDevice::connect`] refuses a build whose identity changes across
//!   a force-stop rather than letting a case read that as a network fault.
//!
//! `ANDROID_SERIAL` selects the device as for `adb` itself;
//! `INTERWEAVE_ANDROID_INSTRUMENTATION` names the instrumentation
//! component (`<test package>/<runner>`), which the app's androidTest
//! build defines.

use std::process::{Command, Output};

use interweave_android_e2e_cases::{cases, keys};
use interweave_transport_api::TransportIdentity;
use serde_json::{Map, Value};

use crate::{CaseRun, Device};

/// The app under test.
pub const PACKAGE: &str = "org.interweave.human";

/// The environment variable naming the instrumentation component.
pub const INSTRUMENTATION_ENV: &str = "INTERWEAVE_ANDROID_INSTRUMENTATION";

/// The prefix the instrumentation gives each result key in its status.
pub const STATUS_PREFIX: &str = "interweave.";

/// The Android side on a device.
pub struct AdbDevice {
    instrumentation: String,
    peer: TransportIdentity,
    reversed: Vec<u16>,
}

impl AdbDevice {
    /// The device `adb` selects, its app's identity read and checked to
    /// survive a force-stop.
    ///
    /// # Panics
    /// With no instrumentation named, no device, a failing `identity`
    /// case, or an identity that changed across the force-stop.
    #[must_use]
    pub fn connect() -> Self {
        let instrumentation = std::env::var(INSTRUMENTATION_ENV).unwrap_or_else(|_| {
            panic!("{INSTRUMENTATION_ENV} names no instrumentation: a device run needs the app's androidTest build installed")
        });
        let identity = |instrumentation: &str| {
            let out = parse(&instrument(
                instrumentation,
                cases::IDENTITY,
                &Value::Object(Map::new()),
            ));
            assert_eq!(
                out.get(keys::RESULT).and_then(Value::as_str),
                Some(keys::PASS),
                "the identity case: {out:?}"
            );
            out.get(keys::PEER)
                .and_then(Value::as_str)
                .and_then(|p| TransportIdentity::parse(p).ok())
                .unwrap_or_else(|| panic!("the identity case answered no PeerId: {out:?}"))
        };
        let peer = identity(&instrumentation);
        force_stop();
        assert_eq!(
            identity(&instrumentation),
            peer,
            "the app's PeerId changed across a force-stop: this build has no persistent identity, and no case can name it in advance"
        );
        Self {
            instrumentation,
            peer,
            reversed: Vec::new(),
        }
    }

    /// The device's recent log, for a failing case to show.
    #[must_use]
    pub fn log() -> String {
        adb(&["logcat", "-d", "-t", "300"]).map_or_else(
            |e| format!("(no logcat: {e})"),
            |o| String::from_utf8_lossy(&o.stdout).into_owned(),
        )
    }

    /// The bytes of `path` under the app's private directories, through
    /// `run-as` -- how a case's captured payloads come back.
    ///
    /// # Panics
    /// If `adb` cannot read it.
    #[must_use]
    pub fn pull(path: &str) -> Vec<u8> {
        let out = adb(&["exec-out", "run-as", PACKAGE, "cat", path]).expect("adb");
        assert!(
            out.status.success(),
            "run-as cat {path}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }
}

impl Device for AdbDevice {
    fn peer(&self) -> TransportIdentity {
        self.peer.clone()
    }

    fn reach(&mut self, port: u16) {
        let spec = format!("tcp:{port}");
        run_ok(&["reverse", &spec, &spec]);
        self.reversed.push(port);
    }

    fn start(&mut self, config: &str) {
        let out = parse(&instrument(
            &self.instrumentation,
            cases::PROVISION,
            &serde_json::json!({ "config": config }),
        ));
        assert_eq!(
            out.get(keys::RESULT).and_then(Value::as_str),
            Some(keys::PASS),
            "provisioning the app: {out:?}"
        );
    }

    fn stop(&mut self) {
        force_stop();
    }

    fn kill(&mut self) {
        force_stop();
    }

    /// Nothing to do: the next case's instrumentation starts the process.
    fn restart(&mut self) {}

    fn run_case(&self, case: &str, args: &Value) -> CaseRun {
        let (instrumentation, name, args) =
            (self.instrumentation.clone(), case.to_owned(), args.clone());
        CaseRun::on_thread(case, move || {
            Value::Object(parse(&instrument(&instrumentation, &name, &args))).to_string()
        })
    }
}

impl Drop for AdbDevice {
    fn drop(&mut self) {
        // A test leaves nothing behind it did not find: the reversals go,
        // and the app does not keep running a case's profile.
        for port in &self.reversed {
            let _ = adb(&["reverse", "--remove", &format!("tcp:{port}")]);
        }
        force_stop();
    }
}

fn adb(args: &[&str]) -> std::io::Result<Output> {
    Command::new("adb").args(args).output()
}

fn run_ok(args: &[&str]) {
    let out = adb(args).expect("adb");
    assert!(
        out.status.success(),
        "adb {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn force_stop() {
    let _ = adb(&["shell", "am", "force-stop", PACKAGE]);
}

/// `am instrument`'s output for `case` with `args`.
fn instrument(instrumentation: &str, case: &str, args: &Value) -> String {
    let command = instrument_argv(instrumentation, case, args);
    let command: Vec<&str> = command.iter().map(String::as_str).collect();
    match adb(&command) {
        Ok(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
        Err(e) => format!("INSTRUMENTATION_FAILED: adb: {e}"),
    }
}

/// The `adb` arguments that run `case`: the arguments travel base64, so
/// no shell between here and the runner can split or expand them.
#[must_use]
pub fn instrument_argv(instrumentation: &str, case: &str, args: &Value) -> Vec<String> {
    [
        "shell",
        "am",
        "instrument",
        "-w",
        "-r",
        "-e",
        "case",
        case,
        "-e",
        "args",
        &base64(args.to_string().as_bytes()),
        instrumentation,
    ]
    .map(str::to_owned)
    .to_vec()
}

/// The result a run's raw output (`am instrument -r`) carries: each
/// `INSTRUMENTATION_STATUS: interweave.<key>=<value>` as `<key>`, a
/// value running on over the lines up to the next `INSTRUMENTATION_` line.
/// A run that reported no result -- a crash, a missing runner -- is a
/// failure whose detail is the whole output, so nothing about why is
/// lost.
#[must_use]
pub fn parse(output: &str) -> Map<String, Value> {
    let mut out = Map::new();
    let mut current: Option<(String, String)> = None;
    let flush = |current: &mut Option<(String, String)>, out: &mut Map<String, Value>| {
        if let Some((key, value)) = current.take() {
            out.insert(key, value.into());
        }
    };
    for line in output.lines() {
        if line.starts_with("INSTRUMENTATION_") {
            flush(&mut current, &mut out);
            if let Some(rest) = line.strip_prefix("INSTRUMENTATION_STATUS: ")
                && let Some((key, value)) = rest.split_once('=')
                && let Some(key) = key.strip_prefix(STATUS_PREFIX)
            {
                current = Some((key.to_owned(), value.to_owned()));
            }
        } else if let Some((_, value)) = current.as_mut() {
            value.push('\n');
            value.push_str(line);
        }
    }
    flush(&mut current, &mut out);
    if !out.contains_key(keys::RESULT) {
        out.insert(keys::RESULT.to_owned(), keys::FAIL.into());
        out.insert(
            keys::DETAIL.to_owned(),
            format!("the instrumentation reported no result:\n{output}").into(),
        );
    }
    out
}

/// RFC 4648 base64, padded: what `android.util.Base64.decode(_, DEFAULT)`
/// reads back.
#[must_use]
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 0x3f) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc_4648s_vectors() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(plain.as_bytes()), encoded, "{plain:?}");
        }
        // Every byte value, so the alphabet's last two and the high bits
        // are read: 0xfb 0xff -> "+/8=".
        assert_eq!(base64(&[0xfb, 0xff]), "+/8=");
    }

    #[test]
    fn the_case_and_its_arguments_reach_the_runner_as_single_words() {
        let hostile = serde_json::json!({ "config": "a b\n'c' $HOME \"d\"" });
        let argv = instrument_argv("org.interweave.human.test/Runner", "provision", &hostile);
        assert_eq!(
            &argv[..8],
            [
                "shell",
                "am",
                "instrument",
                "-w",
                "-r",
                "-e",
                "case",
                "provision"
            ]
        );
        assert_eq!(argv[8], "-e");
        assert_eq!(argv[9], "args");
        assert!(
            argv[10]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b)),
            "nothing a shell would split or expand: {}",
            argv[10]
        );
        assert_eq!(argv[10], base64(hostile.to_string().as_bytes()));
        assert_eq!(argv[11], "org.interweave.human.test/Runner");
        assert_eq!(argv.len(), 12);
    }

    #[test]
    fn a_status_is_read_by_its_interweave_keys_and_a_value_may_run_on() {
        let raw = "\
INSTRUMENTATION_STATUS: interweave.case=paths
INSTRUMENTATION_STATUS: interweave.result=fail
INSTRUMENTATION_STATUS: interweave.detail=no route to 12D3Koo within the deadline
second line of the detail
INSTRUMENTATION_STATUS: class=org.interweave.human.test.E2eCase
INSTRUMENTATION_STATUS_CODE: 0
INSTRUMENTATION_RESULT: stream=
INSTRUMENTATION_CODE: -1
";
        let out = parse(raw);
        assert_eq!(out[keys::CASE], "paths");
        assert_eq!(out[keys::RESULT], keys::FAIL);
        assert_eq!(
            out[keys::DETAIL],
            "no route to 12D3Koo within the deadline\nsecond line of the detail"
        );
        assert!(
            !out.contains_key("class"),
            "only interweave.* keys: {out:?}"
        );
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn a_pass_is_read_as_one() {
        let out = parse(
            "INSTRUMENTATION_STATUS: interweave.result=pass\nINSTRUMENTATION_STATUS: interweave.path=relayed\nINSTRUMENTATION_CODE: -1\n",
        );
        assert_eq!(out[keys::RESULT], keys::PASS);
        assert_eq!(out[keys::PATH], "relayed");
    }

    #[test]
    fn a_run_that_reported_no_result_fails_with_all_it_printed() {
        let raw = "INSTRUMENTATION_RESULT: shortMsg=Process crashed.\nINSTRUMENTATION_CODE: 0\n";
        let out = parse(raw);
        assert_eq!(out[keys::RESULT], keys::FAIL);
        let detail = out[keys::DETAIL].as_str().expect("a detail");
        assert!(detail.contains("Process crashed."), "{detail}");
    }
}
