//! tinhorn — a terminal dice roller whose dice bounce around the screen.
//!
//! Two modes:
//!   tinhorn              # interactive: type an expression and shake the cup
//!   tinhorn 3d6          # interactive, rolling 3d6 immediately
//!   tinhorn -p 3d6       # one-shot: print the result and exit (for scripting)
//!   tinhorn 3d6 | cat    # one-shot too — a non-TTY stdout switches modes
//!   tinhorn --json 3d6   # one-shot, machine-readable JSON breakdown
//!   tinhorn --seed 42 3d6  # reproducible roll
//!
//! Interactive keys: Enter rolls in the current mode · Tab cycles the mode
//! (shake → roll → insta) · ? help · Ctrl-H history · Ctrl-S stats ·
//! Ctrl-Q mute · Esc quit

mod app;
mod cli;
mod foley;
mod parse;
mod ui;

use std::io::{self, IsTerminal};
use std::time::{Duration, Instant};

use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use app::{App, Pane, RollMode};
use cli::Cli;

const FRAME: Duration = Duration::from_millis(16); // ~60 fps
const MAX_CLICKS_PER_FRAME: usize = 8; // impact/knock sounds played per frame; more is mush

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

    // Interactive: launch the animated TUI. The dice are audible unless muted
    // (`--mute` starts muted; Ctrl-Q toggles) or there's no output device —
    // audio spawns lazily inside `run`, on the first sound that needs playing.
    let mut terminal = ratatui::init();

    let mut app = match cli.seed {
        Some(seed) => App::with_seed(expr, seed),
        None => App::new(expr),
    };
    app.muted = cli.mute;
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
        // Mute is global too. Q for "quiet" — Ctrl-M was the obvious chord,
        // but on legacy encodings Ctrl-M *is* Enter (ASCII CR), so it can
        // never be a hotkey; Ctrl-Q arrives cleanly in every terminal.
        KeyCode::Char('q') if ctrl => {
            app.muted = !app.muted;
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

    // The Throw: while the cup is shaking it captures input — Enter/Tab
    // releases the throw, Esc puts the dice down, and everything else is
    // swallowed so the expression can't change mid-shake.
    if app.shaking() {
        match code {
            KeyCode::Enter | KeyCode::Tab => app.throw(),
            KeyCode::Esc => app.cancel_shake(),
            _ => {}
        }
        return Action::Continue;
    }

    match code {
        KeyCode::Esc => return Action::Quit,
        // Enter rolls in the current mode; Tab cycles the mode, in the order
        // the ceremony shrinks (shake → roll → insta). The Throw stays the
        // house default: a second Enter mid-shake (handled above) releases.
        KeyCode::Enter => match app.mode {
            RollMode::Shake => app.start_shake(),
            RollMode::Roll => app.roll(),
            RollMode::Insta => app.insta_roll(),
        },
        KeyCode::Tab => app.mode = app.mode.next(),
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

    // Audio runs on its own thread, spawned lazily on the first sound that
    // actually needs playing — for two reasons:
    //   * opening the output device blocks for tens of ms while the OS audio
    //     stack starts, which on the render loop is a visible hitch on the
    //     first sound; the thread absorbs that cost instead.
    //   * a `--mute` session emits no sound, so the thread never spawns and the
    //     audio APIs are never touched — on some macOS setups even opening
    //     playback draws a microphone prompt, and a muted session shouldn't be
    //     the one to draw it.
    let mut sound: Option<foley::Foley> = None;

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

        // Whatever the physics wanted heard, hand to the audio thread — which
        // spawns on the first event, so a muted session (take_sounds returns
        // nothing while muted) never starts it. Impacts and knocks are capped
        // per frame: a dense pool can strike dozens of times inside 16ms, each
        // one a fresh buffer to synthesize and mix, and eight overlapping
        // clicks already sound like a fistful of dice.
        let mut clicks = 0usize;
        for ev in app.take_sounds() {
            let f = sound.get_or_insert_with(foley::Foley::spawn);
            if matches!(
                ev,
                app::SoundEvent::Impact { .. } | app::SoundEvent::Knock { .. }
            ) {
                clicks += 1;
                if clicks > MAX_CLICKS_PER_FRAME {
                    continue;
                }
            }
            f.play(ev);
        }
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
        assert_eq!(
            handle_key(&mut app, KeyCode::Char('h'), true),
            Action::Continue
        );
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
    fn enter_shakes_then_throws_by_default() {
        let mut app = App::new(String::new());
        type_str(&mut app, "2d20kh1");

        // Enter picks the cup up; nothing is rolled yet.
        assert_eq!(
            handle_key(&mut app, KeyCode::Enter, false),
            Action::Continue
        );
        assert!(app.shaking());
        assert!(app.dice.is_empty(), "shaking must not roll");

        // Typing while shaking is swallowed — the expression is locked in.
        type_str(&mut app, "9d9");
        assert_eq!(app.input, "2d20kh1", "input changed mid-shake");

        // Esc puts the dice down instead of quitting.
        assert_eq!(handle_key(&mut app, KeyCode::Esc, false), Action::Continue);
        assert!(!app.shaking());
        assert!(app.dice.is_empty(), "a cancelled shake must not roll");

        // Shake again and release with a second Enter: now the roll happens.
        handle_key(&mut app, KeyCode::Enter, false);
        assert_eq!(
            handle_key(&mut app, KeyCode::Enter, false),
            Action::Continue
        );
        assert!(!app.shaking());
        assert_eq!(app.dice.len(), 2, "the throw rolls the locked expression");
    }

    #[test]
    fn tab_cycles_the_roll_mode() {
        let mut app = App::new(String::new());
        let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
        terminal.draw(|f| ui::render(f, &mut app)).unwrap(); // size the arena
        type_str(&mut app, "2d6");

        // The full ritual is the default.
        assert_eq!(app.mode, RollMode::Shake);

        // One Tab: a plain animated roll — no cup, dice still bounce.
        assert_eq!(handle_key(&mut app, KeyCode::Tab, false), Action::Continue);
        assert_eq!(app.mode, RollMode::Roll);
        assert_eq!(app.input, "2d6", "Tab must not type or roll");
        handle_key(&mut app, KeyCode::Enter, false);
        assert!(!app.shaking(), "roll mode must not shake");
        assert_eq!(app.dice.len(), 2);
        assert!(!app.all_settled(), "a plain roll still animates");

        // Two Tabs in: insta — the dice land settled between two frames.
        handle_key(&mut app, KeyCode::Tab, false);
        assert_eq!(app.mode, RollMode::Insta);
        handle_key(&mut app, KeyCode::Enter, false);
        assert!(!app.shaking(), "insta must not shake");
        assert_eq!(app.dice.len(), 2);
        assert!(app.all_settled(), "insta must land already at rest");

        // A third Tab wraps back to the cup.
        handle_key(&mut app, KeyCode::Tab, false);
        assert_eq!(app.mode, RollMode::Shake);
    }

    #[test]
    fn shaking_renders_the_cup_and_power_meter() {
        let mut app = App::new(String::new());
        let mut terminal = Terminal::new(TestBackend::new(72, 24)).unwrap();
        terminal.draw(|f| ui::render(f, &mut app)).unwrap(); // size the arena

        type_str(&mut app, "3d6");
        handle_key(&mut app, KeyCode::Enter, false);
        app.update(0.3);
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();

        let screen = flatten(&terminal);
        assert!(screen.contains("shaking"), "title should say shaking");
        assert!(screen.contains("╲____╱"), "the cup is missing");
        assert!(screen.contains("power"), "the power meter is missing");
        assert!(screen.contains("throw"), "the release hint is missing");

        // Throw and let it land: the cup vanishes and the roll completes.
        handle_key(&mut app, KeyCode::Enter, false);
        for _ in 0..6000 {
            app.update(1.0 / 60.0);
            terminal.draw(|f| ui::render(f, &mut app)).unwrap();
            if app.all_settled() {
                break;
            }
        }
        assert!(app.all_settled(), "thrown dice never settled on screen");
        let screen = flatten(&terminal);
        assert!(
            !screen.contains("╲____╱"),
            "the cup should be gone after the throw"
        );
        assert!(
            screen.contains("settled"),
            "the roll should finish as usual"
        );
    }

    #[test]
    fn ctrl_q_toggles_mute_everywhere() {
        let mut app = App::new(String::new());
        assert!(!app.muted, "sound starts on");

        // Plain toggle, and the chord must not type a 'q'.
        type_str(&mut app, "3d6");
        assert_eq!(
            handle_key(&mut app, KeyCode::Char('q'), true),
            Action::Continue
        );
        assert!(app.muted);
        assert_eq!(app.input, "3d6", "the chord must not type 'q'");

        // Works while a pane is open (and leaves the pane alone — the chord
        // must not act as the pane-closing bare 'q')…
        app.pane = Pane::Stats;
        handle_key(&mut app, KeyCode::Char('q'), true);
        assert!(!app.muted);
        assert_eq!(app.pane, Pane::Stats);
        app.pane = Pane::None;

        // …and mid-shake, without throwing or cancelling.
        handle_key(&mut app, KeyCode::Enter, false);
        assert!(app.shaking());
        handle_key(&mut app, KeyCode::Char('q'), true);
        assert!(app.muted);
        assert!(app.shaking(), "muting must not disturb the shake");

        // A bare 'q' (no ctrl) still just types.
        app.cancel_shake();
        type_str(&mut app, "q");
        assert_eq!(app.input, "3d6q");
    }

    #[test]
    fn a_staked_roll_renders_its_verdict() {
        let mut app = App::new(String::new());
        let mut terminal = Terminal::new(TestBackend::new(72, 24)).unwrap();
        roll_to_settle(&mut app, "2d20kh1 vs 12", &mut terminal);
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        let screen = flatten(&terminal);
        assert!(screen.contains("vs 12"), "the target is not on screen");
        let (success, _) = app.verdict().expect("settled staked roll has a verdict");
        if success {
            assert!(
                screen.contains("SUCCESS"),
                "verdict chip missing:\n{screen}"
            );
        } else {
            assert!(screen.contains("FAIL"), "verdict chip missing:\n{screen}");
        }
    }

    #[test]
    fn a_tiny_terminal_skips_the_cup_rather_than_breaking_the_border() {
        // Regression: on an arena narrower than the cup, drawing it would
        // paint over the border chrome. It is skipped instead.
        let mut app = App::new(String::new());
        let mut terminal = Terminal::new(TestBackend::new(9, 12)).unwrap();
        terminal.draw(|f| ui::render(f, &mut app)).unwrap(); // size the arena

        type_str(&mut app, "d6");
        handle_key(&mut app, KeyCode::Enter, false);
        app.update(0.3);
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();

        let screen = flatten(&terminal);
        assert!(
            !screen.contains("╲____╱"),
            "the cup must not squeeze into 7 cells"
        );
        assert!(!screen.contains("power"), "nor the meter");
        // The arena frame survives intact (rows 1..=4 are its left border;
        // rows 0 and 5 are corners, and the results block starts below).
        let buf = terminal.backend().buffer();
        for y in 1..5u16 {
            assert_eq!(
                buf[(0, y)].symbol(),
                "│",
                "left border corrupted at row {y}"
            );
        }
    }

    #[test]
    fn enter_with_a_bad_expression_errors_instead_of_shaking() {
        let mut app = App::new(String::new());
        type_str(&mut app, "garbage");
        handle_key(&mut app, KeyCode::Enter, false);
        assert!(!app.shaking());
        assert!(app.error.is_some(), "the typo surfaces at pickup");
        // The input stays editable — the very next key must reach it.
        type_str(&mut app, "x");
        assert_eq!(app.input, "garbagex");
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
        assert!(screen.contains("tinhorn"), "missing title");
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

    /// Not a real assertion — records scripted TUI sessions as JSON frame dumps
    /// (glyphs + colours per cell) for building demo players. Run with:
    ///   DEMO_OUT=/tmp/demo.json cargo test record_demo -- --ignored --nocapture
    #[test]
    #[ignore]
    fn record_demo() {
        use ratatui::style::Color;
        use serde_json::json;

        const W: u16 = 64;
        const H: u16 = 20;

        fn color_code(c: Color) -> char {
            match c {
                Color::Black => 'k',
                Color::Red => 'r',
                Color::Green => 'g',
                Color::Yellow => 'y',
                Color::Blue => 'b',
                Color::Magenta => 'm',
                Color::Cyan => 'c',
                Color::Gray => 'a',
                Color::DarkGray => 'd',
                Color::LightRed => 'R',
                Color::LightGreen => 'G',
                Color::LightYellow => 'Y',
                Color::LightBlue => 'B',
                Color::LightMagenta => 'M',
                Color::LightCyan => 'C',
                Color::White => 'w',
                _ => '.',
            }
        }

        /// Snapshot the buffer: one text row plus parallel fg/bg code rows per
        /// line. The cell shadowing a wide glyph (the 🎲 in the title) is
        /// dropped so text and colour rows stay index-aligned.
        fn snap(terminal: &Terminal<TestBackend>) -> serde_json::Value {
            let buf = terminal.backend().buffer();
            let area = *buf.area();
            let mut text = Vec::new();
            let mut fg = Vec::new();
            let mut bg = Vec::new();
            for y in 0..area.height {
                let (mut t, mut f, mut b) = (String::new(), String::new(), String::new());
                let mut skip = false;
                for x in 0..area.width {
                    let cell = &buf[(x, y)];
                    if skip {
                        skip = false;
                        continue;
                    }
                    let sym = cell.symbol();
                    skip = sym.chars().next().is_some_and(|c| c == '🎲');
                    t.push_str(if sym.is_empty() { " " } else { sym });
                    f.push(color_code(cell.fg));
                    b.push(color_code(cell.bg));
                }
                text.push(t);
                fg.push(f);
                bg.push(b);
            }
            json!({ "x": text, "f": fg, "b": bg })
        }

        /// One sound event as a compact JSON tuple for the player's WebAudio.
        fn sound_json(ev: &app::SoundEvent) -> serde_json::Value {
            use app::SoundEvent as E;
            match *ev {
                E::Impact { sides, speed } => json!(["impact", sides, speed]),
                E::Knock { sides, speed } => json!(["knock", sides, speed]),
                E::Settle { sides } => json!(["settle", sides]),
                E::Rattle { power } => json!(["rattle", power]),
                E::Throw { power } => json!(["throw", power]),
                E::Crit => json!(["crit"]),
                E::Fumble => json!(["fumble"]),
                E::Success => json!(["success"]),
                E::Failure => json!(["failure"]),
            }
        }

        /// Drive one scripted session: type `expr`, shake for `shake_secs`,
        /// throw, and record until everything settles (30 fps output). Sound
        /// events are drained per emitted frame so the player can foley along.
        fn record(expr: &str, seed: u64, shake_secs: f32) -> serde_json::Value {
            let mut app = App::with_seed(String::new(), seed);
            let mut terminal = Terminal::new(TestBackend::new(W, H)).unwrap();
            let mut frames = Vec::new();
            let mut n = 0usize;
            let mut step = |app: &mut App,
                            terminal: &mut Terminal<TestBackend>,
                            frames: &mut Vec<serde_json::Value>| {
                app.update(1.0 / 60.0);
                terminal.draw(|f| ui::render(f, app)).unwrap();
                // Odd frames are skipped (30 fps output); their sound events
                // stay queued and ride along with the next emitted frame.
                if n.is_multiple_of(2) {
                    let sounds: Vec<serde_json::Value> =
                        app.sounds.drain(..).map(|e| sound_json(&e)).collect();
                    let mut frame = snap(terminal);
                    frame["s"] = json!(sounds);
                    frames.push(frame);
                }
                n += 1;
            };

            terminal.draw(|f| ui::render(f, &mut app)).unwrap();
            for _ in 0..10 {
                step(&mut app, &mut terminal, &mut frames);
            }
            for c in expr.chars() {
                handle_key(&mut app, KeyCode::Char(c), false);
                for _ in 0..4 {
                    step(&mut app, &mut terminal, &mut frames);
                }
            }
            for _ in 0..12 {
                step(&mut app, &mut terminal, &mut frames);
            }
            handle_key(&mut app, KeyCode::Enter, false);
            for _ in 0..(shake_secs * 60.0) as usize {
                step(&mut app, &mut terminal, &mut frames);
            }
            handle_key(&mut app, KeyCode::Enter, false);
            for _ in 0..1500 {
                step(&mut app, &mut terminal, &mut frames);
                if app.all_settled() {
                    break;
                }
            }
            assert!(app.all_settled(), "demo roll never settled");
            for _ in 0..50 {
                step(&mut app, &mut terminal, &mut frames);
            }
            json!({ "w": W, "h": H, "fps": 30, "expr": expr, "frames": frames })
        }

        // Seed-hunt each take: the RNG draw order is fixed, so probing roll()
        // headlessly finds a seed the scripted session will reproduce exactly.
        let find_seed = |expr: &str, wanted: fn(&[u32]) -> bool| -> u64 {
            (0..)
                .find(|&s| {
                    let mut probe = App::with_seed(String::new(), s);
                    probe.input = expr.into();
                    probe.roll();
                    let vals: Vec<u32> = probe.dice.iter().map(|d| d.final_value).collect();
                    wanted(&vals)
                })
                .unwrap()
        };

        // A natural 20 over a low die: the SUCCESS verdict and the crit burst
        // land in one take. And a fail take: a clear miss, but not a natural 1.
        let seed = find_seed("2d20kh1 vs 15", |vals| {
            vals.iter().max() == Some(&20) && vals.iter().min().is_some_and(|&v| v <= 8)
        });
        let fail_seed = find_seed("d20+2 vs 15", |vals| (3..=9).contains(&vals[0]));

        // ~0.78 s of shaking lands the release right at the power peak; ~1.5 s
        // catches the trough for the contrast lob.
        let out = json!({
            "rocket": record("2d20kh1 vs 15", seed, 0.78),
            "fail": record("d20+2 vs 15", fail_seed, 1.05),
            "lob": record("4d6", 7, 1.5),
        });

        let path = std::env::var("DEMO_OUT").expect("set DEMO_OUT=<path> for the frame dump");
        std::fs::write(&path, serde_json::to_vec(&out).unwrap()).unwrap();
        eprintln!("wrote {path} (rocket seed {seed})");
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
            assert_eq!(
                showing, 1,
                "exactly one pane should be visible for {pane:?}"
            );
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
        assert!(screen.contains("The Throw"), "Throw section missing");
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
        assert!(
            flatten(&terminal).contains("notation"),
            "panel did not render"
        );
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
