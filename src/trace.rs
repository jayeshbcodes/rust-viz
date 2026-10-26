//! Plain-text rendering of a trace (`rust-viz --trace file.rs`): the same
//! steps the TUI shows, printed one after another — handy for piping,
//! diffing, or a terminal without TUI support.

use crate::event::Step;
use crate::memory::{describe_heap_object, describe_value, HeapView, OwnershipState};

pub fn render(steps: &[Step]) -> String {
    let mut out = String::new();
    let mut printed = 0;
    for (i, step) in steps.iter().enumerate() {
        out.push_str(&render_step(i, step, printed));
        printed = step.output.len();
    }
    out
}

/// One step's block of the trace; `printed` is how much of the program's
/// output earlier steps already showed (so only this step's new output
/// is printed).
pub fn render_step(i: usize, step: &Step, printed: usize) -> String {
    let mut out = String::new();
    let head = if step.line == 0 { "end of program".to_string() } else { format!("line {}: {}", step.line, step.source.trim()) };
    out.push_str(&format!("── step {} · {head}\n", i + 1));
    for e in &step.events {
        out.push_str(&format!("   • {}\n", e.describe()));
    }
    for frame in &step.stack.frames {
        let indent = if frame.is_scope { "     " } else { "   " };
        out.push_str(&format!("{indent}[{}]\n", frame.name));
        for b in &frame.bindings {
            if b.name == "<temp>" && b.state == OwnershipState::Moved {
                continue;
            }
            let name = if b.name == "<temp>" { "(temporary)" } else { &b.name };
            out.push_str(&format!("{indent}  {name} = {}{}\n", describe_value(&step.stack, &step.heap, &b.value), state_label(b)));
        }
    }
    for id in 0..step.heap.len() {
        match step.heap.view(id) {
            Some(HeapView::Live(o)) => out.push_str(&format!("   heap #{id}: {}\n", describe_heap_object(o))),
            Some(HeapView::Freed(d)) => out.push_str(&format!("   heap #{id}: freed (was {d})\n")),
            None => {}
        }
    }
    if step.output.len() > printed {
        for line in step.output[printed..].lines() {
            out.push_str(&format!("   stdout> {line}\n"));
        }
    }
    out
}

pub fn state_label(b: &crate::memory::Binding) -> String {
    let mut s = match &b.state {
        OwnershipState::Owned => String::new(),
        OwnershipState::Moved => "  (moved)".into(),
        OwnershipState::BorrowedShared(n) => format!("  (borrowed ×{n})"),
        OwnershipState::BorrowedMut => "  (mutably borrowed)".into(),
    };
    if b.expired {
        s.push_str("  (borrow ended — not used again)");
    }
    if !b.moved_paths.is_empty() {
        s.push_str("  (partially moved)");
    }
    s
}
