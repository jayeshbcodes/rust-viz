use std::collections::HashSet;
use std::io;

use crossterm::event::{self, Event as CEvent, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::event::{Event, Step};
use crate::interp::{is_ready_to_feed, Interpreter};
use crate::memory::{describe_heap_object, describe_value, owned_heap_ids, HeapView, OwnershipState};
use crate::trace::state_label;

/// Stepper mode: replay an already-finished trace of a whole program,
/// `n`/`p` to move through it, with the program's source alongside.
pub fn run(steps: &[Step], source: &str, path: &str, error: Option<String>) -> io::Result<()> {
    if steps.is_empty() {
        eprintln!("{}", error.unwrap_or_else(|| "program produced no steps".into()));
        return Ok(());
    }
    let source_lines: Vec<&str> = source.lines().collect();
    let mut terminal = ratatui::init();
    let mut index: usize = 0;
    let result = loop {
        terminal.draw(|frame| draw_stepper(frame, steps, index, &source_lines, path, &error))?;

        if let CEvent::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break Ok(()),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break Ok(()),
                KeyCode::Char('n') | KeyCode::Right | KeyCode::Char(' ') | KeyCode::Down => index = (index + 1).min(steps.len() - 1),
                KeyCode::Char('p') | KeyCode::Left | KeyCode::Up => index = index.saturating_sub(1),
                KeyCode::PageDown => index = (index + 10).min(steps.len() - 1),
                KeyCode::PageUp => index = index.saturating_sub(10),
                KeyCode::Char('g') | KeyCode::Home => index = 0,
                KeyCode::Char('G') | KeyCode::End => index = steps.len() - 1,
                _ => {}
            }
        }
    };
    ratatui::restore();
    result
}

/// Live mode: type statements one at a time, Enter runs whatever's
/// complete so far against `interp`'s ongoing state, and the stack/heap/
/// output panes update immediately — no stepping, always showing the
/// latest step.
pub fn run_live(mut interp: Interpreter) -> io::Result<()> {
    let mut terminal = ratatui::init();
    let mut pending = String::new();
    let mut history: Vec<String> = Vec::new();
    let mut error: Option<String> = None;
    // Steps produced by the most recent `feed()` — their events are what
    // the "what happened" pane explains.
    let mut last_from = 0;

    let result = loop {
        terminal.draw(|frame| draw_live(frame, &interp, &history, &pending, &error, last_from))?;

        if let CEvent::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            // Ctrl+C is the only reserved chord — every plain character,
            // including n/p/q, has to remain literal input here, since
            // someone will type a variable or string containing any of
            // those letters.
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                interp.drop_top_frame();
                break Ok(());
            }
            match key.code {
                KeyCode::Esc => {
                    pending.clear();
                    error = None;
                }
                KeyCode::Enter => {
                    pending.push('\n');
                    if is_ready_to_feed(&pending) {
                        let before = interp.steps.len();
                        match interp.feed(&pending) {
                            Ok(()) => {
                                history.push(std::mem::take(&mut pending));
                                error = None;
                                last_from = before;
                            }
                            Err(e) => error = Some(e.to_string()),
                        }
                    }
                }
                KeyCode::Backspace => {
                    pending.pop();
                }
                KeyCode::Char(c) => pending.push(c),
                _ => {}
            }
        }
    };
    ratatui::restore();
    result
}

