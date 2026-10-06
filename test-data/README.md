# Mutable test data

Non-normative inputs used to exercise tests. These may evolve without protocol/version changes. Keep them separate from frozen `fixtures/`.

- `human-chat/render-parity.json` -- the render-parity golden (`human-client-ui.md` section 13): what `crates/human/ui-slint` draws for each `HumanChatV2` text, compared by its `tests/render_parity.rs` and, at Stage 17, by the Android instrumented test. `RENDER_PARITY_WRITE=1` rewrites it; review every change.
