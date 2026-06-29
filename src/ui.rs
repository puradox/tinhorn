//! All ratatui rendering lives here.

use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Padding, Paragraph};
use ratatui::Frame;

use crate::app::{App, Die, Pane, Stats, DIE_H, DIE_W};

/// Per-die colour palette; dice cycle through it by index.
const PALETTE: [Color; 8] = [
    Color::Cyan,
    Color::Yellow,
    Color::Green,
    Color::Magenta,
    Color::Red,
    Color::Blue,
    Color::LightGreen,
    Color::LightMagenta,
];

fn die_color(idx: usize) -> Color {
    PALETTE[idx % PALETTE.len()]
}

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let chunks = Layout::vertical([
        Constraint::Min(5),    // bouncing arena
        Constraint::Length(4), // results
        Constraint::Length(1), // input field
        Constraint::Length(1), // help
    ])
    .split(area);

    render_arena(frame, app, chunks[0]);
    render_results(frame, app, chunks[1]);
    render_input(frame, app, chunks[2]);
    render_help(frame, chunks[3]);

    // A pop-out pane floats on top of everything when one is toggled open.
    match app.pane {
        Pane::None => {}
        Pane::Help => render_help_overlay(frame, area),
        Pane::History => render_history_overlay(frame, app, area),
        Pane::Stats => render_stats_overlay(frame, app, area),
    }
}

fn render_arena(frame: &mut Frame, app: &mut App, area: Rect) {
    let title = if app.all_settled() {
        " 🎲  roll — settled ".to_string()
    } else if app.spawned {
        " 🎲  roll — rolling… ".to_string()
    } else {
        " 🎲  roll ".to_string()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(title.bold());

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Hand the arena size back to the simulation.
    app.arena_w = inner.width as f32;
    app.arena_h = inner.height as f32;

    if !app.spawned || inner.width < DIE_W as u16 || inner.height < DIE_H as u16 {
        return;
    }

    let buf = frame.buffer_mut();
    for die in &app.dice {
        draw_die(buf, inner, die);
    }
}

/// Paint a single die into the buffer at its current position, drawn as the
/// 2D silhouette of the matching polyhedron (triangle for d4, square for d6,
/// diamond for d8, kite for d10, pentagon for d12, hexagon for d20).
fn draw_die(buf: &mut ratatui::buffer::Buffer, inner: Rect, die: &Die) {
    let max_x = inner.right().saturating_sub(DIE_W as u16);
    let max_y = inner.bottom().saturating_sub(DIE_H as u16);

    let bx = (inner.x + die.x.round() as u16).clamp(inner.x, max_x);
    let by = (inner.y + die.y.round() as u16).clamp(inner.y, max_y);

    // A dropped die (kept == false) is still thrown and animated, but rendered
    // greyed-and-dimmed so you can watch e.g. advantage discard the lower d20.
    let style = if !die.kept {
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM)
    } else if die.settled {
        Style::default()
            .fg(die_color(die.color_idx))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(die_color(die.color_idx))
            .add_modifier(Modifier::DIM)
    };

    let rows = die_shape(die.sides, die.shown);
    for (i, row) in rows.iter().enumerate() {
        buf.set_string(bx, by + i as u16, row, style);
    }
}

/// The four-row, six-cell-wide ASCII silhouette for a die of `sides`, with its
/// face value laid into the body. Non-standard dice fall back to a plain box.
fn die_shape(sides: u32, value: u32) -> [String; 4] {
    // Standard polyhedral faces top out at 20, so a 2-wide slot is enough; the
    // fallback box is given a 4-wide slot for the occasional d100 etc.
    let v = format!("{value:^2}");
    match sides {
        4 => [
            "  ╱╲  ".into(),
            " ╱  ╲ ".into(),
            format!("╱ {v} ╲"),
            "‾‾‾‾‾‾".into(),
        ],
        6 => [
            "┌────┐".into(),
            "│    │".into(),
            format!("│ {v} │"),
            "└────┘".into(),
        ],
        8 => [
            "  ╱╲  ".into(),
            " ╱  ╲ ".into(),
            format!(" ╲{v}╱ "),
            "  ╲╱  ".into(),
        ],
        10 => [
            " ╱‾‾╲ ".into(),
            "╱    ╲".into(),
            format!("╲ {v} ╱"),
            "  ╲╱  ".into(),
        ],
        12 => [
            "  ╱╲  ".into(),
            " ╱  ╲ ".into(),
            format!("│ {v} │"),
            "└────┘".into(),
        ],
        20 => [
            " ╱‾‾╲ ".into(),
            format!("│ {v} │"),
            "│    │".into(),
            " ╲__╱ ".into(),
        ],
        _ => {
            let v = format!("{value:^4}");
            [
                "┌────┐".into(),
                "│    │".into(),
                format!("│{v}│"),
                "└────┘".into(),
            ]
        }
    }
}

