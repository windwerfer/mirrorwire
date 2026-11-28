# winit 0.30.13, patched for MirrorWire

Copy of the crates.io package `winit` 0.30.13 (Apache-2.0, see `LICENSE`) without its
`examples/`, `tests/` and `docs/`; `Cargo.toml` lost its `[[example]]` entries and `include`.

The only code change is in `src/platform_impl/android/mod.rs`, marked "MirrorWire patch": Android
`ACTION_HOVER_ENTER` / `HOVER_MOVE` / `HOVER_EXIT` (a stylus or mouse near the screen, no contact)
become `CursorEntered` / `CursorMoved` / `CursorLeft`. Upstream drops them (`TODO mouse events`),
so the viewer never saw a hovering pen and could not move the presenter's cursor.

Remove this directory and the `[patch.crates-io]` entry in the root `Cargo.toml` once upstream
reports hover. Check against the new upstream when bumping winit.