fn draw_stepper(frame: &mut Frame, steps: &[Step], index: usize, source: &[&str], path: &str, error: &Option<String>) {
    let step = &steps[index];
    let rows = Layout::new(Direction::Vertical, [Constraint::Min(8), Constraint::Length(9), Constraint::Length(1)]).split(frame.area());
    let cols = Layout::new(Direction::Horizontal, [Constraint::Percentage(45), Constraint::Percentage(55)]).split(rows[0]);

    // Source pane, current line highlighted and kept in view.
    let mut lines = Vec::new();
    let mut current = 0;
    for (i, l) in source.iter().enumerate() {
        let n = i + 1;
        if n == step.line {
            current = i;
            lines.push(Line::from(vec![
                Span::styled(format!("▶{n:>3} "), Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                Span::styled(l.to_string(), Style::default().add_modifier(Modifier::BOLD).bg(Color::DarkGray)),
            ]));
        } else {
            lines.push(Line::from(vec![Span::styled(format!(" {n:>3} "), Style::default().fg(Color::DarkGray)), Span::raw(l.to_string())]));
        }
    }
    let src_title = format!("{path} — step {}/{}", index + 1, steps.len());
    frame.render_widget(scrolled(lines, Some(current), cols[0], src_title), cols[0]);

    let right = Layout::new(Direction::Vertical, [Constraint::Percentage(55), Constraint::Percentage(45)]).split(cols[1]);
    draw_stack(frame, right[0], step);
    draw_heap(frame, right[1], step);

    let bottom = Layout::new(Direction::Horizontal, [Constraint::Percentage(60), Constraint::Percentage(40)]).split(rows[1]);
    let is_last = index + 1 == steps.len();
    draw_events(frame, bottom[0], &step.events, if is_last { error.as_deref() } else { None });
    draw_output(frame, bottom[1], step);

    frame.render_widget(
        Paragraph::new("n/→/space next   p/← back   PgUp/PgDn ±10   g/G first/last   q quit").style(Style::default().fg(Color::DarkGray)),
        rows[2],
    );
}

fn draw_live(frame: &mut Frame, interp: &Interpreter, history: &[String], pending: &str, error: &Option<String>, last_from: usize) {
    let rows = Layout::new(
        Direction::Vertical,
        [Constraint::Min(8), Constraint::Length(8), Constraint::Length(2), Constraint::Length(1)],
    )
    .split(frame.area());

    let cols = Layout::new(Direction::Horizontal, [Constraint::Percentage(45), Constraint::Percentage(55)]).split(rows[0]);
    draw_input(frame, cols[0], history, pending);

    let right = Layout::new(Direction::Vertical, [Constraint::Percentage(55), Constraint::Percentage(45)]).split(cols[1]);
    let bottom = Layout::new(Direction::Horizontal, [Constraint::Percentage(60), Constraint::Percentage(40)]).split(rows[1]);
    match interp.steps.last() {
        Some(step) => {
            draw_stack(frame, right[0], step);
            draw_heap(frame, right[1], step);
            let events: Vec<Event> = interp.steps[last_from.min(interp.steps.len())..].iter().flat_map(|s| s.events.clone()).collect();
            draw_events(frame, bottom[0], &events, None);
            draw_output(frame, bottom[1], step);
        }
        None => {
            frame.render_widget(empty_pane("stack"), right[0]);
            frame.render_widget(empty_pane("heap"), right[1]);
            frame.render_widget(empty_pane("what happened"), bottom[0]);
            frame.render_widget(empty_pane("program output"), bottom[1]);
        }
    }

    let (status_text, status_style) = match error {
        Some(e) => (e.clone(), Style::default().fg(Color::Red)),
        None => (String::new(), Style::default()),
    };
    frame.render_widget(Paragraph::new(status_text).style(status_style).wrap(Wrap { trim: true }), rows[2]);

    frame.render_widget(
        Paragraph::new("type Rust (statements, fn/struct definitions), Enter runs it once complete (`;` or closing `}`)   Esc clear pending   Ctrl+C quit")
            .style(Style::default().fg(Color::DarkGray)),
        rows[3],
    );
}

/// A bordered pane that scrolls so line `focus` stays visible — so nothing
/// gets silently clipped once content outgrows the pane.
fn scrolled(lines: Vec<Line<'static>>, focus: Option<usize>, area: Rect, title: String) -> Paragraph<'static> {
    let height = area.height.saturating_sub(2) as usize;
    let total = lines.len();
    let offset = match focus {
        Some(f) if total > height => f.saturating_sub(height / 2).min(total - height),
        // No focus: follow the bottom, like a log.
        None if total > height => total - height,
        _ => 0,
    };
    let title = if total > height { format!("{title} ({}-{} of {total})", offset + 1, (offset + height).min(total)) } else { title };
    Paragraph::new(lines).block(Block::new().borders(Borders::ALL).title(title)).scroll((offset as u16, 0))
}

fn empty_pane(title: &str) -> Paragraph<'static> {
    Paragraph::new("(nothing yet)").block(Block::new().borders(Borders::ALL).title(title.to_string()))
}

fn draw_input(frame: &mut Frame, area: Rect, history: &[String], pending: &str) {
    let mut lines: Vec<Line> = Vec::new();
    for chunk in history {
        for line in chunk.lines() {
            lines.push(Line::from(Span::styled(line.to_string(), Style::default().fg(Color::DarkGray))));
        }
    }
    // `split('\n')`, not `lines()`, so a trailing newline shows an empty
    // line with the cursor on it, rather than disappearing.
    let pending_lines: Vec<&str> = pending.split('\n').collect();
    let last_index = pending_lines.len() - 1;
    for (i, line) in pending_lines.into_iter().enumerate() {
        if i == last_index {
            lines.push(Line::from(format!("{line}\u{2588}")));
        } else {
            lines.push(Line::from(line.to_string()));
        }
    }
    frame.render_widget(scrolled(lines, None, area, "input".into()), area);
}

fn draw_stack(frame: &mut Frame, area: Rect, step: &Step) {
    let touched: HashSet<&str> = step.events.iter().flat_map(|e| e.names()).collect();
    let mut lines: Vec<Line> = Vec::new();
    let mut focus = None;
    // Innermost (most recent) frame on top, like a real call stack; block
    // scopes are drawn indented inside the function they belong to.
    for frame_data in step.stack.frames.iter().rev() {
        let indent = if frame_data.is_scope { "  " } else { "" };
        let header = if frame_data.is_scope { format!("{indent}{{ {} scope }}", frame_data.name) } else { format!("── fn {} ──", frame_data.name) };
        lines.push(Line::from(Span::styled(header, Style::default().add_modifier(Modifier::BOLD).fg(Color::Cyan))));
        for binding in &frame_data.bindings {
            let hidden = binding.name == "<temp>";
            if hidden && binding.state == OwnershipState::Moved {
                continue;
            }
            let value_text = describe_value(&step.stack, &step.heap, &binding.value);
            let color = match binding.state {
                OwnershipState::Moved => Color::DarkGray,
                OwnershipState::BorrowedMut => Color::Red,
                OwnershipState::BorrowedShared(_) => Color::Yellow,
                OwnershipState::Owned if binding.expired => Color::DarkGray,
                OwnershipState::Owned => Color::Green,
            };
            let is_touched = touched.iter().any(|t| *t == binding.name || t.starts_with(&format!("{}.", binding.name)) || t.starts_with(&format!("{}[", binding.name)));
            if is_touched && focus.is_none() {
                focus = Some(lines.len());
            }
            let mutable = if binding.mutable { "mut " } else { "" };
            let name = if hidden { "(temporary)".to_string() } else { binding.name.clone() };
            let marker = if is_touched { "▸ " } else { "  " };
            let name_style = if is_touched { Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED) } else { Style::default() };
            let value_text = if binding.state == OwnershipState::Moved { "<moved out>".to_string() } else { value_text };
            lines.push(Line::from(vec![
                Span::raw(format!("{indent}{marker}")),
                Span::styled(format!("{mutable}{name}"), name_style),
                Span::raw(" = "),
                Span::styled(value_text, Style::default().fg(color)),
                Span::styled(state_label(binding), Style::default().fg(Color::DarkGray)),
            ]));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from("(stack is empty)"));
    }
    frame.render_widget(scrolled(lines, focus, area, "stack".into()).wrap(Wrap { trim: false }), area);
}

