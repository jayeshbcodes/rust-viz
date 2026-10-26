/// Index into the `Heap`'s arena. Stays valid even after the object it
/// points to is freed (lookups on a freed id are an explicit error, not UB)
/// since this is our own model, not real memory.
pub type HeapId = usize;

/// A runtime value as our interpreter understands it. Heap-backed values
/// (`Vec`, `Str`, `Box`) only carry a `HeapId` here — the actual contents
/// live in `Heap`, which is exactly the real-world split between "the
/// Vec/String on the stack" (pointer+len+cap) and "the buffer on the heap".
/// Everything else (arrays, tuples, structs, enums) is stored inline, the
/// same as real Rust. See `NOTES.md` for the fidelity ledger.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    Char(char),
    Unit,
    /// Fixed-size array — stored inline (directly, no `HeapId`), matching
    /// the Rust Reference: `[T; N]` is contiguous and stack-allocated when
    /// local, never behind a pointer. See `NOTES.md`.
    Array(Vec<Value>),
    /// `(a, b, ...)` — inline, like an array with mixed element types.
    Tuple(Vec<Value>),
    /// A user-defined struct, fields stored inline. Tuple structs use
    /// field names `"0"`, `"1"`, …; a unit struct has no fields. `copy` is
    /// whether the struct `#[derive(Copy)]`s — in real Rust a struct is
    /// never `Copy` by default, even if every field is.
    Struct { ty: String, fields: Vec<(String, Value)>, copy: bool },
    /// A user enum (or `Option`) value: which variant, plus that variant's
    /// payload, named the same way as `Struct`'s fields.
    Enum { ty: String, variant: String, fields: Vec<(String, Value)>, copy: bool },
    Vec(HeapId),
    /// An owned, heap-allocated string (`String`).
    Str(HeapId),
    /// `Box<T>` — a single value moved onto the heap.
    Box(HeapId),
    /// A string literal (`&'static str`) — its bytes live in the binary's
    /// read-only data, not the heap, so unlike `Str` this carries its
    /// content directly and needs no `Drop`/heap bookkeeping at all.
    StaticStr(String),
    /// A reference (`&T`/`&mut T`) to a place: a named binding, optionally
    /// narrowed to a field/element/sub-slice of it. A slice (`&s[a..b]`,
    /// `&v[a..b]`) is just a reference whose place ends in `Proj::Range` —
    /// the same "pointer + length into existing data" shape as a real fat
    /// pointer, with no allocation of its own.
    Ref(Place, RefKind),
    /// The `f: &mut fmt::Formatter` handed to a user's `fmt` — index of
    /// the output buffer `write!(f, ..)` appends to. Not real memory the
    /// program owns; it's std's, so it never moves or drops.
    Formatter(usize),
    /// A closure: its captured variables stored inline, like a struct's
    /// fields (that's what a real closure is — an anonymous struct). Each
    /// capture is a reference (`&`/`&mut` borrow of the outer variable) or
    /// the value itself (moved or copied in).
    Closure { id: usize, captures: Vec<(String, Value)>, modes: Vec<Capture>, kind: ClosureKind },
    /// A named function used as a value (`.map(double)`).
    FnItem(String),
    /// An iterator — also an ordinary inline value (`Map<Iter<'_, i32>,
    /// {closure}>` is a struct on the stack), lazy until something
    /// drives it.
    Iter(Box<IterNode>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Capture {
    ByRef,
    ByMut,
    ByValue,
}

/// Which `Fn*` trait the closure implements, decided by what its body
/// does with its captures: only reads → `Fn`; mutates → `FnMut`; moves a
/// captured value out → `FnOnce` (callable only once).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ClosureKind {
    Fn,
    FnMut,
    FnOnce,
}

/// One stage of an iterator pipeline. Leaf sources hold a borrow (as a
/// real `Ref` value, so dropping the iterator ends it) or own the data.
#[derive(Debug, Clone, PartialEq)]
pub enum IterNode {
    /// `a..b` (half-open; `a..=b` is stored as `a..b+1`).
    Range { front: i64, back: i64 },
    /// `.iter()` / `.iter_mut()`: yields `&base[i]` / `&mut base[i]`.
    Slice { base: Place, borrow: Box<Value>, front: usize, back: usize, kind: RefKind },
    /// Owned items: `vec.into_iter()` (owns the Vec's `buffer`), an array's
    /// `into_iter()`, or `.bytes()` (`borrow` holds the string's borrow).
    Owned { items: Vec<Value>, front: usize, back: usize, buffer: Option<HeapId>, borrow: Option<Box<Value>> },
    /// `.chars()` over a borrowed (or static) string.
    Chars { chars: Vec<char>, front: usize, back: usize, borrow: Option<Box<Value>> },
    /// `.split_whitespace()` / `.lines()` / `.split(..)`: `&str` slices of `base`.
    Strs { base: Option<Place>, text: String, ranges: Vec<(usize, usize)>, front: usize, back: usize, borrow: Option<Box<Value>> },
    Map(Box<IterNode>, Box<Value>),
    Filter(Box<IterNode>, Box<Value>),
    FilterMap(Box<IterNode>, Box<Value>),
    TakeWhile(Box<IterNode>, Box<Value>, bool),
    SkipWhile(Box<IterNode>, Box<Value>, bool),
    Enumerate(Box<IterNode>, usize),
    Take(Box<IterNode>, usize),
    Skip(Box<IterNode>, usize),
    StepBy(Box<IterNode>, usize, bool),
    Zip(Box<IterNode>, Box<IterNode>),
    Chain(Box<IterNode>, Box<IterNode>),
    Cloned(Box<IterNode>),
    Copied(Box<IterNode>),
    Rev(Box<IterNode>),
}

