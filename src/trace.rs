//! Plain-text rendering of a trace (`rust-viz --trace file.rs`): the same
//! steps the TUI shows, printed one after another — handy for piping,
//! diffing, or a terminal without TUI support.

use crate::event::Step;
use crate::memory::{describe_heap_object, describe_value, owned_heap_ids, HeapView, OwnershipState};

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
    if !step.events.is_empty() {
        out.push_str(" what happened\n");
        for e in &step.events {
            out.push_str(&format!("   • {}\n", e.describe()));
        }
    }
    out.push_str(" stack\n");
    if step.stack.frames.is_empty() {
        out.push_str("   (empty — the program has finished)\n");
    }
    // Innermost frame first, like a real call stack.
    for frame in step.stack.frames.iter().rev() {
        let indent = if frame.is_scope { "     " } else { "   " };
        let header = if frame.is_scope { format!("{{ {} scope }}", frame.name) } else { format!("fn {}", frame.name) };
        out.push_str(&format!("{indent}{header}\n"));
        for b in &frame.bindings {
            if b.name == "<temp>" && b.state == OwnershipState::Moved {
                continue;
            }
            let name = if b.name == "<temp>" { "(temporary)" } else { &b.name };
            let value = if b.state == OwnershipState::Moved { "<moved out>".to_string() } else { describe_value(&step.stack, &step.heap, &b.value) };
            out.push_str(&format!("{indent}  {name} = {value}{}\n", state_label(b)));
        }
    }
    out.push_str(" heap\n");
    let mut any = false;
    for id in 0..step.heap.len() {
        match step.heap.view(id) {
            Some(HeapView::Live(o)) => {
                let owner = owner_of(step, id).map(|n| format!("  ← owned by {n}")).unwrap_or_default();
                out.push_str(&format!("   #{id}: {}{owner}\n", describe_heap_object(o)));
            }
            Some(HeapView::Freed(d)) => out.push_str(&format!("   #{id}: freed (was {d})\n")),
            None => continue,
        }
        any = true;
    }
    if !any {
        out.push_str("   (nothing allocated)\n");
    }
    if step.output.len() > printed {
        for line in step.output[printed..].lines() {
            out.push_str(&format!("   stdout> {line}\n"));
        }
    }
    out
}

/// The live binding that (transitively) owns heap object `id`.
fn owner_of(step: &Step, id: usize) -> Option<String> {
    for f in &step.stack.frames {
        for b in &f.bindings {
            if b.state == OwnershipState::Moved {
                continue;
            }
            let mut ids = Vec::new();
            owned_heap_ids(&step.heap, &b.value, &mut ids);
            if ids.contains(&id) {
                return Some(if b.name == "<temp>" { "(temporary)".into() } else { b.name.clone() });
            }
        }
    }
    None
}

pub fn state_label(b: &crate::memory::Binding) -> String {
    let mut s = match &b.state {
        OwnershipState::Owned => String::new(),
        OwnershipState::Moved => "  (moved)".into(),
        OwnershipState::BorrowedShared(n) => format!("  (borrowed ×{n})"),
        OwnershipState::BorrowedMut => "  (mutably borrowed)".into(),
    };
    if b.alias {
        let own_copy = matches!(&b.value, crate::value::Value::Ref(p, _) if p.path.last() == Some(&crate::value::Proj::Field(b.name.clone())));
        s.push_str(if own_copy {
            "  (captured by value — the closure's own copy, stored inside it)"
        } else {
            "  (captured — this is the outer variable, reached through the closure)"
        });
        return s;
    }
    if b.expired {
        s.push_str("  (borrow ended — not used again)");
    }
    if !b.moved_paths.is_empty() {
        s.push_str("  (partially moved)");
    }
    s
}