fn render_results(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(" result ".bold());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Error state takes over the panel.
    if let Some(err) = &app.error {
        let p = Paragraph::new(Line::from(vec![
            Span::styled("⚠ ", Style::default().fg(Color::Red)),
            Span::styled(err.clone(), Style::default().fg(Color::Red)),
        ]));
        frame.render_widget(p, inner);
        return;
    }

    if app.dice.is_empty() {
        let p = Paragraph::new(Span::styled(
            "type a dice expression below and press Enter",
            Style::default().fg(Color::DarkGray),
        ));
        frame.render_widget(p, inner);
        return;
    }

    let settled = app.all_settled();

    // Line 1: one coloured chip per die.
    let mut chips: Vec<Span> = Vec::new();
    for (i, die) in app.dice.iter().enumerate() {
        if i > 0 {
            chips.push(Span::raw(" "));
        }
        let val = if settled { die.final_value } else { die.shown };
        if die.kept {
            let mut style = Style::default().fg(die_color(die.color_idx));
            if settled {
                style = style.add_modifier(Modifier::BOLD);
            }
            chips.push(Span::styled(format!("[{val}]"), style));
        } else {
            // Dropped die: greyed and dimmed so it reads as discarded.
            chips.push(Span::styled(
                format!("[{val}]"),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ));
        }
    }
    if app.modifier != 0 {
        let sign = if app.modifier > 0 { "+" } else { "−" };
        chips.push(Span::styled(
            format!("  {sign}{}", app.modifier.abs()),
            Style::default().fg(Color::Gray),
        ));
    }

    // Line 2: the total.
    let total = if settled { app.total() } else { app.live_total() };
    let total_style = if settled {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let total_line = Line::from(vec![
        Span::styled("  Σ total ", Style::default().fg(Color::Gray)),
        Span::styled(format!(" {total} "), total_style),
        Span::raw("   "),
        Span::styled(
            if settled { "" } else { "(rolling…)" },
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    let p = Paragraph::new(vec![Line::from(chips), total_line]);
    frame.render_widget(p, inner);
}

fn render_help(frame: &mut Frame, area: Rect) {
    let key = |k| Span::styled(k, Style::default().fg(Color::Cyan).bold());
    let help = Line::from(vec![
        Span::styled(" › ", Style::default().fg(Color::Cyan)),
        key("Enter"),
        Span::raw(" roll  "),
        key("?"),
        Span::raw(" help  "),
        key("h"),
        Span::raw(" history  "),
        key("s"),
        Span::raw(" stats  "),
        key("Esc"),
        Span::raw(" quit"),
    ]);
    frame.render_widget(Paragraph::new(help), area);
}

/// One row of the syntax table in the help overlay: an example on the left and
/// its meaning on the right.
fn syntax_row<'a>(example: &'a str, meaning: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(format!("{example:<11}"), Style::default().fg(Color::Cyan)),
        Span::styled(meaning, Style::default().fg(Color::Gray)),
    ])
}

/// A bold yellow section heading line for the overlays.
fn heading(text: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(
        text.into(),
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
    ))
}

/// The italic dim "press … to close" footer shared by every pane.
fn close_hint() -> Line<'static> {
    Line::from(Span::styled(
        "  Esc · q to close",
        Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
    ))
}

