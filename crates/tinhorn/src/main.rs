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
//! (shake → roll → insta) · ←/→ move the caret in the expression (Home/End
//! jump) · ↑/↓ scroll an open pane · ? help · Ctrl-H history · Ctrl-S stats ·
//! Ctrl-Q mute · Esc quit

// The roll semantics and 3D dice simulation live in the `tinhorn-core` library
// (shared with the future chronicle web embed). Re-exported at the crate root so
// the binary's modules keep referring to `crate::app` / `crate::parse` /
// `crate::physics` unchanged.
pub use tinhorn_core::{app, parse, physics};

mod cli;
mod foley;
mod render3d;
mod render3d_view;
mod ui;

// The experimental Bevy arena (Stage-2 spike). Compiled only under `--features
// bevy`, so a default build/`cargo install` never links Bevy and this module —
// and the whole engine — is absent from the shipped binary.
#[cfg(feature = "bevy")]
mod scene;

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

    // Experimental Bevy arena, opt-in via `--bevy` or `TINHORN_BEVY=1`. Reached
    // only in interactive mode — placed *after* the one-shot short-circuit above,
    // so scripting and pipes never construct a Bevy `App` or touch a GPU.
    if cli.bevy || std::env::var_os("TINHORN_BEVY").is_some() {
        #[cfg(feature = "bevy")]
        {
            scene::run(expr, cli.seed);
            return Ok(());
        }
        #[cfg(not(feature = "bevy"))]
        {
            eprintln!("tinhorn: --bevy needs a build with `--features bevy`");
            std::process::exit(2);
        }
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
            app.set_pane(toggle(app.pane, Pane::Help));
            return Action::Continue;
        }
        KeyCode::Char('h') if ctrl => {
            app.set_pane(toggle(app.pane, Pane::History));
            return Action::Continue;
        }
        KeyCode::Char('s') if ctrl => {
            app.set_pane(toggle(app.pane, Pane::Stats));
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

    // While a pane is open it captures the rest of input: Esc/q close it, the
    // arrows scroll a pane too tall for the screen, and everything else is
    // ignored so the hidden expression can't be edited blind. The scroll offset
    // is clamped to the overflow at render time, so over-scrolling past the end
    // corrects itself on the next frame.
    if app.pane != Pane::None {
        match code {
            KeyCode::Esc | KeyCode::Char('q') => app.set_pane(Pane::None),
            KeyCode::Up => app.pane_scroll = app.pane_scroll.saturating_sub(1),
            KeyCode::Down => app.pane_scroll = app.pane_scroll.saturating_add(1),
            KeyCode::PageUp => app.pane_scroll = app.pane_scroll.saturating_sub(10),
            KeyCode::PageDown => app.pane_scroll = app.pane_scroll.saturating_add(10),
            KeyCode::Home => app.pane_scroll = 0,
            KeyCode::End => app.pane_scroll = u16::MAX,
            _ => {}
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
        // Line editing on the expression: the caret walks with Left/Right (and
        // jumps with Home/End), and inserts/deletes happen at it.
        KeyCode::Left => app.cursor_left(),
        KeyCode::Right => app.cursor_right(),
        KeyCode::Home => app.cursor_home(),
        KeyCode::End => app.cursor_end(),
        KeyCode::Backspace => app.input_backspace(),
        KeyCode::Delete => app.input_delete(),
        KeyCode::Char(c) if !ctrl => app.input_insert(c),
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

        // The tray is always drawn, so to catch the *cup* we compare two frames of
        // the same scene: roll first (dice on the felt, no idle hint) for a stable
        // baseline, then start shaking. The cup (a solid block) and the meter paint
        // over felt cells, so a healthy count of *changed* arena cells proves the
        // cup rendered — a culled-to-nothing cup would change almost nothing.
        let snapshot = |t: &Terminal<TestBackend>| -> Vec<(String, ratatui::style::Color)> {
            let buf = t.backend().buffer();
            (0..buf.area().width)
                .flat_map(|x| (1..16u16).map(move |y| (x, y)))
                .map(|(x, y)| {
                    let c = &buf[(x, y)];
                    (c.symbol().to_string(), c.fg)
                })
                .collect()
        };

        type_str(&mut app, "3d6");
        handle_key(&mut app, KeyCode::Tab, false); // Roll
        handle_key(&mut app, KeyCode::Tab, false); // Insta
        handle_key(&mut app, KeyCode::Enter, false); // settle a roll on the felt
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        let baseline = snapshot(&terminal); // tray + settled dice

        handle_key(&mut app, KeyCode::Tab, false); // Insta → Shake
        handle_key(&mut app, KeyCode::Enter, false); // start shaking
        app.update(0.3);
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        let shaking = snapshot(&terminal); // + the cup (and the meter)
        let changed = baseline
            .iter()
            .zip(&shaking)
            .filter(|(a, b)| a != b)
            .count();
        assert!(
            changed > 50,
            "the 3D cup didn't render (only {changed} arena cells changed)"
        );

        // The previous roll's dice — and their number overlays — are withheld while
        // shaking (they're gathered in the cup), so the settled digits the baseline
        // shows must be gone now, not painting over the cup and its meter.
        let digit_cells = |snap: &[(String, ratatui::style::Color)]| {
            snap.iter()
                .filter(|(s, _)| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
                .count()
        };
        assert!(
            digit_cells(&baseline) > 0,
            "the settled roll should show its numbers on the felt"
        );
        assert_eq!(
            digit_cells(&shaking),
            0,
            "die numbers must not render over the cup while shaking"
        );

        // The rest of the shake ceremony: the title, the power meter, the hint.
        let screen = flatten(&terminal);
        assert!(screen.contains("shaking"), "title should say shaking");
        assert!(
            screen.contains('▀'),
            "the 3D arena should render while shaking"
        );
        assert!(screen.contains("power"), "the power meter is missing");
        assert!(screen.contains("throw"), "the release hint is missing");

        // Throw and let it land: the meter vanishes and the roll completes.
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
            !screen.contains("power"),
            "the power meter should be gone after the throw"
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
    fn a_tiny_terminal_skips_the_meter_rather_than_breaking_the_border() {
        // Regression: on an arena too narrow for the power meter, drawing it
        // would paint over the border chrome. It is skipped instead. (The cup is
        // now a 3D object, rasterised into the arena interior, so it can't spill
        // over the border at all — but the meter is still a text overlay.)
        let mut app = App::new(String::new());
        let mut terminal = Terminal::new(TestBackend::new(9, 12)).unwrap();
        terminal.draw(|f| ui::render(f, &mut app)).unwrap(); // size the arena

        type_str(&mut app, "d6");
        handle_key(&mut app, KeyCode::Enter, false);
        app.update(0.3);
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();

        let screen = flatten(&terminal);
        assert!(!screen.contains("power"), "the meter must not squeeze in");
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

    #[test]
    fn arrows_move_the_caret_and_edits_land_at_it() {
        let mut app = App::new(String::new());
        type_str(&mut app, "3d6");
        // Typing leaves the caret at the end of the line.
        assert_eq!(app.cursor, 3);

        // Left steps back over the '6'; the next keystroke inserts there rather
        // than at the end.
        handle_key(&mut app, KeyCode::Left, false);
        assert_eq!(app.cursor, 2);
        handle_key(&mut app, KeyCode::Char('0'), false);
        assert_eq!(app.input, "3d06", "the digit lands under the caret");
        assert_eq!(app.cursor, 3);

        // Backspace deletes the character before the caret (the '0' just typed).
        handle_key(&mut app, KeyCode::Backspace, false);
        assert_eq!(app.input, "3d6");
        assert_eq!(app.cursor, 2);

        // Delete removes the character under the caret (the '6').
        handle_key(&mut app, KeyCode::Delete, false);
        assert_eq!(app.input, "3d");
        assert_eq!(app.cursor, 2);

        // Home/End jump to the ends; an insert at Home prepends.
        handle_key(&mut app, KeyCode::Home, false);
        assert_eq!(app.cursor, 0);
        handle_key(&mut app, KeyCode::Char('+'), false);
        assert_eq!(app.input, "+3d");
        handle_key(&mut app, KeyCode::End, false);
        assert_eq!(app.cursor, app.input.len());

        // The caret clamps at both ends — Right past the end and Left past the
        // start are no-ops, not panics.
        handle_key(&mut app, KeyCode::Right, false);
        assert_eq!(app.cursor, app.input.len());
        handle_key(&mut app, KeyCode::Home, false);
        handle_key(&mut app, KeyCode::Left, false);
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn the_caret_renders_over_the_character_it_covers() {
        use ratatui::style::Color;

        let mut app = App::new(String::new());
        let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
        type_str(&mut app, "3d6");
        // Walk the caret back onto the 'd'.
        handle_key(&mut app, KeyCode::Left, false); // over '6'
        handle_key(&mut app, KeyCode::Left, false); // over 'd'
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();

        // Exactly one reverse-video cell (the block caret) is on screen, and it
        // covers the character the caret sits on.
        let buf = terminal.backend().buffer();
        let caret: Vec<_> = buf
            .content()
            .iter()
            .filter(|c| c.bg == Color::Cyan)
            .collect();
        assert_eq!(caret.len(), 1, "exactly one block caret is drawn");
        assert_eq!(caret[0].symbol(), "d", "the caret covers the char under it");
    }

    #[test]
    fn the_input_scrolls_horizontally_to_keep_the_caret_visible() {
        use ratatui::style::Color;

        // A narrow frame: the expression is wider than the input row, so without
        // horizontal scrolling the end-of-line caret would clip off the edge.
        let mut app = App::new(String::new());
        let mut terminal = Terminal::new(TestBackend::new(24, 24)).unwrap();
        type_str(&mut app, "d6+d6+d6+d6+d6+d6"); // 17 chars; caret at the end
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();

        // The caret is still drawn (it scrolled into view rather than clipping).
        let cyan = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .filter(|c| c.bg == Color::Cyan)
            .count();
        assert_eq!(cyan, 1, "the caret follows the edit point into view");
    }

    #[test]
    fn delete_at_end_of_line_is_a_no_op() {
        // The empty branch of input_delete (caret at end, nothing under it) must
        // not panic or alter the input.
        let mut app = App::new(String::new());
        type_str(&mut app, "3d6");
        handle_key(&mut app, KeyCode::End, false);
        handle_key(&mut app, KeyCode::Delete, false);
        assert_eq!(app.input, "3d6");
        assert_eq!(app.cursor, 3);
    }

    #[test]
    fn the_caret_survives_a_throw() {
        // A throw restores the locked expression; the caret must stay a valid
        // index into it, and the next keystroke must edit without panicking.
        let mut app = App::new(String::new());
        type_str(&mut app, "2d20kh1");
        handle_key(&mut app, KeyCode::Home, false); // caret to the start
        handle_key(&mut app, KeyCode::Enter, false); // shake
        assert!(app.shaking());
        handle_key(&mut app, KeyCode::Enter, false); // throw
        assert!(!app.shaking());
        assert_eq!(app.input, "2d20kh1");
        assert!(
            app.cursor <= app.input.len(),
            "caret must stay within the input"
        );
        handle_key(&mut app, KeyCode::Char('x'), false);
        assert_eq!(
            app.input, "x2d20kh1",
            "the next keystroke edits at the caret"
        );
    }

    #[test]
    fn page_keys_scroll_a_pane_by_a_chunk() {
        let mut app = App::new(String::new());
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        handle_key(&mut app, KeyCode::Char('?'), false); // open help (26 lines)
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        assert_eq!(app.pane_scroll, 0);

        // PageDown jumps a chunk (ten lines), not one.
        handle_key(&mut app, KeyCode::PageDown, false);
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        let after_pgdn = app.pane_scroll;
        assert!(after_pgdn >= 6, "PageDown scrolls by a chunk, not a line");

        // PageUp walks back toward the top.
        handle_key(&mut app, KeyCode::PageUp, false);
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        assert!(app.pane_scroll < after_pgdn, "PageUp scrolls back up");
    }

    #[test]
    fn history_pane_scrolls_to_older_rolls() {
        // Regression: the pane used to trim to the frame and hide the rest behind
        // "… and N older", so the scroll affordance couldn't reach them. Now the
        // whole list lays out and scrolls.
        let mut app = App::new(String::new());
        let mut terminal = Terminal::new(TestBackend::new(72, 8)).unwrap();
        for expr in ["1d4", "2d4", "3d4", "4d4", "5d4", "6d4", "7d4", "8d4"] {
            roll_to_settle(&mut app, expr, &mut terminal);
        }
        app.pane = app::Pane::History;
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        let top = flatten(&terminal);
        assert!(top.contains("8d4"), "the newest roll shows at the top");
        assert!(!top.contains("1d4"), "the oldest roll is below the fold");

        // End scrolls to the bottom, bringing the oldest roll into view.
        handle_key(&mut app, KeyCode::End, false);
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        assert!(
            flatten(&terminal).contains("1d4"),
            "scrolling reaches the oldest roll"
        );
    }

    #[test]
    fn stats_pane_scrolls_when_it_overflows() {
        let mut app = App::new(String::new());
        let mut terminal = Terminal::new(TestBackend::new(72, 12)).unwrap();
        roll_to_settle(&mut app, "3d6", &mut terminal); // ~25 lines of stats

        app.pane = app::Pane::Stats;
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        let top = flatten(&terminal);
        assert!(top.contains("Odds for"), "the header shows at the top");
        assert!(
            !top.contains("This session"),
            "the session summary is below the fold"
        );

        // End scrolls to the bottom, revealing the session summary.
        handle_key(&mut app, KeyCode::End, false);
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        assert!(
            flatten(&terminal).contains("This session"),
            "scrolling reveals the session summary"
        );
    }

    #[test]
    fn arrows_scroll_a_pane_taller_than_the_frame() {
        let mut app = App::new(String::new());
        // A frame too short to hold the whole help panel, so it must scroll.
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();

        // Open help (opening resets the scroll to the top).
        handle_key(&mut app, KeyCode::Char('?'), false);
        assert_eq!(app.pane, Pane::Help);
        assert_eq!(app.pane_scroll, 0);
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        let top = flatten(&terminal);
        assert!(top.contains("Dice"), "the first heading shows at the top");
        assert!(!top.contains("to close"), "the footer is below the fold");

        // End jumps to the last screenful (u16::MAX, clamped on render): the
        // footer scrolls in and the first heading scrolls off.
        handle_key(&mut app, KeyCode::End, false);
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        let bottom = flatten(&terminal);
        assert!(bottom.contains("to close"), "the footer scrolls into view");
        assert!(!bottom.contains("Dice"), "the top heading scrolled away");

        // The offset is clamped: another Down can't run past the final screenful.
        let settled = app.pane_scroll;
        assert!(settled > 0, "the pane actually scrolled");
        handle_key(&mut app, KeyCode::Down, false);
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        assert_eq!(app.pane_scroll, settled, "scrolling stops at the bottom");

        // Home returns to the top.
        handle_key(&mut app, KeyCode::Home, false);
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        assert_eq!(app.pane_scroll, 0);
        assert!(
            flatten(&terminal).contains("Dice"),
            "Home shows the top again"
        );
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
        // The dice render in 3D, filling the arena with half-blocks.
        assert!(screen.contains('▀'), "no 3D dice rendered");
    }

    #[test]
    fn renders_the_roll_in_3d_with_burned_values() {
        let mut app = App::new(String::new());
        let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
        terminal.draw(|f| ui::render(f, &mut app)).unwrap(); // size the arena

        // Insta-roll a d6 so a settled die exists to render.
        type_str(&mut app, "1d6");
        handle_key(&mut app, KeyCode::Tab, false); // Roll
        handle_key(&mut app, KeyCode::Tab, false); // Insta
        handle_key(&mut app, KeyCode::Enter, false);
        assert!(app.all_settled());

        // The 3D die renders as half-blocks in the arena, and its value is burned
        // in — proving the whole render3d → blit → overlay path reaches the buffer.
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let arena: Vec<(u16, u16)> = (0..buf.area().width)
            .flat_map(|x| (0..14u16).map(move |y| (x, y)))
            .collect();
        let has_block = arena.iter().any(|&(x, y)| buf[(x, y)].symbol() == "▀");
        assert!(has_block, "the 3D die should fill arena cells with '▀'");
        let value = app.dice[0].final_value.to_string();
        let has_value = arena.iter().any(|&(x, y)| buf[(x, y)].symbol() == value);
        assert!(has_value, "the die's value should be burned onto the face");
    }

    // The number rides the frontmost face and ducks out when the die turns
    // edge-on: a cube squared to the eye reads one face clearly (high clarity, the
    // digit shows), but rotated 45° two faces tie for frontmost (clarity ~0, the
    // airborne digit blinks off) — so it reads as ink on the tumbling solid, not a
    // fixed label. One threshold works because clarity is the leader's lead, not
    // an absolute facing.
    #[test]
    fn the_number_ducks_out_when_the_die_rolls_edge_on() {
        use crate::render3d::dice;
        use crate::render3d::math::{Quat, Vec3};

        let faces = dice::face_geometry(6); // a cube
                                            // Face-on: the +Z face points straight at the eye and clearly leads.
        let (_c, face_on) = ui::read_face(faces, Quat::IDENTITY, Vec3::Z);
        assert!(
            face_on > 0.5,
            "a squarely-presented face should read clearly, got clarity {face_on}"
        );
        // Edge-on: a 45° spin about Y ties two faces for frontmost, so clarity
        // collapses and the airborne number ducks out there.
        let edge = Quat::from_rotation_y(std::f32::consts::FRAC_PI_4);
        let (_c, edge_on) = ui::read_face(faces, edge, Vec3::Z);
        assert!(
            edge_on < 0.1,
            "an edge-on cube ties two faces, so clarity should be ~0, got {edge_on}"
        );
    }

    // The number scales to the die's on-screen size: a small die keeps the crisp
    // single cell (scale 0), and the digits grow as the die does — so a wide
    // terminal, which renders the dice large, doesn't leave a speck of a number on
    // a big die. A wider (two-digit) number needs a wider die for the same scale.
    #[test]
    fn the_number_scales_with_the_die() {
        // A tiny die can't fit even a scale-1 half-block glyph (3×3 cells) → 0.
        assert_eq!(ui::number_scale(3.0, 2.0, 1), 0, "small die → single cell");
        // Room for the 3×3 glyph but not the 6×5 one → scale 1.
        assert_eq!(ui::number_scale(6.0, 3.0, 1), 1);
        // A big die fits the larger glyph → scale 2.
        assert_eq!(ui::number_scale(12.0, 6.0, 1), 2);
        // Bigger dice keep scaling up.
        assert!(ui::number_scale(40.0, 20.0, 1) > ui::number_scale(12.0, 6.0, 1));
        // Two digits are wider, so the same die yields a smaller (or equal) scale.
        assert!(ui::number_scale(9.0, 6.0, 2) <= ui::number_scale(9.0, 6.0, 1));
    }

    /// Eyeball the 3D shaking cup:
    ///   cargo test preview_cup_3d -- --ignored --nocapture
    #[test]
    #[ignore]
    fn preview_cup_3d() {
        let mut app = App::new(String::new());
        let mut terminal = Terminal::new(TestBackend::new(66, 22)).unwrap();
        terminal.draw(|f| ui::render(f, &mut app)).unwrap(); // size the arena
        type_str(&mut app, "3d6");
        handle_key(&mut app, KeyCode::Enter, false); // start shaking
        app.update(0.45); // let the cup sway off-centre and wobble
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        let buf = terminal.backend().buffer();
        let area = buf.area();
        eprintln!("\n=== shaking (3D cup) ===");
        for y in 0..area.height {
            let mut line = String::new();
            for x in 0..area.width {
                line.push_str(buf[(x, y)].symbol());
            }
            eprintln!("{line}");
        }
    }

    /// Eyeball the 3D dice arena after a roll settles. Override the expression
    /// with SNAP=... :
    ///   SNAP="d20+d12+d6" cargo test preview_dice_3d -- --ignored --nocapture
    #[test]
    #[ignore]
    fn preview_dice_3d() {
        let expr = std::env::var("SNAP").unwrap_or_else(|_| "d20+d12+d10+d8+d6+d4".to_string());
        let parse_env = |k: &str, d: u16| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d)
        };
        let (w, h) = (parse_env("W", 66), parse_env("H", 22));
        let mut app = App::new(expr);
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        // A coverage map so die size is actually visible: every arena cell is
        // '▀', so raw symbols show nothing — instead print ' ' for felt-coloured
        // cells and '#'/the digit for cells the dice cover, and report how much
        // of the arena the dice fill (a proxy for how big they read on screen).
        let felt = ratatui::style::Color::Rgb(28, 46, 40);
        let dump = |terminal: &Terminal<TestBackend>, tag: &str| {
            let buf = terminal.backend().buffer();
            let area = buf.area();
            let (mut die_cells, mut arena_cells) = (0u32, 0u32);
            eprintln!("\n=== {tag} ===");
            for y in 0..area.height {
                let mut line = String::new();
                for x in 0..area.width {
                    let cell = &buf[(x, y)];
                    let sym = cell.symbol();
                    // Inside the arena border, translate to a coverage glyph.
                    if sym == "▀" || sym.chars().all(|c| c.is_ascii_digit()) {
                        arena_cells += 1;
                        let is_die = cell.fg != felt || cell.bg != felt;
                        if is_die {
                            die_cells += 1;
                        }
                        if sym.chars().all(|c| c.is_ascii_digit()) {
                            line.push_str(sym);
                        } else if is_die {
                            line.push('#');
                        } else {
                            line.push(' ');
                        }
                        continue;
                    }
                    line.push_str(sym);
                }
                eprintln!("{line}");
            }
            eprintln!(
                "dice cover {die_cells}/{arena_cells} arena cells ({:.0}%)",
                100.0 * die_cells as f32 / arena_cells.max(1) as f32
            );
        };
        for i in 0..40000 {
            app.update(1.0 / 60.0);
            terminal.draw(|f| ui::render(f, &mut app)).unwrap();
            if i == 14 {
                dump(&terminal, "mid-flight (physics: dice scattered)");
            }
            if app.all_settled() {
                break;
            }
        }
        dump(&terminal, "settled (values burned in)");
    }

    /// Not a real assertion — prints a rendered frame so you can eyeball the
    /// layout. Run with: `cargo test snapshot -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn snapshot() {
        // Override the expression with SNAP=... and the terminal size with W=/H=
        // (e.g. to eyeball the number scaling up on a wide terminal).
        let expr = std::env::var("SNAP").unwrap_or_else(|_| "d4+d6+d8+d10+d12+d20".to_string());
        let dim = |k: &str, d: u16| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d)
        };
        let mut app = App::new(expr);
        let mut terminal = Terminal::new(TestBackend::new(dim("W", 72), dim("H", 18))).unwrap();
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

    /// Reconstruct the rendered frame as a PPM image at `path`. The terminal
    /// packs two pixels per cell into '▀' (fg = upper, bg = lower), so a text
    /// dump shows none of the colour; this rebuilds the true image. Convert with
    /// `magick <path> out.png`.
    fn write_ppm(terminal: &Terminal<TestBackend>, path: &str) {
        use ratatui::style::Color;
        fn rgb(c: Color) -> [u8; 3] {
            match c {
                Color::Rgb(r, g, b) => [r, g, b],
                Color::Black => [0, 0, 0],
                Color::White => [230, 230, 230],
                Color::Red => [200, 70, 70],
                Color::Green => [70, 190, 90],
                Color::Yellow => [220, 200, 60],
                Color::Blue => [80, 120, 230],
                Color::Magenta => [200, 90, 200],
                Color::Cyan => [70, 190, 200],
                Color::Gray => [160, 160, 160],
                Color::DarkGray => [90, 90, 90],
                Color::LightGreen => [130, 230, 130],
                Color::LightMagenta => [230, 130, 230],
                _ => [13, 17, 23], // Reset / unknown → terminal background
            }
        }
        let buf = terminal.backend().buffer();
        let area = *buf.area();
        let (pw, ph) = (area.width as usize, area.height as usize * 2);
        let mut px = vec![0u8; pw * ph * 3];
        for cy in 0..area.height {
            for cx in 0..area.width {
                let cell = &buf[(cx, cy)];
                let (top, bot) = if cell.symbol() == "▀" {
                    (rgb(cell.fg), rgb(cell.bg))
                } else {
                    (rgb(cell.fg), rgb(cell.fg))
                };
                for (row, color) in [(cy as usize * 2, top), (cy as usize * 2 + 1, bot)] {
                    let i = (row * pw + cx as usize) * 3;
                    px[i..i + 3].copy_from_slice(&color);
                }
            }
        }
        let mut out = format!("P6\n{pw} {ph}\n255\n").into_bytes();
        out.extend_from_slice(&px);
        std::fs::write(path, out).unwrap();
    }

    /// Not a real assertion — reconstructs the rendered frame as a real image so
    /// the 3D arena can be eyeballed in colour. Writes a PPM; convert and view:
    ///   SNAP="3d6" SHAKE=1 cargo test preview_png -- --ignored --nocapture \
    ///     && magick /tmp/tinhorn.ppm /tmp/tinhorn.png
    /// Env: SNAP=expr, W/H=frame size, SHAKE=show the cup, THROW=N stop mid-air,
    /// SEED=N pin the roll (and the whole frame) for a stable side-by-side.
    #[test]
    #[ignore]
    fn preview_png() {
        let expr = std::env::var("SNAP").unwrap_or_else(|_| "d20+d12+d8+d6".to_string());
        let parse_env = |k: &str, d: u16| {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d)
        };
        let (w, h) = (parse_env("W", 96), parse_env("H", 30));
        let shake = std::env::var("SHAKE").is_ok();

        // SEED=N pins the roll (and so the whole frame) for a stable eyeball;
        // without it each run rolls fresh, as the app does.
        let mut app = match std::env::var("SEED").ok().and_then(|v| v.parse().ok()) {
            Some(seed) => App::with_seed(String::new(), seed),
            None => App::new(String::new()),
        };
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| ui::render(f, &mut app)).unwrap(); // size the arena
        type_str(&mut app, &expr);
        // Settle a roll first (so a shake has dice on the felt, as in the report).
        handle_key(&mut app, KeyCode::Tab, false); // Roll
        handle_key(&mut app, KeyCode::Tab, false); // Insta
        handle_key(&mut app, KeyCode::Enter, false);
        // Let the reading-focus ease in so the settled eyeball shows the lean-in
        // camera (the app reaches this ~1 s after a roll comes to rest).
        if !shake && std::env::var("THROW").is_err() {
            for _ in 0..90 {
                app.update(1.0 / 60.0);
            }
        }
        if shake {
            handle_key(&mut app, KeyCode::Tab, false); // Insta → Shake mode
            handle_key(&mut app, KeyCode::Enter, false); // start shaking
            app.update(0.45); // sway the cup off-centre
        }
        // THROW=<frames>: roll fresh and stop mid-air to check the launch is in view.
        if let Ok(n) = std::env::var("THROW").map(|v| v.parse::<u32>().unwrap_or(6)) {
            let mut fresh = App::with_seed(String::new(), 7);
            fresh.arena_w = app.arena_w;
            fresh.arena_h = app.arena_h;
            type_str(&mut fresh, &expr);
            handle_key(&mut fresh, KeyCode::Tab, false); // Shake → Roll
            handle_key(&mut fresh, KeyCode::Enter, false); // rain the dice in
            for _ in 0..n {
                fresh.update(1.0 / 60.0);
            }
            app = fresh;
        }
        terminal.draw(|f| ui::render(f, &mut app)).unwrap();
        write_ppm(&terminal, "/tmp/tinhorn.ppm");
        eprintln!("wrote /tmp/tinhorn.ppm");
    }

    /// Not a real assertion — renders the arena under several candidate palettes
    /// (the same seeded roll each time) to PPMs for a side-by-side design pick:
    ///   cargo test preview_designs -- --ignored --nocapture
    ///   for f in /tmp/design-*.ppm; do magick "$f" "${f%.ppm}.png"; done
    #[test]
    #[ignore]
    fn preview_designs() {
        use crate::render3d::color::Rgb;
        use ui::ArenaStyle;

        // Each variant changes only the palette (background/felt/lip + light
        // warmth); the shape, texture, and lighting model are the shared new tray,
        // and the roll is identical. `..d` inherits the tuned ambient/lip.
        let d = ArenaStyle::DEFAULT;
        let variants: [(&str, ArenaStyle); 6] = [
            ("0-slate", d), // the current default
            (
                "1-casino",
                ArenaStyle {
                    background: Rgb(12, 22, 16),
                    floor: Rgb(26, 74, 46),
                    wall: Rgb(58, 44, 32),
                    key: Rgb(255, 244, 222),
                    fill: Rgb(80, 96, 88),
                    ..d
                },
            ),
            (
                "2-noir",
                ArenaStyle {
                    background: Rgb(9, 9, 11),
                    floor: Rgb(28, 30, 34),
                    wall: Rgb(66, 56, 50),
                    key: Rgb(255, 230, 198),
                    fill: Rgb(64, 68, 78),
                    ..d
                },
            ),
            (
                "3-leather",
                ArenaStyle {
                    background: Rgb(20, 15, 11),
                    floor: Rgb(104, 80, 52),
                    wall: Rgb(66, 44, 28),
                    key: Rgb(255, 242, 220),
                    fill: Rgb(96, 84, 70),
                    ..d
                },
            ),
            (
                "4-twilight",
                ArenaStyle {
                    background: Rgb(13, 11, 22),
                    floor: Rgb(42, 32, 62),
                    wall: Rgb(70, 46, 84),
                    key: Rgb(232, 224, 255),
                    fill: Rgb(110, 104, 140),
                    ..d
                },
            ),
            (
                "5-ocean",
                ArenaStyle {
                    background: Rgb(10, 18, 24),
                    floor: Rgb(30, 58, 72),
                    wall: Rgb(44, 64, 74),
                    key: Rgb(240, 248, 255),
                    fill: Rgb(88, 110, 120),
                    ..d
                },
            ),
        ];

        for (name, style) in variants {
            ui::set_arena_style(Some(style));
            let mut app = App::with_seed(String::new(), 12);
            let mut terminal = Terminal::new(TestBackend::new(110, 34)).unwrap();
            terminal.draw(|f| ui::render(f, &mut app)).unwrap(); // size the arena
            type_str(&mut app, "d20+d12+d8+d6");
            handle_key(&mut app, KeyCode::Tab, false); // Roll
            handle_key(&mut app, KeyCode::Tab, false); // Insta
            handle_key(&mut app, KeyCode::Enter, false); // settle
            terminal.draw(|f| ui::render(f, &mut app)).unwrap();
            write_ppm(&terminal, &format!("/tmp/design-{name}.ppm"));
            eprintln!("wrote /tmp/design-{name}.ppm");
        }
        ui::set_arena_style(None);
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
            // Match distinctive pane *content* rather than the emoji titles: the
            // dense 3D arena behind can bleed a half-block into a title's wide
            // emoji cell. Help = its notation reference, History = the empty-list
            // line, Stats = the odds header — each unique to one pane.
            let markers = ["dice notation", "no rolls yet", "Odds for"];
            let showing = markers.iter().filter(|t| s.contains(**t)).count();
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
