use crate::memory::{Heap, Stack};
use crate::value::HeapId;

/// One thing that happened during a statement — what the TUI highlights/
/// annotates for a given step. A single statement can produce several
/// (e.g. a move also ends whatever borrow was active, if any).
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Declare { name: String },
    Assign { name: String },
    /// A value moved out of `from` — `from` becomes unusable afterward.
    Move { from: String },
    Mutate { name: String },
    Borrow { name: String, mutable: bool },
    /// The reference held by `name` went away, ending its borrow.
    EndBorrow { name: String },
    /// `name`'s borrow ended early because the reference is never used
    /// again (non-lexical lifetimes) — `name` is still in scope.
    BorrowExpired { name: String, target: String },
    HeapAlloc { id: HeapId, what: String },
    /// A full buffer grew: new, bigger allocation, contents copied over,
    /// old one freed. (Real `realloc` may sometimes grow in place; either
    /// way the capacity jump is the same.)
    HeapRealloc { id: HeapId, old_cap: usize, new_cap: usize },
    HeapFree { id: HeapId },
    Drop { name: String },
    FrameEnter { name: String },
    FrameExit { name: String },
    /// Anything else worth telling the learner (a branch taken, a loop
    /// iteration, a value returned to the caller, …).
    Note(String),
}

impl Event {
    /// Plain-English description for the "what happened" pane.
    pub fn describe(&self) -> String {
        match self {
            Event::Declare { name } => format!("`{name}` declared"),
            Event::Assign { name } => format!("`{name}` assigned"),
            Event::Move { from } => format!("`{from}` moved — ownership left `{from}`, which can't be used anymore"),
            Event::Mutate { name } => format!("`{name}` mutated in place"),
            Event::Borrow { name, mutable: true } => format!("`{name}` mutably borrowed (&mut) — no other access to `{name}` until it ends"),
            Event::Borrow { name, mutable: false } => format!("`{name}` borrowed (&) — read-only while the borrow lives"),
            Event::EndBorrow { name } => format!("reference `{name}` goes away → its borrow ends"),
            Event::BorrowExpired { name, target } => {
                format!("`{name}` is never used again → its borrow of `{target}` ends here (non-lexical lifetimes)")
            }
            Event::HeapAlloc { id, what } => format!("heap: allocated #{id} — {what}"),
            Event::HeapRealloc { id, old_cap, new_cap } => {
                format!("heap: #{id} was full (cap {old_cap}) → reallocated with cap {new_cap}, contents copied over")
            }
            Event::HeapFree { id } => format!("heap: freed #{id}"),
            Event::Drop { name } => format!("`{name}` goes out of scope → dropped"),
            Event::FrameEnter { name } => format!("call `{name}` → new stack frame pushed"),
            Event::FrameExit { name } => format!("`{name}` returns → its stack frame is popped"),
            Event::Note(s) => s.clone(),
        }
    }

    /// A few-word label for inline editor annotations, or `None` when the
    /// event is already conveyed by showing the binding's new value.
    pub fn short(&self) -> Option<String> {
        Some(match self {
            Event::Declare { .. } | Event::Assign { .. } | Event::Mutate { .. } => return None,
            Event::Move { from } => format!("moved {from}"),
            Event::Borrow { name, mutable: true } => format!("&mut {name}"),
            Event::Borrow { name, mutable: false } => format!("&{name}"),
            Event::EndBorrow { name } => format!("{name}'s borrow ends"),
            Event::BorrowExpired { name, .. } => format!("{name}'s borrow ends (last use)"),
            Event::HeapAlloc { id, .. } => format!("alloc #{id}"),
            Event::HeapRealloc { id, old_cap, new_cap } => format!("realloc #{id} cap {old_cap}→{new_cap}"),
            Event::HeapFree { id } => format!("free #{id}"),
            Event::Drop { name } => format!("drop {name}"),
            Event::FrameEnter { name } if name.starts_with("closure ") => format!("→ {name}"),
            Event::FrameEnter { name } => format!("→ {name}()"),
            // A closure's return is implied by its call on the same line.
            Event::FrameExit { name } if name.starts_with("closure ") => return None,
            Event::FrameExit { name } => format!("← {name} returns"),
            Event::Note(n) if n.starts_with('✗') => n.clone(),
            Event::Note(_) => return None,
        })
    }

    /// Binding names this event is about — what the TUI highlights.
    pub fn names(&self) -> Vec<&str> {
        match self {
            Event::Declare { name }
            | Event::Assign { name }
            | Event::Mutate { name }
            | Event::Borrow { name, .. }
            | Event::EndBorrow { name }
            | Event::Drop { name } => vec![name],
            Event::Move { from } => vec![from],
            Event::BorrowExpired { name, target } => vec![name, target],
            _ => vec![],
        }
    }

    pub fn heap_id(&self) -> Option<HeapId> {
        match self {
            Event::HeapAlloc { id, .. } | Event::HeapRealloc { id, .. } | Event::HeapFree { id } => Some(*id),
            _ => None,
        }
    }
}

/// One step of the precomputed execution trace: which source line ran,
/// what happened, and a full snapshot of memory *after* that line. The TUI
/// just indexes into `Vec<Step>` — no re-running the interpreter to step
/// backward.
#[derive(Debug, Clone)]
pub struct Step {
    pub line: usize,
    pub source: String,
    pub events: Vec<Event>,
    pub stack: Stack,
    pub heap: Heap,
    /// Everything `println!`/`print!` had produced by this point — a
    /// snapshot, like `stack`/`heap`, so output appears incrementally as
    /// you step forward instead of all at once.
    pub output: String,
}