fn draw_heap(frame: &mut Frame, area: Rect, step: &Step) {
    let touched: HashSet<usize> = step.events.iter().filter_map(Event::heap_id).collect();
    // Who owns each heap object right now — found by walking every live
    // binding's value for the heap ids it (transitively) owns.
    let mut owners: Vec<(usize, String)> = Vec::new();
    for f in &step.stack.frames {
        for b in &f.bindings {
            if b.state == OwnershipState::Moved {
                continue;
            }
            let mut ids = Vec::new();
            owned_heap_ids(&step.heap, &b.value, &mut ids);
            let name = if b.name == "<temp>" { "(temporary)".to_string() } else { b.name.clone() };
            owners.extend(ids.into_iter().map(|id| (id, name.clone())));
        }
    }
    let mut lines: Vec<Line> = Vec::new();
    let mut focus = None;
    for id in 0..step.heap.len() {
        let hl = touched.contains(&id);
        if hl && focus.is_none() {
            focus = Some(lines.len());
        }
        let marker = if hl { "▸" } else { " " };
        let bold = if hl { Modifier::BOLD } else { Modifier::empty() };
        match step.heap.view(id) {
            Some(HeapView::Freed(desc)) => lines.push(Line::from(Span::styled(
                format!("{marker}#{id}: freed (was {desc})"),
                Style::default().fg(Color::DarkGray).add_modifier(bold),
            ))),
            Some(HeapView::Live(obj)) => {
                let owner = owners.iter().find(|(o, _)| *o == id).map(|(_, n)| format!("  ← owned by {n}")).unwrap_or_default();
                let contents = match obj {
                    crate::memory::HeapObject::Vec { items, .. } => {
                        let parts: Vec<String> = items.iter().map(|v| crate::memory::debug_value(&step.stack, &step.heap, v, false)).collect();
                        format!("  [{}]", parts.join(", "))
                    }
                    crate::memory::HeapObject::Box(v) => format!("  {}", crate::memory::debug_value(&step.stack, &step.heap, v, false)),
                    _ => String::new(),
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("{marker}#{id}: {}{contents}", describe_heap_object(obj)), Style::default().fg(Color::Magenta).add_modifier(bold)),
                    Span::styled(owner, Style::default().fg(Color::Green)),
                ]));
            }
            None => {}
        }
    }
    let title = format!("heap ({} live allocation(s))", step.heap.live_count());
    if lines.is_empty() {
        lines.push(Line::from("(nothing on the heap)"));
    }
    frame.render_widget(scrolled(lines, focus, area, title).wrap(Wrap { trim: false }), area);
}

