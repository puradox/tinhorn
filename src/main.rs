//! roll — a terminal dice roller whose dice bounce around the screen.
//!
//! Two modes:
//!   roll              # interactive: type an expression and watch it bounce
//!   roll 3d6          # interactive, rolling 3d6 immediately
//!   roll -p 3d6       # one-shot: print the result and exit (for scripting)
//!   roll 3d6 | cat    # one-shot too — a non-TTY stdout switches modes
//!   roll --json 3d6   # one-shot, machine-readable JSON breakdown
//!   roll --seed 42 3d6  # reproducible roll
//!
//! Interactive keys: Enter roll · ? help · Ctrl-H history · Ctrl-S stats · Esc quit

mod app;
mod cli;
mod parse;
mod ui;

use std::io::{self, IsTerminal};
use std::time::{Duration, Instant};

use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use app::{App, Pane};
use cli::Cli;

const FRAME: Duration = Duration::from_millis(16); // ~60 fps

/// Pressing a pane's own key while it's open closes it; pressing it while a
/// *different* pane is open switches to it.
fn toggle(current: Pane, target: Pane) -> Pane {
    if current == target {
        Pane::None
    } else {
        target
    }
}

fn main() -> io::Result<()> {
    let cli = Cli::parse();
    let expr = cli.expression();

    // One-shot mode: print a result and exit instead of animating. Triggered by
    // an explicit output flag, or automatically when stdout isn't a terminal
    // (so `roll 3d6 | cat` and `roll 3d6 > f` just work).
    let one_shot = cli.print || cli.json || cli.verbose || !io::stdout().is_terminal();
    if one_shot {
        if expr.is_empty() {
            // Nothing to roll, and either a flag was given or there's no TTY to
            // animate in — a usage error rather than a silent empty TUI.
            eprintln!("roll: no dice expression given (e.g. `roll 3d6`)");
            std::process::exit(2);
        }
        return cli::run_one_shot(&cli, &expr);
    }

    // Interactive: launch the animated TUI.
    let mut terminal = ratatui::init();
    let mut app = match cli.seed {
        Some(seed) => App::with_seed(expr, seed),
        None => App::new(expr),
    };
    let result = run(&mut terminal, &mut app);
    ratatui::restore();
    result
}

/// What the event loop should do after handling a key.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    Continue,
    Quit,
}