/// Draw a centred, bordered panel of `lines` titled `title` over the UI. Sizes
/// itself to its content (capped to the frame), blanks what's behind it, and
/// trims overflow from the bottom so it never spills past its border. Shared by
/// all three pop-out panes.
fn overlay_panel(frame: &mut Frame, area: Rect, title: &str, mut lines: Vec<Line>) {
    let content_h = lines.len() as u16;
    let inner_w = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
    let panel_w = (inner_w + 4).min(area.width); // +4 for borders + side padding
    let panel_h = (content_h + 2).min(area.height); // +2 for top/bottom border

    let rect = centered(panel_w, panel_h, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .padding(Padding::horizontal(1))
        .title(title.to_string().bold());

    let max_lines = block.inner(rect).height as usize;
    if lines.len() > max_lines {
        lines.truncate(max_lines);
    }

    let para = Paragraph::new(lines).block(block).alignment(Alignment::Left);
    frame.render_widget(Clear, rect); // blank whatever's behind the panel
    frame.render_widget(para, rect);
}

/// The dice-notation reference, drawn over the UI when `?` is pressed.
fn render_help_overlay(frame: &mut Frame, area: Rect) {
    let lines = vec![
        heading("Dice"),
        syntax_row("3d6", "three six-sided dice"),
        syntax_row("d20", "one die (count defaults to 1)"),
        syntax_row("d6+d8", "combine different dice"),
        syntax_row("2d20-1", "add or subtract a flat modifier"),
        Line::raw(""),
        heading("Keep / drop"),
        syntax_row("2d20kh1", "advantage — keep the highest 1"),
        syntax_row("2d20kl1", "disadvantage — keep the lowest 1"),
        syntax_row("4d6dl1", "drop the lowest 1 (ability scores)"),
        syntax_row("4d6dh1", "drop the highest 1"),
        Line::raw(""),
        heading("Exploding"),
        syntax_row("3d6!", "a max face rolls another die"),
        syntax_row("d10!>8", "explode on any face over 8"),
        syntax_row("d6!=6", "explode on exactly 6"),
        Line::raw(""),
        heading("Multiply"),
        syntax_row("4d6*2", "double this term's sum"),
        syntax_row("4d6!kh3*2", "modifiers stack, left to right"),
        Line::raw(""),
        Line::from(Span::styled(
            "  Separators: +  -  ,  space  or just write dice next to each other.",
            Style::default().fg(Color::DarkGray),
        )),
        Line::raw(""),
        close_hint(),
    ];
    overlay_panel(frame, area, " 🎲  dice notation ", lines);
}

/// The roll-history pane: recent rolls, newest first.
fn render_history_overlay(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();

    if app.history.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no rolls yet — press Enter to roll some dice",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        // Newest first; show at most what the frame can hold.
        let shown = (area.height as usize).saturating_sub(4).max(1);
        let skipped = app.history.len().saturating_sub(shown);
        for (n, e) in app.history.iter().rev().take(shown).enumerate() {
            let idx = app.history.len() - n; // 1-based, counting from the newest
            let faces = e
                .values
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(" ");
            lines.push(Line::from(vec![
                Span::styled(format!("  {idx:>3}. "), Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{:<12}", e.expr), Style::default().fg(Color::Cyan)),
                Span::styled(format!("[{faces}]  "), Style::default().fg(Color::Gray)),
                Span::styled(
                    format!("= {}", e.total),
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                ),
            ]));
        }
        if skipped > 0 {
            lines.push(Line::from(Span::styled(
                format!("  … and {skipped} older"),
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    lines.push(Line::raw(""));
    lines.push(close_hint());
    overlay_panel(frame, area, " 🎲  history ", lines);
}

/// The statistics pane: theoretical odds for the current expression plus a
/// summary of the rolls actually made this session.
fn render_stats_overlay(frame: &mut Frame, app: &App, area: Rect) {
    let lines = match app.stats() {
        Ok(s) => stats_lines(&s),
        Err(e) => vec![
            Line::from(Span::styled(
                "  can't compute stats — the expression doesn't parse:",
                Style::default().fg(Color::Red),
            )),
            Line::from(Span::styled(
                format!("  {e}"),
                Style::default().fg(Color::Red),
            )),
            Line::raw(""),
            close_hint(),
        ],
    };
    overlay_panel(frame, area, " 🎲  statistics ", lines);
}

/// Lay out the statistics into display lines: a header, the theoretical
/// min/max/mean, a small distribution curve, and the session summary.
fn stats_lines(s: &Stats) -> Vec<Line<'static>> {
    let mut lines = vec![
        heading(format!("Odds for  {}", s.expr)),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("min {}   max {}   avg {:.1}", s.min, s.max, s.mean),
                Style::default().fg(Color::Gray),
            ),
        ]),
        Line::from(Span::styled(
            format!("  (estimated from {} samples)", s.samples),
            Style::default().fg(Color::DarkGray),
        )),
        Line::raw(""),
    ];

    // A little horizontal distribution: one bar per bucket, scaled to the peak.
    if !s.dist.is_empty() {
        let peak = s.dist.iter().map(|b| b.fraction).fold(0.0_f64, f64::max).max(1e-9);
        for b in &s.dist {
            let filled = (b.fraction / peak * 18.0).round() as usize;
            let bar: String = "█".repeat(filled);
            lines.push(Line::from(vec![
                Span::styled(format!("  {:>4} ", b.total), Style::default().fg(Color::DarkGray)),
                Span::styled(bar, Style::default().fg(Color::Cyan)),
                Span::styled(
                    format!(" {:>4.1}%", b.fraction * 100.0),
                    Style::default().fg(Color::Gray),
                ),
            ]));
        }
        lines.push(Line::raw(""));
    }

    // Session summary for this exact expression.
    lines.push(heading("This session"));
    if s.session.count == 0 {
        lines.push(Line::from(Span::styled(
            format!("  no rolls of {} yet ({} rolls total)", s.expr, s.total_rolls),
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            format!(
                "  {} rolls   low {}   high {}   mean {:.1}",
                s.session.count, s.session.min, s.session.max, s.session.mean
            ),
            Style::default().fg(Color::Gray),
        )));
    }

    lines.push(Line::raw(""));
    lines.push(close_hint());
    lines
}

/// A `w`×`h` rectangle centred inside `area` (clamped to fit).
fn centered(w: u16, h: u16, area: Rect) -> Rect {
    let [row] = Layout::vertical([Constraint::Length(h)])
        .flex(Flex::Center)
        .areas(area);
    let [cell] = Layout::horizontal([Constraint::Length(w)])
        .flex(Flex::Center)
        .areas(row);
    cell
}

/// The editable dice expression, with a block cursor.
fn render_input(frame: &mut Frame, app: &App, area: Rect) {
    let p = Paragraph::new(Line::from(vec![
        Span::styled("dice ▸ ", Style::default().fg(Color::Cyan).bold()),
        Span::raw(app.input.clone()),
        Span::styled("█", Style::default().fg(Color::Cyan)),
    ]));
    frame.render_widget(p, area);
}
