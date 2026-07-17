//! tinhorn-core — the roll semantics and 3D dice simulation shared by the
//! `tinhorn` terminal binary (and, later, the chronicle web embed).
//!
//! Everything here is renderer-agnostic: the hand-written [`parse`]r, the seeded
//! roll evaluator and ceremony in [`app`], the Rapier [`physics`] world, the die
//! [`dice_geom`]etry (also the source of the physics collision hulls), and the
//! arena camera math in [`view_math`] (also where the crit/fumble particle
//! bursts are projected). No ratatui, no rodio, no clap — those live in the
//! binary; this crate stays GPU- and terminal-free so its ~30 sim/ceremony
//! tests run without a display.

pub mod app;
pub mod dice_geom;
pub mod parse;
pub mod physics;
pub mod view_math;
