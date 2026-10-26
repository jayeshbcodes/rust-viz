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
    /// A sub-slice `[a..b]` — always the last projection.
    Range(usize, usize),
}

impl Place {
    /// `v[0]`, `rect.width`, `*b`, `s[0..5]` — for messages/the TUI.
    pub fn describe(&self) -> String {
        let mut out = self.name.clone();
        for p in &self.path {
            match p {
                Proj::Field(f) => out = format!("{out}.{f}"),
                Proj::Index(i) => out = format!("{out}[{i}]"),
                // Implicit in Rust syntax (`list.1.0` auto-derefs a Box).
                Proj::Deref => {}
                Proj::Range(a, b) => out = format!("{out}[{a}..{b}]"),
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
