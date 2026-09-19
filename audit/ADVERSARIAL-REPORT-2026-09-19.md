# Adversarial campaign: relay discovery lifecycle

Date: 2026-09-19. Repository: gzapi-org/InterWeave.

## Scope and actual execution

This is an executed, focused campaign against production Rust functions, not a full-repository security certification. No production branch, contract, production implementation, or dependency lockfile was modified. The audit branch contains the installer, two branch-only workflows, and this report. The installer appends test-only modules in the disposable runner checkout. The deliberate mutation described below was made only in that checkout.

Initial production baseline: `8e853b908e5220893f615d5d67404788803b1d47`.

Baseline audit commit: `8137d4e7cf924a3eb0899ee05ef0f826aca7f2c7`.

[Baseline execution, job 105914700649](https://github.com/gzapi-org/InterWeave/actions/runs/35449738109/job/105914700649).

Replay production baseline, resolved from main: `8c51e90dc996fb0e9b0c78094db70a0c162ae489`.

Replay workflow commit: `67166667e53057c8fe10401b036a83069de51ac8`. This is the workflow commit, NOT the production commit under test: the replay explicitly checks out `8c51e90...` before installing the identical test modules.

[Replay execution, job 105915232549](https://github.com/gzapi-org/InterWeave/actions/runs/35449943810/job/105915232549).

Both executions used repository-pinned Rust 1.97.1 on Ubuntu 24.04, `cargo test --locked`, one test thread, and the actual crates `interweave-transport-runtime` and `interweave-transport-libp2p`. Both verified that Cargo.lock was unchanged.

The tests invoke the actual relay driver's `learn()` function and the actual `ReservationManager`. They construct valid Identify information for real Ed25519 identities and use the actual trust-policy objects. They do NOT exchange Identify packets over sockets or demonstrate end-to-end network exploitation. No test dials an external host. Loopback addresses are data in these tests, not external targets.

## Results

| Experiment | Original baseline | Main replay |
|---|---|---|
| Authorized relay advertisement control | Pass | Pass |
| Unauthorized relay advertisement control | Pass | Pass |
| Configured route protected from Identify control | Pass | Pass |
| Learned-to-static promotion replaces peer-supplied routes | Pass | Pass |
| 512 generated histories, 128 operations each | Pass | Pass |
| Fresh explicit HOP withdrawal stops new reservation asks | FAIL | FAIL |
| Current address survives a previously full learned-address set | FAIL | FAIL |

The generated histories contain 65,536 operation steps per run, with a small independent ledger for accepted/withdrawn addresses and assertions on reservation bounds. They are deterministic generated histories, not exhaustive model checking or coverage-guided fuzzing. Passing them proves only the properties and histories exercised.

Observed Rust summaries on each baseline:

```text
interweave-transport-runtime:
  test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 401 filtered out

interweave-transport-libp2p:
  test result: FAILED. 3 passed; 2 failed; 0 ignored; 0 measured; 306 filtered out
```

The workflows deliberately finish red when a counterexample fails. The per-step GitHub `conclusion` can appear successful because `continue-on-error` allows the other experiments to execute. The actual test logs and the recorded `steps.driver.outcome = failure` are the result, not that success-looking step summary.

## F1: Learned relay capability withdrawal is ignored

Classification: reproduced functional/policy defect at the production driver boundary. Suggested priority: P2. Requires `use_authorized_identify_relays = true` and an already authorized peer. This is not a trust bypass.

Locations:

- `crates/transport/libp2p/src/runtime/relay_driver.rs`, `handle_relay()` and `learn()`.
- `architecture/transport/libp2p/CONNECTIVITY.md`, section 7.

Contract: a learned relay requires configured or freshly observed HOP support; fresh Identify evidence supersedes cached capability observations.

Minimal history:

1. An authorized peer advertises Identify and HOP and one direct listener address.
2. The driver records the peer as a learned relay; no reservation has yet been requested.
3. The same peer supplies fresh Identify information with a NONEMPTY supported-protocol list containing Identify but not HOP.
4. The next manager tick still returns `Action::Reserve` for that peer.

Both runs failed this assertion with:

```text
a fresh explicit HOP withdrawal still planned a reservation at ["/ip4/127.0.0.1/tcp/4001"]
```

Test: `runtime::relay_driver::adversarial_campaign::fresh_identify_withdrawal_must_stop_new_reservation_requests`.

Cause: `learn()` immediately returns when HOP is absent. That avoids adding a new candidate but does not invalidate a previously learned one. The old candidate remains askable. The nonempty protocol list prevents confusing explicit withdrawal with an omitted field in a partial push. The test stays idle until withdrawal, so it does not depend on an interpretation of whether an existing active reservation should be terminated.

Impact: a peer that explicitly no longer advertises relay service is still selected for new reservation work. Stale learned topology survives fresh contradictory evidence. The test proves the wrong reservation action; repeated network retries and operational cost are inferred from the production action path, not measured on the wire here.

Fix direction: handle Identify as an update. For learned candidates, invalidate eligibility on explicit capability withdrawal and reconcile pending work through the driver's normal lifecycle cleanup. Preserve operator-configured static candidates. Decide any active-reservation teardown separately according to its contract.

## F2: The bounded learned-address list permanently excludes fresh routes after churn

Classification: reproduced availability defect at the production driver boundary. Suggested priority: P2. Same opt-in and authorization prerequisites as F1.

Locations:

- `crates/transport/libp2p/src/runtime/relay_driver.rs`, `learn()`.
- `crates/transport/runtime/src/relay.rs`, `ReservationManager::learn()` and `Candidate::add_address()`.

Minimal history:

1. An authorized HOP peer advertises eight listener addresses, ports 4100 through 4107.
2. Fresh Identify information replaces that listener set with one address, port 4900.
3. The next reservation action still carries only the eight old addresses; the sole current address is absent.

Both executions reported:

```text
the only current address was discarded; planned stale routes:
["/ip4/127.0.0.1/tcp/4100", "/ip4/127.0.0.1/tcp/4101",
 "/ip4/127.0.0.1/tcp/4102", "/ip4/127.0.0.1/tcp/4103",
 "/ip4/127.0.0.1/tcp/4104", "/ip4/127.0.0.1/tcp/4105",
 "/ip4/127.0.0.1/tcp/4106", "/ip4/127.0.0.1/tcp/4107"]
```

The display above wraps the single long log line without changing the addresses.

Test: `runtime::relay_driver::adversarial_campaign::fresh_listen_address_must_survive_previous_address_capacity`.

Cause: learned addresses are accumulated by appending. A novel ninth address is refused at the eight-address ceiling. The driver ignores the refusal and neither replaces the old snapshot nor reclaims obsolete entries. This is a liveness problem despite bounded memory.

Impact: normal address churn can leave an otherwise eligible relay selectable only at obsolete addresses. If those old addresses are no longer usable, the driver's reservation attempts cannot use the route the relay currently advertises. The test proves the stale action set, not a measured network outage.

Fix direction: reconcile each learned peer's current bounded Identify address snapshot, rather than treating every observed address as a permanent append-only fact. Ensure a current usable address cannot be crowded out by obsolete historical addresses. Keep configured and learned provenance separate; raising the ceiling only postpones the failure.

## Mutation control

On the ORIGINAL baseline only, after running the unmodified implementation, the runner changed exactly one line in its temporary working tree:

```rust
candidate.addresses = vec![address.to_owned()];
```

to:

```rust
candidate.addresses.push(address.to_owned());
```

It then reran only `relay::adversarial_campaign::control_promotion_retains_only_operator_supplied_routes`.

The mutant compiled and failed its behavioral assertion. The original passed. Thus this regression test detects the reintroduced promotion defect. The mutant is NOT a third production bug. It was not committed, and no mutation was applied during the main replay.

## Reproduce the original baseline

From an existing repository checkout, use a separate working tree:

```sh
git fetch origin audit/adversarial-base-8e853b9
git worktree add --detach ../InterWeave-adversarial 8137d4e7cf924a3eb0899ee05ef0f826aca7f2c7
cd ../InterWeave-adversarial
python3 audit/adversarial/install_tests.py
cargo test -p interweave-transport-runtime --lib adversarial_campaign --locked -- --nocapture
cargo test -p interweave-transport-libp2p --lib adversarial_campaign --locked -- --nocapture
```

The installer refuses to proceed if relevant production paths differ from its recorded baseline or if its modules were already installed. It modifies only the disposable working tree by adding test modules. The driver test command is expected to fail twice on the recorded baselines.

For the exact main replay procedure, see `.github/workflows/adversarial-replay.yml` on this audit branch; it records the separate production SHA and changes only the installer's baseline literal, not its test expectations.

## Limits and remaining campaign work

This batch covered relay state histories, capability withdrawal, address churn at capacity, authorization/configured-route controls, one targeted mutation, and replay on the current main recorded above. It did not run the complete workspace CI, long-duration fuzzing, persistence fault injection, power-loss simulation, a full real-socket hostile-peer campaign, or a separate independent reviewer. The two reports were checked against the contract and controls, but are not externally adjudicated.

Neither issue grants data-plane authority to an unauthorized peer. Both show that admitting a fresh snapshot and reconciling an already-known peer are different operations. The memory bound and initial-admission controls work in these tests while freshness/recovery fails.
