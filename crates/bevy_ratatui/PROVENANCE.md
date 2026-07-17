# Vendored: bevy_ratatui

Snapshot of **puradox/bevy_ratatui** @ `6779d5492271e186ac4aae2893c4595bdab8cf50`
(fork of `ratatui/bevy_ratatui` + PR #98 "Update bevy to `0.19`", apekros).

Vendored in-tree because crates.io forbids git dependencies and `cargo install
tinhorn` must keep working. Only used when the `bevy` feature of the `tinhorn`
binary is enabled. To sync: merge upstream into the fork, copy `src/` here, and
update the SHA above.

Trimmed from upstream: `examples/`, `Cargo.lock`, `cliff.toml`, `CHANGELOG.md`.
