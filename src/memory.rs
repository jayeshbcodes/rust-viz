use std::fmt;

use crate::value::{HeapId, Place, Proj, RefKind, Value};

/// Tracks what's currently allowed to happen to a binding — this *is* the
/// borrow checker's rule set, just enforced at our own runtime instead of
/// at compile time: at most one mutable access, or any number of shared
/// ones, never both at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnershipState {
    Owned,
    Moved,
    BorrowedShared(u32),
    BorrowedMut,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryError {
    UseAfterMove(String),
    MoveWhileBorrowed(String),
    BorrowConflict(String),
    AssignToImmutable(String),
    UnknownVariable(String),
    UseOfFreedHeapObject(HeapId),
    /// Reading `x` while a `&mut x` is still live.
    UseWhileMutablyBorrowed(String),
    /// Assigning to/mutating `x` directly while any borrow of it is live.
    AssignWhileBorrowed(String),
    /// A reference whose target binding no longer exists.
    Dangling(String),
    IndexOutOfBounds { len: usize, index: usize },
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            MemoryError::UseAfterMove(n) => write!(f, "use of moved value `{n}` — ownership moved away earlier, so `{n}` can't be used anymore (E0382)"),
            MemoryError::MoveWhileBorrowed(n) => write!(f, "cannot move out of `{n}` because it is borrowed (E0505)"),
            MemoryError::BorrowConflict(n) => write!(f, "cannot borrow `{n}`: it's already borrowed in a conflicting way — one `&mut` XOR any number of `&` (E0499/E0502)"),
            MemoryError::AssignToImmutable(n) => write!(f, "cannot assign to/mutate immutable variable `{n}` — declare it `let mut {n}` (E0384/E0596)"),
            MemoryError::UnknownVariable(n) => write!(f, "cannot find value `{n}` in this scope (E0425)"),
            MemoryError::UseOfFreedHeapObject(id) => write!(f, "heap object #{id} was already freed"),
            MemoryError::UseWhileMutablyBorrowed(n) => write!(f, "cannot use `{n}` because it is mutably borrowed (E0503)"),
            MemoryError::AssignWhileBorrowed(n) => write!(f, "cannot assign to/mutate `{n}` because it is borrowed (E0506)"),
            MemoryError::Dangling(n) => write!(f, "reference to `{n}`, which no longer exists (dangling)"),
            MemoryError::IndexOutOfBounds { len, index } => write!(f, "index out of bounds: the len is {len} but the index is {index}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Binding {
    pub name: String,
    pub value: Value,
    pub mutable: bool,
    pub state: OwnershipState,
    /// For a reference binding: its borrow already ended because it's never
    /// used again (non-lexical lifetimes), even though the name is still in
    /// scope. Dropping it later must not end the borrow a second time.
    pub expired: bool,
    /// Declared with `let x;` and no initializer — the first assignment is
    /// allowed even without `mut`, exactly like real Rust's deferred init.
    pub deferred: bool,
    /// Fields moved out of this binding (`let n = p.name;`) — a *partial*
    /// move: those paths are unusable and won't be dropped again, but the
    /// rest of the value still is usable and still gets dropped.
    pub moved_paths: Vec<Vec<Proj>>,
}

impl Binding {
    fn new(name: impl Into<String>, value: Value, mutable: bool) -> Self {
        Binding { name: name.into(), value, mutable, state: OwnershipState::Owned, expired: false, deferred: false, moved_paths: Vec::new() }
    }

    pub fn mark_moved(&mut self) -> Result<(), MemoryError> {
        match self.state {
            OwnershipState::Moved => Err(MemoryError::UseAfterMove(self.name.clone())),
            OwnershipState::BorrowedShared(_) | OwnershipState::BorrowedMut => {
                Err(MemoryError::MoveWhileBorrowed(self.name.clone()))
            }
            OwnershipState::Owned => {
                self.state = OwnershipState::Moved;
                Ok(())
            }
        }
    }

    pub fn borrow_shared(&mut self) -> Result<(), MemoryError> {
        match self.state {
            OwnershipState::Moved => Err(MemoryError::UseAfterMove(self.name.clone())),
            OwnershipState::BorrowedMut => Err(MemoryError::BorrowConflict(self.name.clone())),
            OwnershipState::BorrowedShared(n) => {
                self.state = OwnershipState::BorrowedShared(n + 1);
                Ok(())
            }
            OwnershipState::Owned => {
                self.state = OwnershipState::BorrowedShared(1);
                Ok(())
            }
        }
    }

    pub fn borrow_mut(&mut self) -> Result<(), MemoryError> {
        match self.state {
            OwnershipState::Moved => Err(MemoryError::UseAfterMove(self.name.clone())),
            OwnershipState::BorrowedShared(_) | OwnershipState::BorrowedMut => {
                Err(MemoryError::BorrowConflict(self.name.clone()))
            }
            OwnershipState::Owned => {
                self.state = OwnershipState::BorrowedMut;
                Ok(())
            }
        }
    }

    pub fn end_borrow_shared(&mut self) {
        if let OwnershipState::BorrowedShared(n) = self.state {
            self.state = if n <= 1 { OwnershipState::Owned } else { OwnershipState::BorrowedShared(n - 1) };
        }
    }

    pub fn end_borrow_mut(&mut self) {
        if self.state == OwnershipState::BorrowedMut {
            self.state = OwnershipState::Owned;
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Frame {
    pub name: String,
    pub bindings: Vec<Binding>,
    /// A block scope (`if`/loop body/`{ }`) inside a function, rather than
    /// a function's own frame. Real Rust doesn't push a new stack frame for
    /// a block — its locals live in the enclosing function's frame — but a
    /// block *does* end its locals' lifetimes (and drop them) at `}`. So a
    /// scope is drawn nested inside its function, and name lookup sees
    /// through it to the function's other locals, but never past the
    /// function frame into the caller.
    pub is_scope: bool,
}

impl Frame {
    pub fn new(name: impl Into<String>) -> Self {
        Frame { name: name.into(), bindings: Vec::new(), is_scope: false }
    }

    pub fn scope(name: impl Into<String>) -> Self {
        Frame { name: name.into(), bindings: Vec::new(), is_scope: true }
    }

    /// Declares (or shadows) a binding, returning its slot. Shadowing just
    /// appends — `get` always finds the most recent one, same as how
    /// `let x = x + 1;` makes the old `x` unreachable by name without
    /// actually removing (or dropping) it.
    pub fn declare(&mut self, name: impl Into<String>, value: Value, mutable: bool) -> usize {
        self.bindings.push(Binding::new(name, value, mutable));
        self.bindings.len() - 1
    }

    pub fn get(&self, name: &str) -> Option<&Binding> {
        self.bindings.iter().rev().find(|b| b.name == name)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Binding> {
        self.bindings.iter_mut().rev().find(|b| b.name == name)
    }

    fn slot_of(&self, name: &str) -> Option<usize> {
        self.bindings.iter().rposition(|b| b.name == name)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum HeapObject {
    /// A `Vec`'s buffer. `cap` is how many elements fit before the next
    /// push has to reallocate — modeled on real `RawVec` growth (verified
    /// against `rustc`'s `.capacity()`, see `NOTES.md`). `cap == 0` means no
    /// buffer has actually been allocated yet (`Vec::new()` doesn't).
    Vec { items: Vec<Value>, cap: usize },
    /// A `String`'s byte buffer; same `cap` rules as `Vec<u8>`.
    Str { s: String, cap: usize },
    Box(Value),
}

impl HeapObject {
    /// Whether real Rust would have an actual heap allocation behind this.
    pub fn is_allocated(&self) -> bool {
        match self {
            HeapObject::Vec { cap, .. } | HeapObject::Str { cap, .. } => *cap > 0,
            HeapObject::Box(_) => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Slot {
    Live(HeapObject),
    /// Freed, with a short description of what it was so the TUI can still
    /// show "#3 freed (was String "hi")" instead of a bare gap.
    Freed(String),
    /// Dropped without ever having had a real allocation (`cap == 0`).
    NeverAllocated,
}

#[derive(Debug, Clone, Default)]
pub struct Heap {
    // A freed slot stays in place — the id stays valid to look up (as an
    // error) rather than silently reused, so a stale reference is
    // detectable instead of aliasing a new unrelated object.
    objects: Vec<Slot>,
}

impl Heap {
    pub fn alloc(&mut self, object: HeapObject) -> HeapId {
        self.objects.push(Slot::Live(object));
        self.objects.len() - 1
    }

    pub fn get(&self, id: HeapId) -> Result<&HeapObject, MemoryError> {
        match self.objects.get(id) {
            Some(Slot::Live(o)) => Ok(o),
            _ => Err(MemoryError::UseOfFreedHeapObject(id)),
        }
    }

    pub fn get_mut(&mut self, id: HeapId) -> Result<&mut HeapObject, MemoryError> {
        match self.objects.get_mut(id) {
            Some(Slot::Live(o)) => Ok(o),
            _ => Err(MemoryError::UseOfFreedHeapObject(id)),
        }
    }

    /// Frees `id`, returning whether a real allocation was released (false
    /// for a `cap == 0` buffer that never allocated, or an already-freed id).
    pub fn free(&mut self, id: HeapId) -> bool {
        let Some(slot) = self.objects.get_mut(id) else { return false };
        let Slot::Live(obj) = slot else { return false };
        if obj.is_allocated() {
            *slot = Slot::Freed(describe_heap_object(obj));
            true
        } else {
            *slot = Slot::NeverAllocated;
            false
        }
    }

    pub fn is_freed(&self, id: HeapId) -> bool {
        !matches!(self.objects.get(id), Some(Slot::Live(_)))
    }

    /// What the TUI should show for this id: `None` if there's nothing
    /// worth drawing (never had a real allocation).
    pub fn view(&self, id: HeapId) -> Option<HeapView<'_>> {
        match self.objects.get(id)? {
            Slot::Live(o) if o.is_allocated() => Some(HeapView::Live(o)),
            Slot::Live(_) | Slot::NeverAllocated => None,
            Slot::Freed(desc) => Some(HeapView::Freed(desc)),
        }
    }

    /// Total number of ids ever allocated (freed or not) — the TUI iterates
    /// `0..heap.len()` to show every heap object, freed ones included (so
    /// a freed slot is visibly "freed," not just absent).
    pub fn len(&self) -> usize {
        self.objects.len()
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    /// Real allocations currently live — "how much is on the heap right now".
    pub fn live_count(&self) -> usize {
        self.objects.iter().filter(|s| matches!(s, Slot::Live(o) if o.is_allocated())).count()
    }
}

pub enum HeapView<'a> {
    Live(&'a HeapObject),
    Freed(&'a str),
}

/// A short label for a heap object by kind — used where we just need "what
/// is this" (the TUI's heap pane) without recursively rendering contained
/// values the way `display_value` does for a full `Value`.
pub fn describe_heap_object(obj: &HeapObject) -> String {
    match obj {
        HeapObject::Str { s, cap } => format!("String buffer {s:?}  len {} cap {cap}", s.len()),
        HeapObject::Vec { items, cap } => format!("Vec buffer, {} item(s), cap {cap}", items.len()),
        HeapObject::Box(_) => "Box allocation".to_string(),
    }
}

#[derive(Debug, Clone, Default)]
pub struct Stack {
    pub frames: Vec<Frame>,
}

impl Stack {
    pub fn push_frame(&mut self, name: impl Into<String>) {
        self.frames.push(Frame::new(name));
    }

    pub fn push_scope(&mut self, name: impl Into<String>) {
        self.frames.push(Frame::scope(name));
    }

    /// Pops and returns the top frame so the caller (the interpreter) can
    /// decide what to do with each binding still owning heap data — that's
    /// where `Drop` events get produced, not here.
    pub fn pop_frame(&mut self) -> Option<Frame> {
        self.frames.pop()
    }

    pub fn current_frame_mut(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("stack must have at least one frame")
    }

    /// Indices of frames whose locals are nameable from the current point:
    /// the top frame and enclosing block scopes, down to and including the
    /// current function's own frame — never the caller's.
    fn visible(&self) -> impl Iterator<Item = usize> + '_ {
        let mut done = false;
        (0..self.frames.len()).rev().take_while(move |&i| {
            if done {
                return false;
            }
            done = !self.frames[i].is_scope;
            true
        })
    }

    /// Looks up a name starting from the innermost (top) frame outward —
    /// a block-local binding shadows an outer one with the same name.
    pub fn lookup(&self, name: &str) -> Option<&Binding> {
        let (f, s) = self.lookup_slot(name)?;
        Some(&self.frames[f].bindings[s])
    }

    pub fn lookup_mut(&mut self, name: &str) -> Option<&mut Binding> {
        let (f, s) = self.lookup_slot(name)?;
        Some(&mut self.frames[f].bindings[s])
    }

    /// `(frame index, slot)` a name currently resolves to — needed when
    /// creating a `Ref`/`Place`, since the reference must record *where*
    /// it's pointing, not just the name.
    pub fn lookup_slot(&self, name: &str) -> Option<(usize, usize)> {
        self.visible().find_map(|i| self.frames[i].slot_of(name).map(|s| (i, s)))
    }

    pub fn binding(&self, frame: usize, slot: usize) -> Option<&Binding> {
        self.frames.get(frame)?.bindings.get(slot)
    }

    pub fn binding_mut(&mut self, frame: usize, slot: usize) -> Option<&mut Binding> {
        self.frames.get_mut(frame)?.bindings.get_mut(slot)
    }
}

/// What a place resolves to — a whole value, or a sub-slice of one.
pub enum Resolved<'a> {
    Val(&'a Value),
    Str(&'a str),
    Slice(&'a [Value]),
}

/// Follows `place` through the stack (and into the heap for `Vec` elements
/// / `Box` contents) to the data it names.
pub fn resolve<'a>(stack: &'a Stack, heap: &'a Heap, place: &Place) -> Result<Resolved<'a>, MemoryError> {
    let b = stack.binding(place.frame, place.slot).ok_or_else(|| MemoryError::Dangling(place.name.clone()))?;
    let mut cur = &b.value;
    for proj in &place.path {
        cur = match (proj, cur) {
            (Proj::Field(f), Value::Struct { fields, .. } | Value::Enum { fields, .. }) => {
                &fields.iter().find(|(n, _)| n == f).ok_or_else(|| MemoryError::Dangling(place.describe()))?.1
            }
            (Proj::Field(f), Value::Tuple(items)) => {
                let i: usize = f.parse().map_err(|_| MemoryError::Dangling(place.describe()))?;
                items.get(i).ok_or_else(|| MemoryError::Dangling(place.describe()))?
            }
            (Proj::Index(i), Value::Array(items)) => {
                items.get(*i).ok_or(MemoryError::IndexOutOfBounds { len: items.len(), index: *i })?
            }
            (Proj::Index(i), Value::Vec(id)) => match heap.get(*id)? {
                HeapObject::Vec { items, .. } => items.get(*i).ok_or(MemoryError::IndexOutOfBounds { len: items.len(), index: *i })?,
                _ => return Err(MemoryError::Dangling(place.describe())),
            },
            (Proj::Deref, Value::Box(id)) => match heap.get(*id)? {
                HeapObject::Box(v) => v,
                _ => return Err(MemoryError::Dangling(place.describe())),
            },
            (Proj::Range(a, b), v) => {
                let out_of_range = |len| MemoryError::IndexOutOfBounds { len, index: *b };
                return match v {
                    Value::StaticStr(s) => s.get(*a..*b).map(Resolved::Str).ok_or(out_of_range(s.len())),
                    Value::Str(id) => match heap.get(*id)? {
                        HeapObject::Str { s, .. } => s.get(*a..*b).map(Resolved::Str).ok_or(out_of_range(s.len())),
                        _ => Err(MemoryError::Dangling(place.describe())),
                    },
                    Value::Array(items) => items.get(*a..*b).map(Resolved::Slice).ok_or(out_of_range(items.len())),
                    Value::Vec(id) => match heap.get(*id)? {
                        HeapObject::Vec { items, .. } => items.get(*a..*b).map(Resolved::Slice).ok_or(out_of_range(items.len())),
                        _ => Err(MemoryError::Dangling(place.describe())),
                    },
                    _ => Err(MemoryError::Dangling(place.describe())),
                };
            }
            _ => return Err(MemoryError::Dangling(place.describe())),
        };
    }
    Ok(Resolved::Val(cur))
}

/// Mutable counterpart of `resolve` (no trailing `Range` — mutating through
/// a sub-slice isn't modeled). Walks once to find the last hop into the
/// heap, then walks mutably from there, so the stack and heap are never
/// both mutably borrowed at once.
pub fn resolve_mut<'a>(stack: &'a mut Stack, heap: &'a mut Heap, place: &Place) -> Result<&'a mut Value, MemoryError> {
    let dangling = || MemoryError::Dangling(place.describe());
    // Find the last projection that enters the heap.
    let mut heap_root: Option<(HeapId, usize)> = None;
    {
        let b = stack.binding(place.frame, place.slot).ok_or_else(dangling)?;
        let mut cur = &b.value;
        for (i, proj) in place.path.iter().enumerate() {
            let mut partial = place.clone();
            partial.path.truncate(i + 1);
            if let (Proj::Index(_), Value::Vec(id)) | (Proj::Deref, Value::Box(id)) = (proj, cur) {
                heap_root = Some((*id, i));
            }
            cur = match resolve(stack, heap, &partial)? {
                Resolved::Val(v) => v,
                _ => return Err(dangling()),
            };
        }
    }
    let (mut cur, rest) = match heap_root {
        None => (&mut stack.binding_mut(place.frame, place.slot).ok_or_else(dangling)?.value, &place.path[..]),
        Some((id, i)) => {
            let v = match (heap.get_mut(id)?, &place.path[i]) {
                (HeapObject::Vec { items, .. }, Proj::Index(n)) => {
                    let len = items.len();
                    items.get_mut(*n).ok_or(MemoryError::IndexOutOfBounds { len, index: *n })?
                }
                (HeapObject::Box(v), Proj::Deref) => v,
                _ => return Err(dangling()),
            };
            (v, &place.path[i + 1..])
        }
    };
    for proj in rest {
        cur = match (proj, cur) {
            (Proj::Field(f), Value::Struct { fields, .. } | Value::Enum { fields, .. }) => {
                &mut fields.iter_mut().find(|(n, _)| n == f).ok_or_else(dangling)?.1
            }
            (Proj::Field(f), Value::Tuple(items)) => items.get_mut(f.parse::<usize>().map_err(|_| dangling())?).ok_or_else(dangling)?,
            (Proj::Index(i), Value::Array(items)) => {
                let len = items.len();
                items.get_mut(*i).ok_or(MemoryError::IndexOutOfBounds { len, index: *i })?
            }
            _ => return Err(dangling()),
        };
    }
    Ok(cur)
}

/// Renders a `Value` the way `{}` (`Display`) would print it — shared by
/// the interpreter (for `println!`/`print!`) and the TUI. Takes `&Stack`
/// as well as `&Heap` because a `Ref` has to be resolved through the stack
/// to show what it actually points at — real Rust's `Display` for `&T`
/// shows the referent's value, not the reference itself. (Whether a type
/// implements `Display` at all is checked by the interpreter, not here.)
pub fn display_value(stack: &Stack, heap: &Heap, v: &Value) -> String {
    match v {
        Value::Int(i) => i.to_string(),
        Value::Float(x) => x.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Char(c) => c.to_string(),
        Value::Str(id) => match heap.get(*id) {
            Ok(HeapObject::Str { s, .. }) => s.clone(),
            _ => "<freed>".to_string(),
        },
        Value::StaticStr(s) => s.clone(),
        Value::Box(id) => match heap.get(*id) {
            Ok(HeapObject::Box(inner)) => display_value(stack, heap, inner),
            _ => "<freed>".to_string(),
        },
        Value::Ref(place, _) => match resolve(stack, heap, place) {
            Ok(Resolved::Val(target)) => display_value(stack, heap, target),
            Ok(Resolved::Str(s)) => s.to_string(),
            Ok(Resolved::Slice(_)) | Err(_) => debug_value(stack, heap, v, false),
        },
        // No `Display` in real Rust for these; the interpreter rejects
        // `{}` on them, so this is only reached from the TUI.
        _ => debug_value(stack, heap, v, false),
    }
}

/// `{:?}` / `{:#?}` rendering. Built on std's own `Debug` builders
/// (`debug_struct`, `debug_list`, …), so spacing, quoting and pretty-print
/// indentation come out exactly like real derived `Debug`.
pub fn debug_value(stack: &Stack, heap: &Heap, v: &Value, pretty: bool) -> String {
    let view = DebugView { stack, heap, v };
    if pretty { format!("{view:#?}") } else { format!("{view:?}") }
}

struct DebugView<'a> {
    stack: &'a Stack,
    heap: &'a Heap,
    v: &'a Value,
}

impl<'a> DebugView<'a> {
    fn with(&self, v: &'a Value) -> DebugView<'a> {
        DebugView { stack: self.stack, heap: self.heap, v }
    }

    fn fields(&self, f: &mut fmt::Formatter, name: &str, fields: &'a [(String, Value)]) -> fmt::Result {
        let Some((first, _)) = fields.first() else { return f.write_str(name) };
        if first.chars().all(|c| c.is_ascii_digit()) {
            let mut t = f.debug_tuple(name);
            for (_, v) in fields {
                t.field(&self.with(v));
            }
            t.finish()
        } else {
            let mut s = f.debug_struct(name);
            for (n, v) in fields {
                s.field(n, &self.with(v));
            }
            s.finish()
        }
    }
}

impl fmt::Debug for DebugView<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.v {
            Value::Int(i) => fmt::Debug::fmt(i, f),
            Value::Float(x) => fmt::Debug::fmt(x, f),
            Value::Bool(b) => fmt::Debug::fmt(b, f),
            Value::Char(c) => fmt::Debug::fmt(c, f),
            Value::Unit => f.write_str("()"),
            Value::Formatter(_) => f.write_str("Formatter"),
            Value::StaticStr(s) => fmt::Debug::fmt(s.as_str(), f),
            Value::Str(id) => match self.heap.get(*id) {
                Ok(HeapObject::Str { s, .. }) => fmt::Debug::fmt(s.as_str(), f),
                _ => f.write_str("<freed>"),
            },
            Value::Array(items) => f.debug_list().entries(items.iter().map(|x| self.with(x))).finish(),
            Value::Vec(id) => match self.heap.get(*id) {
                Ok(HeapObject::Vec { items, .. }) => f.debug_list().entries(items.iter().map(|x| self.with(x))).finish(),
                _ => f.write_str("<freed>"),
            },
            Value::Box(id) => match self.heap.get(*id) {
                Ok(HeapObject::Box(inner)) => fmt::Debug::fmt(&self.with(inner), f),
                _ => f.write_str("<freed>"),
            },
            Value::Tuple(items) => {
                let mut t = f.debug_tuple("");
                for x in items {
                    t.field(&self.with(x));
                }
                t.finish()
            }
            Value::Struct { ty, fields, .. } => self.fields(f, ty, fields),
            Value::Enum { variant, fields, .. } => self.fields(f, variant, fields),
            Value::Ref(place, _) => match resolve(self.stack, self.heap, place) {
                Ok(Resolved::Val(target)) => fmt::Debug::fmt(&self.with(target), f),
                Ok(Resolved::Str(s)) => fmt::Debug::fmt(s, f),
                Ok(Resolved::Slice(items)) => f.debug_list().entries(items.iter().map(|x| self.with(x))).finish(),
                Err(_) => f.write_str("<dangling>"),
            },
        }
    }
}

/// How the TUI shows a value sitting in a stack slot: what's physically in
/// that slot (e.g. a `String`'s ptr/len/cap header), not just what it
/// prints as.
pub fn describe_value(stack: &Stack, heap: &Heap, v: &Value) -> String {
    match v {
        Value::Str(id) => match heap.get(*id) {
            Ok(HeapObject::Str { s, cap }) if *cap == 0 => format!("String {s:?}  [ptr: none, len 0, cap 0 — no heap buffer yet]"),
            Ok(HeapObject::Str { s, cap }) => format!("String {s:?}  [ptr → #{id}, len {}, cap {cap}]", s.len()),
            _ => "String <freed>".into(),
        },
        Value::Vec(id) => match heap.get(*id) {
            Ok(HeapObject::Vec { items, cap }) => {
                let shown = debug_value(stack, heap, v, false);
                if *cap == 0 {
                    format!("Vec {shown}  [ptr: none, len 0, cap 0 — no heap buffer yet]")
                } else {
                    format!("Vec {shown}  [ptr → #{id}, len {}, cap {cap}]", items.len())
                }
            }
            _ => "Vec <freed>".into(),
        },
        Value::Box(id) => format!("Box → #{id} ({})", debug_value(stack, heap, v, false)),
        Value::StaticStr(s) => format!("&str {s:?}  (static: lives in the binary, not the heap)"),
        Value::Formatter(_) => "&mut Formatter (collects the text `write!` produces)".into(),
        Value::Ref(place, kind) => {
            let amp = if *kind == RefKind::Mutable { "&mut " } else { "&" };
            format!("{amp}{} → {}", place.describe(), debug_value(stack, heap, v, false))
        }
        Value::Array(items) => format!("{}  (inline, {} elements)", debug_value(stack, heap, v, false), items.len()),
        Value::Struct { .. } | Value::Enum { .. } | Value::Tuple(_) => format!("{}  (inline)", debug_value(stack, heap, v, false)),
        _ => debug_value(stack, heap, v, false),
    }
}

/// Heap ids directly owned by `v` (not through references) — what dropping
/// `v` would free, and how the heap pane finds each object's owner.
pub fn owned_heap_ids(heap: &Heap, v: &Value, out: &mut Vec<HeapId>) {
    match v {
        Value::Str(id) => out.push(*id),
        Value::Vec(id) => {
            out.push(*id);
            if let Ok(HeapObject::Vec { items, .. }) = heap.get(*id) {
                for x in items {
                    owned_heap_ids(heap, x, out);
                }
            }
        }
        Value::Box(id) => {
            out.push(*id);
            if let Ok(HeapObject::Box(inner)) = heap.get(*id) {
                owned_heap_ids(heap, inner, out);
            }
        }
        Value::Array(items) | Value::Tuple(items) => {
            for x in items {
                owned_heap_ids(heap, x, out);
            }
        }
        Value::Struct { fields, .. } | Value::Enum { fields, .. } => {
            for (_, x) in fields {
                owned_heap_ids(heap, x, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declare_and_lookup_finds_value() {
        let mut stack = Stack::default();
        stack.push_frame("main");
        stack.current_frame_mut().declare("x", Value::Int(5), false);
        assert_eq!(stack.lookup("x").unwrap().value, Value::Int(5));
    }

    #[test]
    fn shadowing_finds_most_recent() {
        let mut frame = Frame::new("main");
        frame.declare("y", Value::Int(5), false);
        frame.declare("y", Value::Int(6), false);
        assert_eq!(frame.get("y").unwrap().value, Value::Int(6));
    }

    #[test]
    fn inner_scope_shadows_outer() {
        let mut stack = Stack::default();
        stack.push_frame("outer");
        stack.current_frame_mut().declare("x", Value::Int(1), false);
        stack.push_scope("inner");
        stack.current_frame_mut().declare("x", Value::Int(2), false);
        assert_eq!(stack.lookup("x").unwrap().value, Value::Int(2));
        stack.pop_frame();
        assert_eq!(stack.lookup("x").unwrap().value, Value::Int(1));
    }

    #[test]
    fn callee_frame_cannot_see_caller_locals() {
        let mut stack = Stack::default();
        stack.push_frame("main");
        stack.current_frame_mut().declare("x", Value::Int(1), false);
        stack.push_frame("helper");
        assert!(stack.lookup("x").is_none());
        stack.push_scope("if");
        assert!(stack.lookup("x").is_none());
    }

    #[test]
    fn move_then_use_is_an_error() {
        let mut b = Binding::new("s", Value::Str(0), false);
        b.mark_moved().unwrap();
        assert_eq!(b.mark_moved(), Err(MemoryError::UseAfterMove("s".into())));
    }

    #[test]
    fn move_while_borrowed_is_an_error() {
        let mut b = Binding::new("s", Value::Str(0), false);
        b.borrow_shared().unwrap();
        assert_eq!(b.mark_moved(), Err(MemoryError::MoveWhileBorrowed("s".into())));
    }

    #[test]
    fn multiple_shared_borrows_are_allowed() {
        let mut b = Binding::new("v", Value::Vec(0), false);
        b.borrow_shared().unwrap();
        b.borrow_shared().unwrap();
        assert_eq!(b.state, OwnershipState::BorrowedShared(2));
        b.end_borrow_shared();
        assert_eq!(b.state, OwnershipState::BorrowedShared(1));
        b.end_borrow_shared();
        assert_eq!(b.state, OwnershipState::Owned);
    }

    #[test]
    fn mutable_borrow_conflicts_with_shared_borrow() {
        let mut b = Binding::new("v", Value::Vec(0), true);
        b.borrow_shared().unwrap();
        assert_eq!(b.borrow_mut(), Err(MemoryError::BorrowConflict("v".into())));
    }

    #[test]
    fn mutable_borrow_conflicts_with_another_mutable_borrow() {
        let mut b = Binding::new("v", Value::Vec(0), true);
        b.borrow_mut().unwrap();
        assert_eq!(b.borrow_mut(), Err(MemoryError::BorrowConflict("v".into())));
        b.end_borrow_mut();
        assert!(b.borrow_mut().is_ok());
    }

    #[test]
    fn heap_alloc_get_and_free() {
        let mut heap = Heap::default();
        let id = heap.alloc(HeapObject::Str { s: String::from("hi"), cap: 2 });
        assert!(matches!(heap.get(id), Ok(HeapObject::Str { s, .. }) if s == "hi"));
        assert!(heap.free(id));
        assert!(heap.is_freed(id));
        assert_eq!(heap.get(id), Err(MemoryError::UseOfFreedHeapObject(id)));
    }

    #[test]
    fn zero_capacity_buffer_frees_nothing() {
        let mut heap = Heap::default();
        let id = heap.alloc(HeapObject::Vec { items: vec![], cap: 0 });
        assert!(heap.view(id).is_none());
        assert!(!heap.free(id));
    }

    #[test]
    fn resolve_mut_reaches_into_vec_element_field() {
        let mut stack = Stack::default();
        let mut heap = Heap::default();
        stack.push_frame("main");
        let p = Value::Struct { ty: "P".into(), fields: vec![("x".into(), Value::Int(1))], copy: false };
        let id = heap.alloc(HeapObject::Vec { items: vec![p], cap: 4 });
        stack.current_frame_mut().declare("v", Value::Vec(id), true);
        let place = Place { frame: 0, slot: 0, name: "v".into(), path: vec![Proj::Index(0), Proj::Field("x".into())], reborrow: false };
        *resolve_mut(&mut stack, &mut heap, &place).unwrap() = Value::Int(9);
        assert!(matches!(resolve(&stack, &heap, &place).unwrap(), Resolved::Val(Value::Int(9))));
    }

    #[test]
    fn debug_matches_derived_debug_formatting() {
        let stack = Stack::default();
        let heap = Heap::default();
        let r = Value::Struct { ty: "Rect".into(), fields: vec![("w".into(), Value::Int(1)), ("h".into(), Value::Int(2))], copy: false };
        assert_eq!(debug_value(&stack, &heap, &r, false), "Rect { w: 1, h: 2 }");
        assert_eq!(debug_value(&stack, &heap, &r, true), "Rect {\n    w: 1,\n    h: 2,\n}");
        assert_eq!(debug_value(&stack, &heap, &Value::Tuple(vec![Value::Int(1)]), false), "(1,)");
    }
}