fn draw_events(frame: &mut Frame, area: Rect, events: &[Event], error: Option<&str>) {
    let mut lines: Vec<Line> = events
        .iter()
        .map(|e| {
            let color = match e {
                Event::HeapAlloc { .. } | Event::HeapRealloc { .. } => Color::Magenta,
                Event::HeapFree { .. } | Event::Drop { .. } => Color::LightRed,
                Event::Move { .. } => Color::LightBlue,
                Event::Borrow { .. } | Event::EndBorrow { .. } | Event::BorrowExpired { .. } => Color::Yellow,
                Event::FrameEnter { .. } | Event::FrameExit { .. } => Color::Cyan,
                Event::Note(n) if n.starts_with('✗') => Color::Red,
                _ => Color::White,
            };
            Line::from(Span::styled(format!("• {}", e.describe()), Style::default().fg(color)))
        })
        .collect();
    if let Some(e) = error
        && !events.iter().any(|ev| matches!(ev, Event::Note(n) if n.starts_with('✗')))
    {
        lines.push(Line::from(Span::styled(format!("✗ {e}"), Style::default().fg(Color::Red))));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled("(nothing changed in memory)", Style::default().fg(Color::DarkGray))));
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }).block(Block::new().borders(Borders::ALL).title("what happened")), area);
}

fn draw_output(frame: &mut Frame, area: Rect, step: &Step) {
    let lines: Vec<Line> = if step.output.is_empty() {
        vec![Line::from(Span::styled("(no output yet)", Style::default().fg(Color::DarkGray)))]
    } else {
        step.output.lines().map(|l| Line::from(l.to_string())).collect()
    };
    frame.render_widget(scrolled(lines, None, area, "program output".into()), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// Renders every step of every sample at a roomy and a cramped size —
    /// catches layout/scroll arithmetic panics without a real terminal.
    #[test]
    fn every_sample_step_renders() {
        for entry in std::fs::read_dir("samples").unwrap() {
            let path = entry.unwrap().path();
            let source = std::fs::read_to_string(&path).unwrap();
            let (interp, _) = Interpreter::run_program(&source);
            let lines: Vec<&str> = source.lines().collect();
            for (w, h) in [(140, 45), (40, 12)] {
                let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
                for i in 0..interp.steps.len() {
                    term.draw(|f| draw_stepper(f, &interp.steps, i, &lines, "x.rs", &None)).unwrap();
                }
            }
        }
    }

    #[test]
    #[ignore]
    fn print_snapshot() {
        let source = std::fs::read_to_string("samples/ownership.rs").unwrap();
        let (interp, _) = Interpreter::run_program(&source);
        let lines: Vec<&str> = source.lines().collect();
        let mut term = Terminal::new(TestBackend::new(150, 36)).unwrap();
        let i = interp.steps.iter().position(|s| s.events.iter().any(|e| matches!(e, Event::FrameExit { name } if name == "takes_ownership"))).unwrap();
        term.draw(|f| draw_stepper(f, &interp.steps, i, &lines, "samples/ownership.rs", &None)).unwrap();
        let buf = term.backend().buffer();
        for y in 0..buf.area.height {
            let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect();
            println!("{row}");
        }
    }

    #[test]
    fn live_mode_renders() {
        let mut interp = Interpreter::new();
        interp.feed("let mut v = vec![String::from(\"a\")];").unwrap();
        interp.feed("let r = &v;").unwrap();
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw_live(f, &interp, &["let mut v = ...".into()], "v.pu", &None, 0)).unwrap();
    }
}
