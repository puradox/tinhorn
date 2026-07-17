# Vendored: bevy_ratatui (in-tree, as the `term` module)

Snapshot of **puradox/bevy_ratatui** @ `6779d5492271e186ac4aae2893c4595bdab8cf50`
(fork of `ratatui/bevy_ratatui` + PR #98 "Update bevy to `0.19`", apekros).

Vendored **in-tree** as `crates/tinhorn/src/term/` (a plain module, `mod term;`)
rather than a `path` dependency on a git fork, because crates.io forbids git
dependencies — this is what lets `cargo publish` / `cargo install tinhorn` work.
The module is always compiled (the arena is the default renderer).

Trimmed from the upstream crate:

- `examples/`, `Cargo.toml`, `Cargo.lock`, `cliff.toml`, `CHANGELOG.md`, README.
- `src/windowed_context/` — the winit/`soft_ratatui` backend tinhorn never uses.
- `src/lib.rs` became `src/term/mod.rs`, with the `windowed` module dropped.
- The crate's `crossterm` / `keyboard` / `mouse` cargo features (all enabled in
  tinhorn's build) are **resolved to always-on**: their `#[cfg(feature = …)]`
  gates were removed so the code compiles unconditionally.
- Internal `crate::…` paths were rewritten to `crate::term::…`.
- `#![allow(dead_code, unused_imports)]` on the module keeps the upstream surface
  verbatim even though the binary only reads `RatatuiContext`, `RatatuiPlugins`,
  and `event::KeyMessage`.

The two external crates it still pulls in (`bitflags`, `color-eyre`) are declared
in `crates/tinhorn/Cargo.toml`; `bevy`, `ratatui`, and `crossterm` are already
tinhorn dependencies.

To sync with upstream: merge upstream into the fork, recopy the crossterm `src/`
here, re-apply the trims above, and update the SHA in this file.