impl IterNode {
    /// `Map<Filter<Iter over &v>>`-style name for display.
    pub fn describe(&self) -> String {
        let inner = |n: &IterNode| n.describe();
        match self {
            IterNode::Range { front, back } => format!("Range {front}..{back}"),
            IterNode::Slice { base, front, back, kind, .. } => {
                let amp = if *kind == RefKind::Mutable { "&mut " } else { "&" };
                format!("Iter over {amp}{} [{} left]", base.describe(), back.saturating_sub(*front))
            }
            IterNode::Owned { front, back, buffer, .. } => match buffer {
                Some(id) => format!("IntoIter owning #{id} [{} left]", back.saturating_sub(*front)),
                None => format!("IntoIter [{} left]", back.saturating_sub(*front)),
            },
            IterNode::Chars { front, back, .. } => format!("Chars [{} left]", back.saturating_sub(*front)),
            IterNode::Strs { front, back, .. } => format!("Split [{} left]", back.saturating_sub(*front)),
            IterNode::Map(i, _) => format!("Map<{}>", inner(i)),
            IterNode::Filter(i, _) => format!("Filter<{}>", inner(i)),
            IterNode::FilterMap(i, _) => format!("FilterMap<{}>", inner(i)),
            IterNode::TakeWhile(i, ..) => format!("TakeWhile<{}>", inner(i)),
            IterNode::SkipWhile(i, ..) => format!("SkipWhile<{}>", inner(i)),
            IterNode::Enumerate(i, _) => format!("Enumerate<{}>", inner(i)),
            IterNode::Take(i, n) => format!("Take<{}, {n}>", inner(i)),
            IterNode::Skip(i, n) => format!("Skip<{}, {n}>", inner(i)),
            IterNode::StepBy(i, n, _) => format!("StepBy<{}, {n}>", inner(i)),
            IterNode::Zip(a, b) => format!("Zip<{}, {}>", inner(a), inner(b)),
            IterNode::Chain(a, b) => format!("Chain<{}, {}>", inner(a), inner(b)),
            IterNode::Cloned(i) => format!("Cloned<{}>", inner(i)),
            IterNode::Copied(i) => format!("Copied<{}>", inner(i)),
            IterNode::Rev(i) => format!("Rev<{}>", inner(i)),
        }
    }

    /// Every value this node holds (closures, borrows, owned items) — for
    /// dropping, ownership display, and borrow tracking.
    pub fn held_values(&self) -> Vec<&Value> {
        match self {
            IterNode::Range { .. } => vec![],
            IterNode::Slice { borrow, .. } => vec![borrow],
            IterNode::Owned { items, front, back, borrow, .. } => {
                let mut v: Vec<&Value> = items[*front..*back].iter().collect();
                v.extend(borrow.as_deref());
                v
            }
            IterNode::Chars { borrow, .. } | IterNode::Strs { borrow, .. } => borrow.as_deref().into_iter().collect(),
            IterNode::Map(i, f) | IterNode::Filter(i, f) | IterNode::FilterMap(i, f) | IterNode::TakeWhile(i, f, _) | IterNode::SkipWhile(i, f, _) => {
                let mut v = i.held_values();
                v.push(f);
                v
            }
            IterNode::Enumerate(i, _) | IterNode::Take(i, _) | IterNode::Skip(i, _) | IterNode::StepBy(i, ..) | IterNode::Cloned(i) | IterNode::Copied(i) | IterNode::Rev(i) => i.held_values(),
            IterNode::Zip(a, b) | IterNode::Chain(a, b) => {
                let mut v = a.held_values();
                v.extend(b.held_values());
                v
            }
        }
    }

