---
role: "devex-tooling"
class: solution
description: "How to measure a CI display/AT-SPI wrapper for real on develop-qzapp, which has no Xvfb or dbus-run-session"
tier: 2
knowledge_scope: full
distilled_at: "2026-10-05"
origin:
  - agent: "devex-tooling"
    host: "develop-qzapp"
    project: interweave
    working_copy: interweave
derived_from:
  - 4644b78272687145
---

## How to measure a CI display/AT-SPI wrapper for real on develop-qzapp, which has no Xvfb or dbus-run-session

develop-qzapp (Qubes, Fedora) has no Xvfb and no dbus-run-session, but rootless podman
works: `podman run --rm -v $PWD/tools/ci:/w:ro,Z docker.io/library/ubuntu:24.04` with
`apt-get install --no-install-recommends xvfb dbus-daemon libglib2.0-bin at-spi2-core`
reproduces the GitHub runner's packages. A GTK3 window read back through pyatspi
(python3-gi gir1.2-gtk-3.0 gir1.2-atspi-2.0 python3-pyatspi libatk-adaptor) is the live
control that AT-SPI carries an app's tree; without at-spi2-core, org.a11y.Bus is
ServiceUnknown. Measured 2026-10-04 for InterWeave's tools/ci/with_display.sh: 30/30,
under a second to stand up. Remove the pulled image afterwards (80.7 MB).

Traps hit writing its self-test: a stub that `exec sleep` leaves an orphan holding the
`$(...)` capture pipe (each case waits for the sleep); and "Xvfb gone after return"
passes for a wrapper that only waits rather than kills — record the SIGTERM in the stub.

New tooling dirs under tools/ are invisible to check_guards_are_wired.sh and to
`cargo xtask selftests` until both list them (tools/ci added on the stage15-supply
branch, 2026-10-04). See [[review-class-cannot-write-scratch]].

*References: review-class-cannot-write-scratch*

*Observed 2026-10-04 (devex-tooling)*