/// Apply one key press to the app. Pure (no I/O) so the routing is unit-tested;
/// the bug this guards against is a pane hotkey eating a letter the user needs
/// to *type* (e.g. the `h` in `kh`).
fn handle_key(app: &mut App, code: KeyCode, ctrl: bool) -> Action {
    // Ctrl-C always quits, even from an overlay.
    if ctrl && code == KeyCode::Char('c') {
        return Action::Quit;
    }

    // Pane hotkeys are global (work whether or not a pane is open) and use
    // chords / `?` so they never collide with typed notation — `h` and `s` have
    // to stay typeable for `kh`/`dh` and any expression containing them. Pressing
    // a pane's key again closes it.
    match code {
        KeyCode::Char('?') => {
            app.pane = toggle(app.pane, Pane::Help);
            return Action::Continue;
        }
        KeyCode::Char('h') if ctrl => {
            app.pane = toggle(app.pane, Pane::History);
            return Action::Continue;
        }
        KeyCode::Char('s') if ctrl => {
            app.pane = toggle(app.pane, Pane::Stats);
            return Action::Continue;
        }
        _ => {}
    }

    // While a pane is open it captures the rest of input: Esc/q close it,
    // everything else is ignored so the hidden expression can't be edited blind.
    if app.pane != Pane::None {
        if matches!(code, KeyCode::Esc | KeyCode::Char('q')) {
            app.pane = Pane::None;
        }
        return Action::Continue;
    }

    match code {
        KeyCode::Esc => return Action::Quit,
        KeyCode::Enter => app.roll(),
        KeyCode::Backspace => {
            app.input.pop();
        }
        KeyCode::Char(c) if !ctrl => app.input.push(c),
        _ => {}
    }
    Action::Continue
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
                if handle_key(app, key.code, ctrl) == Action::Quit {
                    break;
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
    use app::Pane;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// Feed a string of plain (non-ctrl) characters through the key handler.
    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            assert_eq!(handle_key(app, KeyCode::Char(c), false), Action::Continue);
        }
    }

    #[test]
    fn typing_kh_reaches_the_input_and_does_not_open_a_pane() {
        // Regression: `h`/`s` must be typeable so `kh`, `dh`, etc. work. Before
        // the fix a bare `h` opened the history pane instead of typing.
        let mut app = App::new(String::new());
        type_str(&mut app, "2d20kh1");
        assert_eq!(app.input, "2d20kh1");
        assert_eq!(app.pane, Pane::None, "typing must never open a pane");

        // An `s`-bearing expression types cleanly too.
        let mut app2 = App::new(String::new());
        type_str(&mut app2, "3d6s"); // a stray 's' is just text
        assert_eq!(app2.input, "3d6s");
        assert_eq!(app2.pane, Pane::None);
    }

    #[test]
    fn ctrl_chords_toggle_panes_without_typing() {
        let mut app = App::new("3d6".to_string());

        // Ctrl-H opens history; the letter is not inserted into the input.
        assert_eq!(handle_key(&mut app, KeyCode::Char('h'), true), Action::Continue);
        assert_eq!(app.pane, Pane::History);
        assert_eq!(app.input, "3d6", "the chord must not type 'h'");

        // Pressing it again closes it; Ctrl-S then opens stats.
        handle_key(&mut app, KeyCode::Char('h'), true);
        assert_eq!(app.pane, Pane::None);
        handle_key(&mut app, KeyCode::Char('s'), true);
        assert_eq!(app.pane, Pane::Stats);

        // `?` toggles help and switches from another open pane.
        handle_key(&mut app, KeyCode::Char('?'), false);
        assert_eq!(app.pane, Pane::Help);
    }

    #[test]
    fn esc_closes_a_pane_then_quits() {
        let mut app = App::new("3d6".to_string());
        app.pane = Pane::History;
        // First Esc just closes the pane (does not quit).
        assert_eq!(handle_key(&mut app, KeyCode::Esc, false), Action::Continue);
        assert_eq!(app.pane, Pane::None);
        // With no pane open, Esc quits.
        assert_eq!(handle_key(&mut app, KeyCode::Esc, false), Action::Quit);
    }

    #[test]
    fn keys_are_swallowed_while_a_pane_is_open() {
        let mut app = App::new(String::new());
        app.pane = Pane::Stats;
        // Typing while a pane is open must not edit the hidden expression.
        type_str(&mut app, "9d9");
        assert_eq!(app.input, "", "input changed while a pane was open");
        assert_eq!(app.pane, Pane::Stats);
    }

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

    /// Roll an expression to completion so it lands in history.
    fn roll_to_settle(app: &mut App, expr: &str, terminal: &mut Terminal<TestBackend>) {
        app.input = expr.to_string();
        app.roll();
        terminal.draw(|f| ui::render(f, app)).unwrap();
        for _ in 0..6000 {
            app.update(1.0 / 60.0);
            if app.all_settled() {
                break;
            }
        }
    }

    #[test]
    fn history_pane_lists_recent_rolls() {
        let mut app = App::new(String::new());
        let mut terminal = Terminal::new(TestBackend::new(72, 28)).unwrap();

        // Empty history shows a hint, not a crash.
        app.pane = app::Pane::History;
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        assert!(flatten(&terminal).contains("no rolls yet"));

        // After a couple of rolls the pane lists them with their totals.
        app.pane = app::Pane::None;
        roll_to_settle(&mut app, "3d6", &mut terminal);
        roll_to_settle(&mut app, "d20", &mut terminal);
        app.pane = app::Pane::History;
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        let screen = flatten(&terminal);
        assert!(screen.contains("history"), "history title missing");
        assert!(screen.contains("3d6"), "first roll not listed");
        assert!(screen.contains("d20"), "second roll not listed");
    }

    #[test]
    fn stats_pane_shows_odds_and_session_summary() {
        let mut app = App::new(String::new());
        let mut terminal = Terminal::new(TestBackend::new(72, 28)).unwrap();
        roll_to_settle(&mut app, "3d6", &mut terminal);

        app.pane = app::Pane::Stats;
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        let screen = flatten(&terminal);
        assert!(screen.contains("statistics"), "stats title missing");
        assert!(screen.contains("Odds for"), "odds header missing");
        assert!(screen.contains("min 3"), "min not shown for 3d6");
        assert!(screen.contains("max 18"), "max not shown for 3d6");
        assert!(screen.contains("This session"), "session summary missing");
    }

    #[test]
    fn panes_are_mutually_exclusive() {
        // Only one pane title is ever on screen at a time.
        let mut app = App::new("3d6".to_string());
        let mut terminal = Terminal::new(TestBackend::new(72, 28)).unwrap();
        for pane in [app::Pane::Help, app::Pane::History, app::Pane::Stats] {
            app.pane = pane;
            terminal.draw(|f| ui::render(f, &mut app)).unwrap();
            let s = flatten(&terminal);
            let titles = ["dice notation", "🎲   history", "🎲   statistics"];
            let showing = titles.iter().filter(|t| s.contains(**t)).count();
            assert_eq!(showing, 1, "exactly one pane should be visible for {pane:?}");
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
        app.pane = app::Pane::Help;
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        let screen = flatten(&terminal);
        assert!(screen.contains("dice notation"), "help title missing");
        assert!(screen.contains("advantage"), "keep/drop section missing");
        assert!(screen.contains("explode"), "exploding section missing");
        assert!(screen.contains("to close"), "dismiss hint missing");

        // Close it again.
        app.pane = app::Pane::None;
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        assert!(!flatten(&terminal).contains("dice notation"));
    }

    #[test]
    fn help_overlay_fits_a_short_terminal_without_panicking() {
        // A frame too short to hold the whole panel must still render (trimmed).
        let mut app = App::new("3d6".to_string());
        app.pane = app::Pane::Help;
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
