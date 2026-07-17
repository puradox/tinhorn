//! Mouse support.
use std::io::stdout;

use bevy::prelude::*;
use ratatui::crossterm::{
    ExecutableCommand,
    event::{DisableMouseCapture, EnableMouseCapture},
};

/// Plugin responsible for enabling mouse capture.
pub struct MousePlugin;

impl Plugin for MousePlugin {
    fn build(&self, app: &mut bevy::prelude::App) {
        app.add_systems(Startup, mouse_setup);
    }
}

/// Resource indicating that mouse capture was successfully enabled in the current terminal buffer.
#[derive(Resource, Default)]
pub struct MouseEnabled;

fn mouse_setup(mut commands: Commands) -> Result {
    stdout().execute(EnableMouseCapture)?;
    commands.insert_resource(MouseEnabled);
    Ok(())
}

impl Drop for MouseEnabled {
    fn drop(&mut self) {
        let _ = stdout().execute(DisableMouseCapture);
    }
}
