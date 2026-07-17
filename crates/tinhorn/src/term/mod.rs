//! Terminal integration for the Bevy arena — a vendored, in-tree snapshot of
//! **puradox/bevy_ratatui** @ `6779d549` (fork of `ratatui/bevy_ratatui` +
//! apekros's PR #98, the Bevy-0.19 port).
//!
//! Vendored as a module (rather than a `path` dependency on a git fork) because
//! crates.io forbids git dependencies, and `cargo install tinhorn` /
//! `cargo publish` must keep working. Trimmed to the **crossterm** terminal path
//! tinhorn actually drives: the winit/`soft_ratatui` `windowed` backend is
//! dropped, and the crate's `crossterm`/`keyboard`/`mouse` cargo features (all
//! enabled in tinhorn's build) are resolved to always-on. `#![allow(dead_code)]`
//! because we keep the upstream module's full surface even though the binary
//! only reads [`RatatuiContext`], [`RatatuiPlugins`], and [`event::KeyMessage`].
//! `dead_code`/`unused_imports` are allowed so the upstream surface can be kept
//! verbatim without the binary's `-D warnings` flagging the parts it doesn't call.
//!
//! To sync with upstream: merge upstream into the fork, recopy the crossterm
//! `src/`, re-apply the trims above, and note the new SHA here.
#![allow(dead_code, unused_imports)]

mod context_trait;
mod crossterm_context;
mod ratatui_context;
mod ratatui_plugin;

pub use ratatui_context::RatatuiContext;
pub use ratatui_plugin::RatatuiPlugins;

pub mod context {
    pub use super::context_trait::TerminalContext;
    pub use super::crossterm_context::context::CrosstermContext;
    pub use super::ratatui_context::DefaultContext;
    pub use super::ratatui_plugin::ContextPlugin;
}

pub mod cleanup {
    pub use super::crossterm_context::cleanup::CleanupPlugin;
}

pub mod error {
    pub use super::crossterm_context::error::ErrorPlugin;
}

pub mod event {
    pub use super::crossterm_context::event::{
        CrosstermMessage, EventPlugin, FocusMessage, InputSet, KeyMessage, MouseMessage,
        PasteMessage, ResizeMessage,
    };
}

pub mod kitty {
    pub use super::crossterm_context::kitty::{KittyEnabled, KittyPlugin};
}

pub mod mouse {
    pub use super::crossterm_context::mouse::{MouseEnabled, MousePlugin};
}

pub mod translation {
    pub use super::crossterm_context::translation::*;
}