    /// The heap buffer an `IntoIter` source owns, if any.
    pub fn buffers(&self) -> Vec<HeapId> {
        match self {
            IterNode::Owned { buffer: Some(id), .. } => vec![*id],
            IterNode::Map(i, _) | IterNode::Filter(i, _) | IterNode::FilterMap(i, _) | IterNode::TakeWhile(i, ..) | IterNode::SkipWhile(i, ..)
            | IterNode::Enumerate(i, _) | IterNode::Take(i, _) | IterNode::Skip(i, _) | IterNode::StepBy(i, ..) | IterNode::Cloned(i)
            | IterNode::Copied(i) | IterNode::Rev(i) => i.buffers(),
            IterNode::Zip(a, b) | IterNode::Chain(a, b) => {
                let mut v = a.buffers();
                v.extend(b.buffers());
                v
            }
            _ => vec![],
        }
    }
}

impl Value {
    /// Whether reusing this value (e.g. reading a variable in a new
    /// expression) is a copy rather than a move — mirrors real Rust's
    /// `Copy` trait: plain scalars and `&T` are `Copy`, but `&mut T` is
    /// deliberately not (duplicating a mutable reference would let two
    /// live `&mut`s alias the same data, which is exactly what borrowing
    /// rules forbid), and anything heap-backed here (`Vec`/`Str`/`Box`)
    /// never is.
    pub fn is_copy(&self) -> bool {
        match self {
            Value::Int(_) | Value::Float(_) | Value::Bool(_) | Value::Char(_) | Value::Unit | Value::StaticStr(_) => true,
            Value::Formatter(_) | Value::FnItem(_) => true,
            // A closure is `Copy` exactly when all its captures are (so one
            // that only holds `&` borrows or copied integers is).
            Value::Closure { captures, .. } => captures.iter().all(|(_, v)| v.is_copy()),
            Value::Iter(_) => false,
            Value::Ref(_, RefKind::Shared) => true,
            Value::Ref(_, RefKind::Mutable) => false,
            // `[T; N]`/tuples are `Copy` exactly when every element is.
            Value::Array(items) | Value::Tuple(items) => items.iter().all(Value::is_copy),
            Value::Struct { copy, .. } | Value::Enum { copy, .. } => *copy,
            Value::Vec(_) | Value::Str(_) | Value::Box(_) => false,
        }
    }

    /// Short type name for messages/the TUI.
    pub fn type_name(&self) -> String {
        match self {
            Value::Int(_) => "integer".into(),
            Value::Float(_) => "float".into(),
            Value::Bool(_) => "bool".into(),
            Value::Char(_) => "char".into(),
            Value::Unit => "()".into(),
            Value::Array(items) => format!("[_; {}]", items.len()),
            Value::Tuple(_) => "tuple".into(),
            Value::Struct { ty, .. } | Value::Enum { ty, .. } => ty.clone(),
            Value::Vec(_) => "Vec".into(),
            Value::Str(_) => "String".into(),
            Value::Box(_) => "Box".into(),
            Value::StaticStr(_) => "&str".into(),
            Value::Ref(_, RefKind::Shared) => "&".into(),
            Value::Ref(_, RefKind::Mutable) => "&mut".into(),
            Value::Formatter(_) => "&mut Formatter".into(),
            Value::Closure { .. } => "closure".into(),
            Value::FnItem(n) => format!("fn {n}"),
            Value::Iter(n) => n.describe(),
        }
    }
}

/// What a `Ref` points at: binding `slot` in stack frame `frame` (an index,
/// not a name lookup, so shadowing the name later doesn't redirect an
/// existing reference), then `path` to narrow into it. `name` is kept only
/// for display.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub frame: usize,
    pub slot: usize,
    pub name: String,
    pub path: Vec<Proj>,
    /// A reborrow derived from another live reference (e.g. a `for x in
    /// &mut v` element, or `&mut *r` passed to a call) — it rides on its
    /// parent's borrow and doesn't hold a borrow count of its own.
    pub reborrow: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Proj {
    Field(String),
    Index(usize),
    /// Into a `Box`'s heap allocation.
    Deref,
    /// A sub-slice `[a..b]`.
    Range(usize, usize),
    /// `s.as_bytes()`: view a string's bytes as `[u8]`. Only `Index`/`Range`
    /// may follow it.
    Bytes,
}

impl Place {
    /// `v[0]`, `rect.width`, `*b`, `s[0..5]` — for messages/the TUI.
    pub fn describe(&self) -> String {
        let mut out = if self.name == "<temp>" { "(temporary)".to_string() } else { self.name.clone() };
        for p in &self.path {
            match p {
                Proj::Field(f) => out = format!("{out}.{f}"),
                Proj::Index(i) => out = format!("{out}[{i}]"),
                // Implicit in Rust syntax (`list.1.0` auto-derefs a Box).
                Proj::Deref => {}
                Proj::Range(a, b) => out = format!("{out}[{a}..{b}]"),
                Proj::Bytes => out = format!("{out}.as_bytes()"),
            }
        }
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefKind {
    Shared,
    Mutable,
}
