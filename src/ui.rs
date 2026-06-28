//! All ratatui rendering lives here.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::{App, Die, DIE_H, DIE_W};

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

    let color = die_color(die.color_idx);
    let style = if die.settled {
        Style::default().fg(color).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(color).add_modifier(Modifier::DIM)
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
        let mut style = Style::default().fg(die_color(die.color_idx));
        if settled {
            style = style.add_modifier(Modifier::BOLD);
        }
        chips.push(Span::styled(format!("[{val}]"), style));
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
    let help = Line::from(vec![
        Span::styled(" › ", Style::default().fg(Color::Cyan)),
        Span::styled("Enter", Style::default().fg(Color::Cyan).bold()),
        Span::raw(" roll  "),
        Span::styled("Esc", Style::default().fg(Color::Cyan).bold()),
        Span::raw(" quit   try: "),
        Span::styled(
            "3d6 · d6+d8 · d6d10 · d6,d12 · 2d20-1",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(Paragraph::new(help), area);
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
