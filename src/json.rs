//! `rust-viz --json`: the trace in a machine-readable form, for editor
//! integrations (see `emacs/rust-viz.el`). Hand-written rather than via
//! serde — the shape is small and fixed:
//!
//! ```text
//! { "version": 1,
//!   "error": null | "message",
//!   "output": "everything the program printed",
//!   "steps": [ { "line": 3,                       // 0 = end of program
//!                "events": ["full sentence", ...],
//!                "short": ["alloc #0", ...],     // inline labels
//!                "changed": [{"name": "s", "value": "String \"hi\" [...]"}],
//!                "depth": 1,                     // call depth this step runs at
//!                "output_len": 12,               // bytes of output so far
//!                "detail": "multi-line stack/heap/events block" } ] }
//! ```

use crate::event::{Event, Step};
use crate::memory::describe_value;
use crate::trace::render_step;

pub fn render(steps: &[Step], output: &str, error: Option<&str>) -> String {
    let mut out = String::from("{\"version\":1,\"error\":");
    match error {
        Some(e) => out.push_str(&string(e)),
        None => out.push_str("null"),
    }
    out.push_str(",\"output\":");
    out.push_str(&string(output));
    out.push_str(",\"steps\":[");
    let mut printed = 0;
    for (i, step) in steps.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let events: Vec<String> = step.events.iter().map(|e| string(&e.describe())).collect();
        let short: Vec<String> = step.events.iter().filter_map(Event::short).map(|s| string(&s)).collect();
        out.push_str(&format!(
            "{{\"line\":{},\"depth\":{},\"events\":[{}],\"short\":[{}],\"changed\":[{}],\"output_len\":{},\"detail\":{}}}",
            step.line,
            depth(step),
            events.join(","),
            short.join(","),
            changed(step).join(","),
            step.output.len(),
            string(&render_step(i, step, printed)),
        ));
        printed = step.output.len();
    }
    out.push_str("]}\n");
    out
}

/// Function frames on the stack. A function's closing-`}` step is
/// recorded just after its frame is popped, but it belongs to the callee,
/// so it counts at the callee's depth.
fn depth(step: &Step) -> usize {
    let exits = step.events.iter().filter(|e| matches!(e, Event::FrameExit { .. })).count();
    step.stack.frames.iter().filter(|f| !f.is_scope).count() + exits
}

/// The current value of each binding this step declared/assigned/mutated
/// — what an inline annotation shows next to the line.
fn changed(step: &Step) -> Vec<String> {
    let mut seen = Vec::new();
    for e in &step.events {
        let (Event::Declare { name } | Event::Assign { name } | Event::Mutate { name }) = e else { continue };
        // `p.x` / `v[0]` → show the whole `p` / `v`.
        let root = name.split(['.', '[']).next().unwrap_or(name).to_string();
        if seen.contains(&root) {
            continue;
        }
        if let Some(b) = step.stack.lookup(&root) {
            seen.push(root.clone());
            let v = describe_value(&step.stack, &step.heap, &b.value);
            let v = if b.state == crate::memory::OwnershipState::Moved { "<moved out>".to_string() } else { v };
            seen.push(format!("{{\"name\":{},\"value\":{}}}", string(&root), string(&v)));
        }
    }
    seen.into_iter().filter(|s| s.starts_with('{')).collect()
}

fn string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interp::Interpreter;

    #[test]
    fn escapes_strings() {
        assert_eq!(string("a\"b\\c\nd\u{1}"), r#""a\"b\\c\nd\u0001""#);
    }

    #[test]
    fn renders_a_program() {
        let (interp, r) = Interpreter::run_program("fn main() { let s = String::from(\"hi\"); println!(\"{s}\"); }");
        r.unwrap();
        let j = render(&interp.steps, &interp.output, None);
        assert!(j.starts_with("{\"version\":1,\"error\":null,\"output\":\"hi\\n\""));
        assert!(j.contains("\"short\":[\"alloc #0\"]"));
        assert!(j.contains(r#""changed":[{"name":"s","value":"String \"hi\"  [ptr → #0, len 2, cap 2]"}]"#), "{j}");
    }
}
