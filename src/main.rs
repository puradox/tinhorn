//! roll — a terminal dice roller whose dice bounce around the screen.
//!
//! Usage:
//!   roll            # start empty, type an expression and press Enter
//!   roll 3d6        # roll immediately
//!   roll "d6+d8"    # quote expressions that contain shell-special characters
//!
//! Keys (while running):
//!   Enter        roll (or re-roll) the current expression
//!   Backspace    edit the expression
//!   Esc / Ctrl-C quit

mod app;
mod parse;
mod ui;

use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use app::App;

const FRAME: Duration = Duration::from_millis(16); // ~60 fps

fn main() -> io::Result<()> {
    let initial = std::env::args().skip(1).collect::<Vec<_>>().join(" ");

    let mut terminal = ratatui::init();
    let mut app = App::new(initial);
    let result = run(&mut terminal, &mut app);
    ratatui::restore();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> io::Result<()> {
    let mut last = Instant::now();

    loop {
        terminal.draw(|f| ui::render(f, app))?;

        // Wait for input, but never longer than our frame budget.
        if event::poll(FRAME)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

                // Ctrl-C always quits, even from the help overlay.
                if ctrl && key.code == KeyCode::Char('c') {
                    break;
                }

                // While the help overlay is up it captures input: any of
                // ?/Esc/q closes it, everything else is ignored so the user
                // can't blindly edit the hidden expression.
                if app.show_help {
                    match key.code {
                        KeyCode::Char('?') | KeyCode::Esc | KeyCode::Char('q') => {
                            app.show_help = false;
                        }
                        _ => {}
                    }
                    continue;
                }

                match key.code {
                    KeyCode::Char('?') => app.show_help = true,
                    KeyCode::Esc => break,
                    KeyCode::Enter => app.roll(),
                    KeyCode::Backspace => {
                        app.input.pop();
                    }
                    KeyCode::Char(c) if !ctrl => app.input.push(c),
                    _ => {}
                }
            }
        }

        // Advance the physics by the real elapsed time.
        let now = Instant::now();
        let dt = (now - last).as_secs_f32();
        last = now;
        app.update(dt);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn flatten(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    #[test]
    fn renders_through_a_full_roll_without_panicking() {
        let mut app = App::new("3d6".to_string());
        let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();

        // First draw establishes the arena size; the next update spawns the dice.
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        for _ in 0..6000 {
            app.update(1.0 / 60.0);
            terminal.draw(|f| ui::render(f, &mut app)).unwrap();
            if app.all_settled() {
                break;
            }
        }

        assert!(app.all_settled(), "dice did not settle within the budget");
        let screen = flatten(&terminal);
        assert!(screen.contains("roll"), "missing title");
        assert!(screen.contains("total"), "missing total label");
        // The settled d6 squares should be on screen.
        assert!(screen.contains("┌────┐"), "no die boxes rendered");
    }

    /// Not a real assertion — prints a rendered frame so you can eyeball the
    /// layout. Run with: `cargo test snapshot -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn snapshot() {
        // Override the expression with SNAP=... to eyeball other rolls.
        let expr = std::env::var("SNAP").unwrap_or_else(|_| "d4+d6+d8+d10+d12+d20".to_string());
        let mut app = App::new(expr);
        let mut terminal = Terminal::new(TestBackend::new(72, 18)).unwrap();
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        // Generous budget: an exploding chain settles dice one at a time.
        for _ in 0..40000 {
            app.update(1.0 / 60.0);
            terminal.draw(|f| ui::render(f, &mut app)).unwrap();
            if app.all_settled() {
                break;
            }
        }
        let buf = terminal.backend().buffer();
        let area = buf.area();
        eprintln!();
        for y in 0..area.height {
            let mut line = String::new();
            for x in 0..area.width {
                line.push_str(buf[(x, y)].symbol());
            }
            eprintln!("{line}");
        }
    }

    #[test]
    fn advantage_renders_the_dropped_die_dimmed() {
        use ratatui::style::Color;

        let mut app = App::new("2d20kh1".to_string());
        let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        for _ in 0..6000 {
            app.update(1.0 / 60.0);
            terminal.draw(|f| ui::render(f, &mut app)).unwrap();
            if app.all_settled() {
                break;
            }
        }
        assert!(app.all_settled());

        // Both d20s are still on screen (the dropped one isn't hidden)...
        let dropped = app.dice.iter().filter(|d| !d.kept).count();
        assert_eq!(dropped, 1, "advantage should drop exactly one die");

        // ...and the dropped die's face value is painted in the dropped-die
        // colour (DarkGray). The borders are also DarkGray, so key off a die
        // glyph: the dropped d20's value digits drawn in that colour.
        let dropped_val = app.dice.iter().find(|d| !d.kept).unwrap().final_value;
        let first_digit = dropped_val.to_string().chars().next().unwrap().to_string();
        let buf = terminal.backend().buffer();
        let has_dimmed_face = buf
            .content()
            .iter()
            .any(|c| c.fg == Color::DarkGray && c.symbol() == first_digit);
        assert!(
            has_dimmed_face,
            "dropped die's face was not rendered in its dimmed colour"
        );
    }

    #[test]
    fn help_overlay_shows_the_notation_when_toggled() {
        let mut app = App::new("3d6".to_string());
        let mut terminal = Terminal::new(TestBackend::new(72, 28)).unwrap();

        // Closed by default: the panel title isn't on screen.
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        assert!(!flatten(&terminal).contains("dice notation"));

        // Open it (what pressing `?` does) and the syntax table appears.
        app.show_help = true;
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        let screen = flatten(&terminal);
        assert!(screen.contains("dice notation"), "help title missing");
        assert!(screen.contains("advantage"), "keep/drop section missing");
        assert!(screen.contains("explode"), "exploding section missing");
        assert!(screen.contains("to close"), "dismiss hint missing");

        // Close it again.
        app.show_help = false;
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        assert!(!flatten(&terminal).contains("dice notation"));
    }

    #[test]
    fn help_overlay_fits_a_short_terminal_without_panicking() {
        // A frame too short to hold the whole panel must still render (trimmed).
        let mut app = App::new("3d6".to_string());
        app.show_help = true;
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).unwrap();
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        assert!(flatten(&terminal).contains("notation"), "panel did not render");
    }

    #[test]
    fn bad_input_shows_an_error_not_a_crash() {
        let mut app = App::new(String::new());
        app.input = "nonsense".to_string();
        app.roll();
        let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        assert!(flatten(&terminal).contains("⚠"), "error not surfaced");
    }
}
