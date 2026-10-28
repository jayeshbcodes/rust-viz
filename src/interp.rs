use std::collections::{HashMap, HashSet};
use std::fmt;
use std::rc::Rc;

use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens;
use syn::spanned::Spanned;
use syn::{BinOp, Block, Expr, ExprLit, FnArg, Item, Lit, Local, Member, Pat, RangeLimits, Stmt, UnOp};

use crate::event::{Event, Step};
use crate::format::{pad, parse_format, Piece, Spec};
use crate::memory::{
    debug_value, display_value, owned_heap_ids, resolve, resolve_mut, Heap, HeapObject, MemoryError, OwnershipState,
    Resolved, Stack,
};
use crate::value::{HeapId, Place, Proj, RefKind, Value};

/// Guards against runaway programs — every step snapshots the whole
/// stack/heap, so an infinite loop would otherwise eat all memory.
const MAX_STEPS: usize = 20_000;
const MAX_CALL_DEPTH: usize = 200;

#[derive(Debug, PartialEq)]
pub enum InterpError {
    Parse(String),
    Memory(MemoryError),
    /// This interpreter doesn't implement this (valid-in-real-Rust) syntax
    /// yet — a gap in us, not a gap in the program.
    Unsupported { what: String, line: usize },
    /// A real `rustc` would reject this at compile time too, before the
    /// program ever runs (e.g. `println!(x)` with a bare variable instead
    /// of a string literal) — not a missing feature, the program is wrong.
    CompileError(String),
    /// A real Rust program would also panic at *runtime* here (e.g.
    /// out-of-bounds indexing) — distinct from `CompileError` (never runs
    /// at all) and from `Unsupported` (a gap in this interpreter).
    Panic(String),
}

impl fmt::Display for InterpError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            InterpError::Parse(e) => write!(f, "parse error: {e}"),
            // Every ownership/borrow violation we catch at runtime is one
            // `rustc` would have caught at compile time.
            InterpError::Memory(e) => write!(f, "rustc would reject this: {e}"),
            InterpError::Unsupported { what, line } => write!(f, "rust-viz doesn't support {what} yet (line {line})"),
            InterpError::CompileError(e) => write!(f, "rustc would reject this: {e}"),
            InterpError::Panic(e) => write!(f, "thread 'main' panicked: {e}"),
        }
    }
}

impl From<MemoryError> for InterpError {
    fn from(e: MemoryError) -> Self {
        match e {
            MemoryError::IndexOutOfBounds { .. } => InterpError::Panic(e.to_string()),
            e => InterpError::Memory(e),
        }
    }
}

/// Non-local control flow travels on the `Err` side so `?` unwinds it
/// through nested expressions; loops/calls catch the variants they own.
enum Flow {
    Err(InterpError),
    Break(Option<String>, Value),
    Continue(Option<String>),
    Return(Value),
}

impl From<InterpError> for Flow {
    fn from(e: InterpError) -> Self {
        Flow::Err(e)
    }
}

impl From<MemoryError> for Flow {
    fn from(e: MemoryError) -> Self {
        Flow::Err(e.into())
    }
}

type R<T> = Result<T, Flow>;

fn compile_err<T>(msg: impl Into<String>) -> R<T> {
    Err(Flow::Err(InterpError::CompileError(msg.into())))
}

fn panic_err<T>(msg: impl Into<String>) -> R<T> {
    Err(Flow::Err(InterpError::Panic(msg.into())))
}

fn flow_to_err(f: Flow) -> InterpError {
    match f {
        Flow::Err(e) => e,
        Flow::Break(..) => InterpError::CompileError("`break` outside of a loop (E0268)".into()),
        Flow::Continue(_) => InterpError::CompileError("`continue` outside of a loop (E0268)".into()),
        Flow::Return(_) => InterpError::CompileError("`return` outside of a function".into()),
    }
}

mod iters;
mod maps;

struct FnDef {
    /// `main`, `area`, `Rect::new` — shown as the frame name.
    name: String,
    sig: syn::Signature,
    block: syn::Block,
    /// The `impl` type this is defined on, for resolving `Self`.
    self_ty: Option<String>,
    /// The module it's defined in, for resolving paths in its body.
    module: Vec<String>,
    /// `line_offset` when the definition was fed (REPL), so lines inside
    /// the body map back to what the user typed.
    line_base: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Shape {
    Named,
    Tuple,
    Unit,
}

struct Derives {
    copy: bool,
    clone: bool,
    debug: bool,
}

struct StructDef {
    shape: Shape,
    fields: Vec<String>,
    derives: Derives,
}

struct EnumDef {
    variants: Vec<(String, Shape, Vec<String>)>,
    derives: Derives,
}

/// "These identifiers still appear in code that hasn't run yet" — the
/// liveness information non-lexical lifetimes are computed from. One per
/// executing block (and one per loop, covering its whole body, since the
/// next iteration runs it again).
struct LiveCtx {
    inv: usize,
    frame: usize,
    idents: HashSet<String>,
}

/// A place expression, resolved: where it lives, and whether we reached it
/// through a reference (and which kind) — access through a reference is
/// governed by that reference, not by the target binding's own state.
#[derive(Clone)]
struct PlaceRef {
    place: Place,
    via: Option<RefKind>,
}

#[derive(Clone, Copy, PartialEq)]
enum Access {
    Read,
    Move,
    Write,
    Shared,
    Mut,
}

/// A pattern binding found during matching, not yet committed (so a match
/// guard that fails, or a later arm, doesn't leave moves behind).
struct PatBind {
    name: String,
    mutable: bool,
    by_ref: Option<RefKind>,
    src: PlaceRef,
}

/// A cloned-out view of what a place resolves to.
enum Got {
    Val(Value),
    Str(String),
    Slice(Vec<Value>),
}

enum IterSrc {
    Ints(Vec<i64>),
    Chars(Vec<char>),
    /// Owned values moved out of a consumed `Vec` (`buffer` gets freed
    /// when the loop ends) or array.
    Owned { items: Vec<Value>, buffer: Option<HeapId> },
    /// `&v` / `&mut v` / `.iter()` — element references into `base`.
    Refs { base: PlaceRef, indices: Vec<usize>, kind: RefKind },
    /// Any other iterator value (adaptor chains, ranges as values, …),
    /// pulled one item at a time.
    Lazy(Box<crate::value::IterNode>),
}

pub struct Interpreter {
    pub stack: Stack,
    pub heap: Heap,
    pub steps: Vec<Step>,
    pub output: String,
    source_lines: Vec<String>,
    /// How many lines were fed in *previous* `feed()` calls — added to
    /// every line number `syn` reports, since each call re-parses just its
    /// own chunk (starting back at line 1 in isolation) but line numbers
    /// shown to the user need to keep counting up across the whole session.
    line_offset: usize,
    cur_line: usize,
    pending: Vec<Event>,
    fns: HashMap<String, Rc<FnDef>>,
    structs: HashMap<String, StructDef>,
    enums: HashMap<String, EnumDef>,
    consts: HashMap<String, Value>,
    trait_defaults: HashMap<String, HashMap<String, Rc<FnDef>>>,
    impls: HashMap<String, Vec<String>>,
    /// `use List::{Cons, Nil};` → `Cons` ↦ `List::Cons`. Keyed by the
    /// alias's full path (`ops::Cons` inside `mod ops`), and the target is
    /// kept as written, with the module it was written in.
    aliases: HashMap<String, (Vec<String>, Vec<String>)>,
    /// `use List::*;` — enums whose variants are nameable unqualified.
    glob_enums: Vec<String>,
    /// `use some_module::*;` — (where it was written, the path as written).
    glob_uses: Vec<(Vec<String>, Vec<String>)>,
    /// Every `mod` in the program by full path (`a::b`) → whether it's `pub`.
    /// Structs and enums share one namespace whatever module they're in;
    /// functions and consts in a module are keyed by full path (`ops::add`).
    modules: HashMap<String, bool>,
    /// Functions and consts in a module that aren't `pub`, by full path.
    private: HashSet<String>,
    /// Module of the code running now (`[]` = crate root), innermost last.
    mod_stack: Vec<Vec<String>>,
    /// Whether the rest of the program is known up front (a whole file),
    /// so borrows can end right after a reference's last use (NLL). In the
    /// live REPL the future is unknown: a borrow instead ends lazily, when
    /// something conflicting needs it to — and using the old reference
    /// after that is reported as the compile error it would be.
    lookahead: bool,
    live: Vec<LiveCtx>,
    /// Parallel to `stack.frames`: which function invocation each frame
    /// belongs to.
    frame_inv: Vec<usize>,
    next_inv: usize,
    self_tys: Vec<Option<String>>,
    /// Hidden bindings holding temporaries, dropped at the end of the
    /// statement that created them (unless borrowed — lifetime extension).
    temps: Vec<(usize, usize)>,
    /// One output buffer per user `fmt` call in progress (they nest: a
    /// `Display` impl can `{}`-format another `Display` type).
    fmt_bufs: Vec<String>,
    /// Dropping a value whose borrows already ended early (NLL): free its
    /// heap data but don't end those borrows a second time.
    keep_refs: bool,
    /// Closure definitions, indexed by `Value::Closure::id`.
    closures: Vec<Rc<iters::ClosureDef>>,
    /// Target type for the next `.collect()` (from `let v: Vec<_> = ...`).
    collect_hint: Option<String>,
    /// Source came in through `feed` (wrapped as `{chunk}`), not a file.
    chunk_wrapped: bool,
}

impl Default for Interpreter {
    fn default() -> Self {
        Self::new()
    }
}

impl Interpreter {
    fn empty() -> Self {
        Interpreter {
            stack: Stack::default(),
            heap: Heap::default(),
            steps: Vec::new(),
            output: String::new(),
            source_lines: Vec::new(),
            line_offset: 0,
            cur_line: 0,
            pending: Vec::new(),
            fns: HashMap::new(),
            structs: HashMap::new(),
            enums: HashMap::new(),
            consts: HashMap::new(),
            trait_defaults: HashMap::new(),
            impls: HashMap::new(),
            aliases: HashMap::new(),
            glob_enums: Vec::new(),
            glob_uses: Vec::new(),
            modules: HashMap::new(),
            private: HashSet::new(),
            mod_stack: vec![Vec::new()],
            lookahead: false,
            live: Vec::new(),
            frame_inv: Vec::new(),
            next_inv: 1,
            self_tys: vec![None],
            temps: Vec::new(),
            fmt_bufs: Vec::new(),
            keep_refs: false,
            closures: Vec::new(),
            collect_hint: None,
            chunk_wrapped: false,
        }
    }

    pub fn new() -> Self {
        let mut interp = Self::empty();
        interp.stack.push_frame("main");
        interp.frame_inv.push(0);
        interp
    }

    /// Runs one whole chunk (a single pasted block, or — in the live REPL
    /// — one more completed statement) against this interpreter's
    /// *existing* state. Calling this repeatedly, once per statement, is
    /// what makes the REPL incremental rather than starting over each time.
    /// Item definitions (`fn`, `struct`, `enum`, `impl`) are accepted too.
    pub fn feed(&mut self, source_chunk: &str) -> Result<(), InterpError> {
        // `{` shares line 1 with the chunk's own first line (rather than
        // sitting on its own line) specifically so syn's line numbers —
        // computed against this wrapped text — match what the user
        // actually typed, with no off-by-one.
        let block: Block = syn::parse_str(&format!("{{{source_chunk}\n}}"))
            .map_err(|e| InterpError::Parse(e.to_string()))?;
        self.source_lines.extend(source_chunk.lines().map(String::from));
        self.chunk_wrapped = true;
        let depth = self.stack.frames.len();
        let result = self.exec_stmts(&block.stmts).and_then(|v| self.drop_value(&v));
        self.line_offset += source_chunk.lines().count();
        match result {
            Ok(()) | Err(Flow::Return(_)) => Ok(()),
            Err(f) => {
                // Unwind anything a failed call left behind so the REPL
                // stays usable for the next statement.
                self.stack.frames.truncate(depth);
                self.frame_inv.truncate(depth);
                self.live.clear();
                self.temps.clear();
                self.pending.clear();
                Err(flow_to_err(f))
            }
        }
    }

    fn line_of<T: Spanned>(&self, node: &T) -> usize {
        self.line_offset + node.span().start().line
    }

    /// The exact source text of a node, when it sits on one line.
    fn source_text<T: Spanned>(&self, node: &T) -> Option<String> {
        let (start, end) = (node.span().start(), node.span().end());
        let line = self.source_lines.get(self.line_offset + start.line - 1)?;
        // `feed` wraps a chunk as `{chunk`, shifting its first line by one.
        let shift = usize::from(self.chunk_wrapped && start.line == 1);
        let chars: Vec<char> = line.chars().collect();
        let a = start.column.checked_sub(shift)?;
        if start.line != end.line {
            // Multi-line: its first line, then an ellipsis.
            let first: String = chars.get(a..)?.iter().collect();
            return Some(format!("{} … }}", first.trim_end()));
        }
        let b = end.column.checked_sub(shift)?;
        Some(chars.get(a..b)?.iter().collect())
    }

    fn close_line(&self, block: &Block) -> usize {
        self.line_offset + block.brace_token.span.close().start().line
    }

    fn unsupported(&self, what: impl Into<String>) -> Flow {
        Flow::Err(InterpError::Unsupported { what: what.into(), line: self.cur_line })
    }

    /// One-shot convenience for a bare statement block (what the golden
    /// tests use for snippet samples) — `new()` + one `feed()` + a final
    /// scope-exit drop, as if the whole thing were one program that just
    /// ran to completion.
    pub fn run(source: &str) -> Result<Interpreter, InterpError> {
        let mut interp = Interpreter::new();
        interp.lookahead = true;
        interp.feed(source)?;
        interp.drop_top_frame();
        Ok(interp)
    }

    /// Runs a whole program: a real `.rs` file with `fn main()` (plus any
    /// other functions/structs/enums/impls), or — if there's no `fn main`
    /// — a bare statement block. Always returns the interpreter so the
    /// trace up to an error can still be shown, with the error (if any)
    /// recorded as the final step.
    pub fn run_program(source: &str) -> (Interpreter, Result<(), InterpError>) {
        if !defines_main(source) {
            let mut interp = Interpreter::new();
            interp.lookahead = true;
            let r = interp.feed(source);
            match &r {
                Ok(()) => interp.drop_top_frame(),
                Err(e) => interp.record_error(e),
            }
            return (interp, r);
        }
        let mut interp = Interpreter::empty();
        interp.lookahead = true;
        interp.source_lines = source.lines().map(String::from).collect();
        let r = interp.run_file(source);
        if let Err(e) = &r {
            interp.record_error(e);
        }
        (interp, r)
    }

    fn run_file(&mut self, source: &str) -> Result<(), InterpError> {
        let file = syn::parse_file(source).map_err(|e| {
            let line = e.span().start().line;
            InterpError::Parse(format!("{e} (line {line})"))
        })?;
        for item in &file.items {
            self.register_item(item).map_err(flow_to_err)?;
        }
        let main = self.fns.get("main").cloned().ok_or_else(|| InterpError::CompileError("no `fn main` found".into()))?;
        let line = self.line_of(&main.sig);
        self.call_fn(main, None, Vec::new(), line).map(|_| ()).map_err(flow_to_err)
    }

    fn record_error(&mut self, e: &InterpError) {
        self.pending.push(Event::Note(format!("✗ {e}")));
        let line = self.cur_line;
        self.record_step(line);
    }

    /// Pops and finalizes the current top frame — called automatically at
    /// the end of batch `run()`, and by the live REPL on quit so the final
    /// scope-exit drops are visible/recorded before the session ends.
    pub fn drop_top_frame(&mut self) {
        if !self.stack.frames.is_empty() {
            let _ = self.pop_frame(0);
        }
    }

    fn record_step(&mut self, line: usize) {
        let source = self.source_lines.get(line.wrapping_sub(1)).cloned().unwrap_or_default();
        self.steps.push(Step {
            line,
            source,
            events: std::mem::take(&mut self.pending),
            stack: self.stack.clone(),
            heap: self.heap.clone(),
            output: self.output.clone(),
        });
    }

    /// Borrows that ended right after a statement belong to that
    /// statement's step, not the next one.
    fn amend_last_step(&mut self) {
        if self.pending.is_empty() || !self.pending.iter().all(|e| matches!(e, Event::BorrowExpired { .. })) {
            return;
        }
        if let Some(last) = self.steps.last_mut() {
            last.events.append(&mut self.pending);
            last.stack = self.stack.clone();
            last.heap = self.heap.clone();
        }
    }

    fn check_step_budget(&self) -> R<()> {
        if self.steps.len() > MAX_STEPS {
            return panic_err(format!("rust-viz stopped after {MAX_STEPS} steps — is there an infinite loop?"));
        }
        Ok(())
    }

    // ───────────────────────────── items ─────────────────────────────

    fn register_item(&mut self, item: &Item) -> R<()> {
        match item {
            Item::Fn(f) => {
                let name = self.qualify(&f.sig.ident.to_string(), &f.vis);
                let def = FnDef { name: name.clone(), sig: f.sig.clone(), block: (*f.block).clone(), self_ty: None, module: self.cur_mod().to_vec(), line_base: self.line_offset };
                self.fns.insert(name, Rc::new(def));
            }
            Item::Struct(s) => {
                let (shape, fields) = match &s.fields {
                    syn::Fields::Named(n) => (Shape::Named, n.named.iter().map(|f| f.ident.as_ref().unwrap().to_string()).collect()),
                    syn::Fields::Unnamed(u) => (Shape::Tuple, (0..u.unnamed.len()).map(|i| i.to_string()).collect()),
                    syn::Fields::Unit => (Shape::Unit, Vec::new()),
                };
                self.structs.insert(s.ident.to_string(), StructDef { shape, fields, derives: derives_of(&s.attrs) });
            }
            Item::Enum(e) => {
                let variants = e
                    .variants
                    .iter()
                    .map(|v| match &v.fields {
                        syn::Fields::Named(n) => (v.ident.to_string(), Shape::Named, n.named.iter().map(|f| f.ident.as_ref().unwrap().to_string()).collect()),
                        syn::Fields::Unnamed(u) => (v.ident.to_string(), Shape::Tuple, (0..u.unnamed.len()).map(|i| i.to_string()).collect()),
                        syn::Fields::Unit => (v.ident.to_string(), Shape::Unit, Vec::new()),
                    })
                    .collect();
                self.enums.insert(e.ident.to_string(), EnumDef { variants, derives: derives_of(&e.attrs) });
            }
            Item::Impl(imp) => {
                let ty = type_name(&imp.self_ty);
                if let Some((path, _)) = &imp.trait_ {
                    let tr = path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default();
                    self.impls.entry(ty.clone()).or_default().push(tr);
                }
                let tr = imp.trait_.as_ref().and_then(|(p, _)| p.segments.last()).map(|s| s.ident.to_string());
                for it in &imp.items {
                    if let syn::ImplItem::Fn(f) = it {
                        let name = format!("{ty}::{}", f.sig.ident);
                        let def = Rc::new(FnDef { name: name.clone(), sig: f.sig.clone(), block: f.block.clone(), self_ty: Some(ty.clone()), module: self.cur_mod().to_vec(), line_base: self.line_offset });
                        // `impl Display` and `impl Debug` both define `fmt`,
                        // so trait methods are also keyed by trait.
                        if let Some(tr) = &tr {
                            self.fns.insert(format!("{ty}::{tr}::{}", f.sig.ident), def.clone());
                        }
                        self.fns.insert(name, def);
                    }
                }
            }
            Item::Trait(t) => {
                let module = self.cur_mod().to_vec();
                let defaults = self.trait_defaults.entry(t.ident.to_string()).or_default();
                for it in &t.items {
                    if let syn::TraitItem::Fn(f) = it
                        && let Some(block) = &f.default
                    {
                        let def = FnDef { name: f.sig.ident.to_string(), sig: f.sig.clone(), block: block.clone(), self_ty: None, module: module.clone(), line_base: self.line_offset };
                        defaults.insert(f.sig.ident.to_string(), Rc::new(def));
                    }
                }
            }
            Item::Const(c) => {
                let v = self.eval_expr(&c.expr)?;
                let name = self.qualify(&c.ident.to_string(), &c.vis);
                self.consts.insert(name, v);
            }
            Item::Static(s) => {
                let v = self.eval_expr(&s.expr)?;
                let name = self.qualify(&s.ident.to_string(), &s.vis);
                self.consts.insert(name, v);
            }
            Item::Use(u) => self.register_use(&u.tree, Vec::new()),
            Item::Mod(m) => {
                let Some((_, items)) = &m.content else {
                    return Err(self.unsupported(format!(
                        "`mod {};` loading another file — rust-viz runs one file, so write the module inline: `mod {} {{ ... }}`",
                        m.ident, m.ident
                    )));
                };
                let mut path = self.cur_mod().to_vec();
                path.push(m.ident.to_string());
                self.modules.insert(path.join("::"), !matches!(m.vis, syn::Visibility::Inherited));
                self.mod_stack.push(path);
                let r = items.iter().try_for_each(|item| self.register_item(item));
                self.mod_stack.pop();
                r?;
            }
            _ => return Err(self.unsupported("this kind of item (only fn/struct/enum/impl/trait/const/use/mod)")),
        }
        Ok(())
    }

    fn register_use(&mut self, tree: &syn::UseTree, prefix: Vec<String>) {
        let with = |s: &syn::Ident| {
            let mut p = prefix.clone();
            p.push(s.to_string());
            p
        };
        match tree {
            syn::UseTree::Path(p) => self.register_use(&p.tree, with(&p.ident)),
            syn::UseTree::Name(n) => {
                let key = self.mod_key(&n.ident.to_string());
                self.aliases.insert(key, (self.cur_mod().to_vec(), with(&n.ident)));
            }
            syn::UseTree::Rename(r) => {
                let key = self.mod_key(&r.rename.to_string());
                self.aliases.insert(key, (self.cur_mod().to_vec(), with(&r.ident)));
            }
            syn::UseTree::Glob(_) => {
                if let Some(last) = prefix.last() {
                    self.glob_enums.push(last.clone());
                }
                self.glob_uses.push((self.cur_mod().to_vec(), prefix));
            }
            syn::UseTree::Group(g) => {
                for t in &g.items {
                    self.register_use(t, prefix.clone());
                }
            }
        }
    }

    fn find_method(&self, ty: &str, method: &str) -> Option<(Rc<FnDef>, String)> {
        if let Some(def) = self.fns.get(&format!("{ty}::{method}")) {
            return Some((def.clone(), ty.to_string()));
        }
        for tr in self.impls.get(ty)? {
            if let Some(def) = self.trait_defaults.get(tr).and_then(|d| d.get(method)) {
                return Some((def.clone(), ty.to_string()));
            }
        }
        None
    }

    fn current_self_ty(&self) -> Option<String> {
        self.self_tys.last().cloned().flatten()
    }

    /// `Self::new` → `Rect::new` inside an `impl Rect`.
    fn resolve_path_segments(&self, path: &syn::Path) -> Vec<String> {
        let mut segs: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        if segs.first().map(String::as_str) == Some("Self")
            && let Some(ty) = self.current_self_ty()
        {
            segs[0] = ty;
        }
        segs = self.module_path(segs);
        if segs.len() == 1 {
            if let Some((_, full)) = self.alias(&segs[0])
                && full.len() >= 2
                && self.enums.contains_key(&full[full.len() - 2])
            {
                segs = full[full.len() - 2..].to_vec();
            } else if let Some(e) = self.glob_enums.iter().find(|e| self.enums.get(*e).is_some_and(|d| d.variants.iter().any(|(v, ..)| *v == segs[0]))) {
                segs.insert(0, e.clone());
            }
        }
        // `std::mem::drop`, `std::string::String::from` etc.
        while segs.len() > 2 && matches!(segs[0].as_str(), "std" | "core" | "alloc") {
            segs.remove(0);
            if segs.len() > 2 {
                segs.remove(0);
            }
        }
        segs
    }

    // ─────────────────────────── modules ───────────────────────────

    fn cur_mod(&self) -> &[String] {
        self.mod_stack.last().map(Vec::as_slice).unwrap_or(&[])
    }

    /// `add` declared in `mod ops` → `ops::add`.
    fn mod_key(&self, name: &str) -> String {
        let mut path = self.cur_mod().to_vec();
        path.push(name.to_string());
        path.join("::")
    }

    /// `mod_key`, also remembering a non-`pub` item for privacy checks.
    fn qualify(&mut self, name: &str, vis: &syn::Visibility) -> String {
        let key = self.mod_key(name);
        if !self.cur_mod().is_empty() && matches!(vis, syn::Visibility::Inherited) {
            self.private.insert(key.clone());
        }
        key
    }

    /// The `use` alias `name` visible here: one written in this module,
    /// else (leniently) one at the crate root.
    fn alias(&self, name: &str) -> Option<&(Vec<String>, Vec<String>)> {
        self.aliases.get(&self.mod_key(name)).or_else(|| self.aliases.get(name))
    }

    fn is_item(&self, path: &[String]) -> bool {
        let key = path.join("::");
        self.fns.contains_key(&key) || self.consts.contains_key(&key) || self.modules.contains_key(&key)
    }

    /// Rewrites a path written in module `cur` to start at the crate root:
    /// `crate::`, `self::`, `super::`, a child module, an item of this
    /// module, or a `use` that names something in a module.
    fn absolute(&self, segs: &[String], cur: &[String], follow_use: bool) -> Vec<String> {
        match segs[0].as_str() {
            "crate" => return segs[1..].to_vec(),
            "self" => return [cur, &segs[1..]].concat(),
            "super" => {
                let n = segs.iter().take_while(|s| *s == "super").count();
                return [&cur[..cur.len().saturating_sub(n)], &segs[n..]].concat();
            }
            _ => {}
        }
        if follow_use && let Some((at, target)) = self.alias(&segs[0]) {
            let target = self.absolute(target, at, false);
            if self.modules.contains_key(&target[0]) {
                return [&target[..], &segs[1..]].concat();
            }
        }
        let local = [cur, segs].concat();
        if !cur.is_empty() && self.is_item(&local[..=cur.len()]) {
            return local;
        }
        if follow_use {
            for (at, prefix) in &self.glob_uses {
                if at.as_slice() != cur {
                    continue;
                }
                let target = self.absolute(prefix, at, false);
                if self.modules.contains_key(&target.join("::")) && self.is_item(&[&target[..], &segs[..1]].concat()) {
                    return [&target[..], segs].concat();
                }
            }
        }
        segs.to_vec()
    }

    /// Resolves module paths: `ops::add` stays `ops::add` (functions are
    /// keyed by full path), `shapes::Circle::new` becomes `Circle::new`
    /// (types aren't).
    fn module_path(&self, segs: Vec<String>) -> Vec<String> {
        if self.modules.is_empty() {
            return segs;
        }
        let abs = self.absolute(&segs, self.cur_mod(), true);
        let k = (1..abs.len()).rev().find(|&k| self.modules.contains_key(&abs[..k].join("::"))).unwrap_or(0);
        if k > 0 && (self.structs.contains_key(&abs[k]) || self.enums.contains_key(&abs[k])) {
            return abs[k..].to_vec();
        }
        abs
    }

    /// E0603: a private item is only reachable from inside its module
    /// (and that module's children); a private module only from its parent.
    fn check_visible(&self, path: &[String]) -> R<()> {
        let cur = self.cur_mod();
        for k in 1..path.len() {
            if self.modules.get(&path[..k].join("::")) == Some(&false) && !cur.starts_with(&path[..k - 1]) {
                return compile_err(format!("module `{}` is private (E0603)", path[k - 1]));
            }
        }
        if self.private.contains(&path.join("::")) && !cur.starts_with(&path[..path.len() - 1]) {
            let what = if self.consts.contains_key(&path.join("::")) { "constant" } else { "function" };
            return compile_err(format!("{what} `{}` is private (E0603)", path[path.len() - 1]));
        }
        Ok(())
    }

    // ─────────────────────────── frames ────────────────────────────

    fn cur_inv(&self) -> usize {
        *self.frame_inv.last().unwrap_or(&0)
    }

    fn push_scope(&mut self, name: &str) {
        self.stack.push_scope(name);
        self.frame_inv.push(self.cur_inv());
    }

    /// Pops the top frame. For every binding not moved out: if it's a
    /// reference, ending its borrow on whatever it pointed at (so that
    /// binding becomes usable again, same as a real borrow ending when the
    /// reference holding it goes out of scope); otherwise dropping it,
    /// which frees any heap data it owns — this is what a real scope exit
    /// does to anything nobody took ownership of. Reverse declaration
    /// order, like real Rust.
    fn pop_frame(&mut self, line: usize) -> R<()> {
        let idx = self.stack.frames.len() - 1;
        let n = self.stack.frames[idx].bindings.len();
        let is_scope = self.stack.frames[idx].is_scope;
        let fname = self.stack.frames[idx].name.clone();
        for slot in (0..n).rev() {
            let b = &self.stack.frames[idx].bindings[slot];
            if b.state == OwnershipState::Moved {
                continue;
            }
            let (name, value, expired, moved_paths) = (b.name.clone(), b.value.clone(), b.expired, b.moved_paths.clone());
            let hidden = name == "<temp>";
            if b.alias {
                // A captured variable's stand-in: the real thing lives on.
                self.stack.frames[idx].bindings[slot].state = OwnershipState::Moved;
                continue;
            }
            match &value {
                Value::Ref(p, _) => {
                    if !expired {
                        self.drop_value(&value)?;
                        // A reborrow (an iterator's `&v[i]`, …) held no
                        // borrow of its own, so there's nothing to announce.
                        if !hidden && !p.reborrow {
                            self.pending.push(Event::EndBorrow { name });
                        }
                    }
                }
                Value::Formatter(_) | Value::FnItem(_) => {}
                _ => {
                    let prev = std::mem::replace(&mut self.keep_refs, expired);
                    let r = self.drop_value_except(&value, &moved_paths, &mut Vec::new());
                    self.keep_refs = prev;
                    r?;
                    if !hidden {
                        self.pending.push(Event::Drop { name });
                    }
                }
            }
            self.stack.frames[idx].bindings[slot].state = OwnershipState::Moved;
        }
        self.stack.pop_frame();
        self.frame_inv.pop();
        self.temps.retain(|(f, _)| *f < idx);
        if is_scope {
            if !self.pending.is_empty() {
                self.pending.insert(0, Event::Note(format!("end of `{fname}` scope")));
                self.record_step(line);
            }
        } else {
            self.pending.push(Event::FrameExit { name: fname });
            self.record_step(line);
        }
        Ok(())
    }

    /// Pops a scope that only held tentative clones (match guard).
    fn pop_frame_silently(&mut self) {
        let idx = self.stack.frames.len() - 1;
        self.stack.pop_frame();
        self.frame_inv.pop();
        self.temps.retain(|(f, _)| *f < idx);
    }

    fn declare(&mut self, name: String, value: Value, mutable: bool) {
        self.stack.current_frame_mut().declare(name.clone(), value, mutable);
        self.pending.push(Event::Declare { name });
    }

    fn temp_place(&mut self, v: Value) -> PlaceRef {
        let frame = self.stack.frames.len() - 1;
        let slot = self.stack.current_frame_mut().declare("<temp>", v, true);
        self.temps.push((frame, slot));
        PlaceRef { place: Place { frame, slot, name: "<temp>".into(), path: vec![], reborrow: false }, via: None }
    }

    /// Drops temporaries created since `mark` — end of statement. One that's
    /// still borrowed (`let r = &String::from(..)`) lives on until its
    /// scope ends, like real temporary lifetime extension.
    fn drop_temps_from(&mut self, mark: usize) -> R<()> {
        while self.temps.len() > mark {
            let (f, s) = self.temps.pop().unwrap();
            let Some(b) = self.stack.binding(f, s) else { continue };
            if b.state != OwnershipState::Owned {
                continue;
            }
            let (v, moved) = (b.value.clone(), b.moved_paths.clone());
            self.drop_value_except(&v, &moved, &mut Vec::new())?;
            self.stack.binding_mut(f, s).unwrap().state = OwnershipState::Moved;
        }
        Ok(())
    }

    // ────────────────────────── ownership ──────────────────────────

    /// Runs `v`'s destructor: ends the borrow if it's a reference, frees
    /// its heap allocation(s) if it owns any — children first, like real
    /// drop glue (a `Vec<String>` drops each `String`, then its buffer).
    fn drop_value(&mut self, v: &Value) -> R<()> {
        self.drop_value_except(v, &[], &mut Vec::new())
    }

    fn drop_value_except(&mut self, v: &Value, moved: &[Vec<Proj>], path: &mut Vec<Proj>) -> R<()> {
        if moved.iter().any(|m| m == path) {
            return Ok(());
        }
        match v {
            Value::Ref(place, kind)
                if !self.keep_refs => {
                    self.release(place, *kind);
                }
            Value::Str(id) => self.free(*id),
            Value::Closure { captures, .. } => {
                for (n, x) in captures {
                    path.push(Proj::Field(n.clone()));
                    self.drop_value_except(x, moved, path)?;
                    path.pop();
                }
            }
            Value::Iter(node) => {
                for x in node.held_values() {
                    self.drop_value_except(x, &[], &mut Vec::new())?;
                }
                for id in node.buffers() {
                    self.free(id);
                }
            }
            Value::Vec(id) => {
                if let Ok(HeapObject::Vec { items, .. }) = self.heap.get(*id) {
                    for x in items.clone() {
                        self.drop_value(&x)?;
                    }
                }
                self.free(*id);
            }
            Value::Map(id) => {
                // Entries first (keys and values), then the table itself.
                for (k, x) in self.map_entries(*id)? {
                    self.drop_value(&k)?;
                    self.drop_value(&x)?;
                }
                self.free(*id);
            }
            Value::Box(id) => {
                if let Ok(HeapObject::Box(inner)) = self.heap.get(*id) {
                    let inner = inner.clone();
                    path.push(Proj::Deref);
                    self.drop_value_except(&inner, moved, path)?;
                    path.pop();
                }
                self.free(*id);
            }
            Value::Array(items) | Value::Tuple(items) => {
                for (i, x) in items.iter().enumerate() {
                    path.push(Proj::Field(i.to_string()));
                    self.drop_value_except(x, moved, path)?;
                    path.pop();
                }
            }
            Value::Struct { fields, .. } | Value::Enum { fields, .. } => {
                for (n, x) in fields {
                    path.push(Proj::Field(n.clone()));
                    self.drop_value_except(x, moved, path)?;
                    path.pop();
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn free(&mut self, id: HeapId) {
        if self.heap.free(id) {
            self.pending.push(Event::HeapFree { id });
        }
    }

    /// Ends the borrow a reference value holds (if it holds one of its own).
    fn release(&mut self, place: &Place, kind: RefKind) {
        if place.reborrow {
            return;
        }
        if let Some(b) = self.stack.binding_mut(place.frame, place.slot) {
            match kind {
                RefKind::Shared => b.end_borrow_shared(),
                RefKind::Mutable => b.end_borrow_mut(),
            }
        }
    }

    /// A shared reference got duplicated — each copy holds its own share of
    /// the borrow, so the target stays borrowed until *all* copies are gone.
    fn on_copy(&mut self, v: &Value) {
        match v {
            Value::Ref(p, RefKind::Shared) if !p.reborrow => {
                if let Some(b) = self.stack.binding_mut(p.frame, p.slot) {
                    let _ = b.borrow_shared();
                }
            }
            Value::Array(items) | Value::Tuple(items) => items.iter().for_each(|x| self.on_copy(x)),
            Value::Struct { fields, .. } | Value::Enum { fields, .. } | Value::Closure { captures: fields, .. } => {
                fields.iter().for_each(|(_, x)| self.on_copy(x))
            }
            _ => {}
        }
    }

    /// The single gate every direct (not-through-a-reference) use of a
    /// binding goes through — this is the borrow checker.
    fn access(&mut self, frame: usize, slot: usize, path: &[Proj], a: Access) -> R<()> {
        for attempt in 0..2 {
            let lookahead = self.lookahead;
            let b = self.stack.binding_mut(frame, slot).ok_or_else(|| MemoryError::Dangling("?".into()))?;
            if b.expired && !b.alias && !lookahead && a != Access::Write {
                return compile_err(format!(
                    "`{}` is used here, but the borrow it holds was already invalidated by a later conflicting use of what it points to (E0502/E0499)",
                    b.name
                ));
            }
            let display = describe_path(&b.name, path);
            if let Some(m) = b.moved_paths.iter().find(|m| m.starts_with(path) || path.starts_with(m)) {
                // Writing exactly the moved-out path (or the whole binding)
                // re-initializes it; anything else touches moved data.
                let reinit = a == Access::Write && (m.as_slice() == path || path.is_empty());
                if !reinit {
                    return Err(MemoryError::UseAfterMove(describe_path(&b.name, m)).into());
                }
            }
            let r = match a {
                Access::Read => match b.state {
                    OwnershipState::Moved if b.deferred => {
                        return compile_err(format!("used binding `{}` isn't initialized (E0381)", b.name));
                    }
                    OwnershipState::Moved => Err(MemoryError::UseAfterMove(display)),
                    OwnershipState::BorrowedMut => Err(MemoryError::UseWhileMutablyBorrowed(display)),
                    _ => Ok(()),
                },
                Access::Move if !path.is_empty() => match b.state {
                    OwnershipState::Moved => Err(MemoryError::UseAfterMove(display)),
                    OwnershipState::Owned => {
                        b.moved_paths.push(path.to_vec());
                        Ok(())
                    }
                    _ => Err(MemoryError::MoveWhileBorrowed(display)),
                },
                Access::Move => {
                    if !b.moved_paths.is_empty() {
                        return Err(MemoryError::UseAfterMove(format!("{} (partially moved)", b.name)).into());
                    }
                    b.mark_moved()
                }
                Access::Write => match b.state {
                    OwnershipState::Moved if path.is_empty() => Ok(()),
                    OwnershipState::Moved => Err(MemoryError::UseAfterMove(display)),
                    OwnershipState::BorrowedShared(_) | OwnershipState::BorrowedMut => Err(MemoryError::AssignWhileBorrowed(display)),
                    OwnershipState::Owned => Ok(()),
                },
                Access::Shared => b.borrow_shared(),
                Access::Mut => b.borrow_mut(),
            };
            match r {
                Ok(()) => return Ok(()),
                Err(e) => {
                    let conflict = matches!(
                        e,
                        MemoryError::MoveWhileBorrowed(_)
                            | MemoryError::BorrowConflict(_)
                            | MemoryError::UseWhileMutablyBorrowed(_)
                            | MemoryError::AssignWhileBorrowed(_)
                    );
                    if attempt == 0 && conflict && !lookahead && self.invalidate_borrowers(frame, slot) {
                        continue;
                    }
                    return Err(e.into());
                }
            }
        }
        unreachable!()
    }

    /// REPL-mode (no lookahead) NLL: something conflicting needs `frame/
    /// slot`, so assume the references currently borrowing it are done.
    fn invalidate_borrowers(&mut self, frame: usize, slot: usize) -> bool {
        let mut found = Vec::new();
        for (fi, f) in self.stack.frames.iter().enumerate() {
            for (si, b) in f.bindings.iter().enumerate() {
                if b.expired || b.alias || b.state == OwnershipState::Moved {
                    continue;
                }
                let mut refs = Vec::new();
                refs_in(&b.value, &mut refs);
                if refs.iter().any(|(p, _)| p.frame == frame && p.slot == slot) {
                    found.push((fi, si, refs, b.name.clone()));
                }
            }
        }
        // The holder (a reference, or a closure/iterator holding one) is
        // assumed done: all of its borrows end.
        for (fi, si, refs, name) in &found {
            for (p, kind) in refs {
                self.release(p, *kind);
                self.pending.push(Event::BorrowExpired { name: name.clone(), target: p.describe() });
            }
            self.stack.binding_mut(*fi, *si).unwrap().expired = true;
        }
        !found.is_empty()
    }

    /// File-mode NLL: after each statement, end the borrow of every
    /// reference binding whose name never appears in code still to run.
    fn expire_borrows(&mut self) {
        if !self.lookahead {
            return;
        }
        let inv = self.cur_inv();
        loop {
            let mut changed = false;
            for k in (0..self.stack.frames.len()).rev() {
                if self.frame_inv[k] != inv {
                    break;
                }
                for slot in 0..self.stack.frames[k].bindings.len() {
                    let b = &self.stack.frames[k].bindings[slot];
                    // Still borrowed itself (`&r`) → the chain is in use.
                    if b.expired || b.state != OwnershipState::Owned {
                        continue;
                    }
                    if b.alias {
                        continue;
                    }
                    // A reference, or a value holding references (a closure
                    // that captured by &/&mut, an iterator over a borrow…).
                    let is_ref = matches!(b.value, Value::Ref(..));
                    if !is_ref && b.name == "<temp>" {
                        continue;
                    }
                    let mut refs = Vec::new();
                    refs_in(&b.value, &mut refs);
                    if refs.is_empty() && !is_ref {
                        continue;
                    }
                    let name = &b.name;
                    if self.live.iter().any(|c| c.inv == inv && c.frame >= k && c.idents.contains(name)) {
                        continue;
                    }
                    let name = name.clone();
                    for (place, kind) in &refs {
                        self.release(place, *kind);
                        if name != "<temp>" {
                            self.pending.push(Event::BorrowExpired { name: name.clone(), target: place.describe() });
                        }
                    }
                    self.stack.frames[k].bindings[slot].expired = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
    }

    fn push_live<T: ToTokens>(&mut self, node: &T) -> bool {
        if !self.lookahead {
            return false;
        }
        let frame = self.stack.frames.len() - 1;
        self.live.push(LiveCtx { inv: self.cur_inv(), frame, idents: idents_of(node) });
        true
    }

    // ─────────────────────────── places ────────────────────────────

    /// Resolves a place expression (`x`, `x.f`, `v[i]`, `*r`, `s[a..b]`),
    /// or `None` if `expr` isn't one (it computes a fresh value instead).
    fn eval_place(&mut self, expr: &Expr) -> R<Option<PlaceRef>> {
        match expr {
            Expr::Path(p) => {
                let Some(name) = path_ident(&p.path) else { return Ok(None) };
                let Some((frame, slot)) = self.stack.lookup_slot(&name) else { return Ok(None) };
                // A captured variable inside a closure call: go straight to
                // the real storage, governed by the capture's borrow kind.
                if let Some(b) = self.stack.binding(frame, slot)
                    && b.alias
                    && let Value::Ref(target, kind) = &b.value
                {
                    return Ok(Some(PlaceRef { place: target.clone(), via: Some(*kind) }));
                }
                Ok(Some(PlaceRef { place: Place { frame, slot, name, path: vec![], reborrow: false }, via: None }))
            }
            Expr::Paren(p) => self.eval_place(&p.expr),
            Expr::Group(g) => self.eval_place(&g.expr),
            Expr::Field(f) => {
                let base = self.place_or_temp(&f.base)?;
                let mut base = self.auto_deref(base)?;
                let field = member_name(&f.member);
                match self.got(&base.place)? {
                    Got::Val(Value::Struct { fields, ty, .. }) if !fields.iter().any(|(n, _)| *n == field) => {
                        return compile_err(format!("no field `{field}` on type `{ty}` (E0609)"));
                    }
                    Got::Val(Value::Struct { .. } | Value::Tuple(_)) => {}
                    Got::Val(v) => return compile_err(format!("no field `{field}` on a value of type `{}` (E0609)", v.type_name())),
                    _ => return compile_err(format!("no field `{field}` on a slice")),
                }
                base.place.path.push(Proj::Field(field));
                Ok(Some(base))
            }
            Expr::Index(ix) => {
                let base = self.place_or_temp(&ix.expr)?;
                let mut base = self.auto_deref(base)?;
                // `map[&key]`: the `Index` impl for HashMap — a lookup that
                // panics when the key is missing.
                if let Got::Val(Value::Map(id)) = self.got(&base.place)? {
                    if base.via.is_none() {
                        self.access(base.place.frame, base.place.slot, &base.place.path, Access::Read)?;
                    }
                    let (key, temp) = self.eval_peek(&ix.index)?;
                    let found = self.map_find(id, &key)?;
                    if temp {
                        self.drop_value(&key)?;
                    }
                    let Some(i) = found else { return panic_err("key not found in HashMap (`map[&key]` panics when the key is missing — use `.get()`)") };
                    base.place.path.push(Proj::MapVal(i));
                    return Ok(Some(base));
                }
                let len = self.len_of(&base.place)?;
                let proj = match &*ix.index {
                    Expr::Range(r) => {
                        let a = match &r.start {
                            Some(e) => self.eval_usize(e)?,
                            None => 0,
                        };
                        let mut b = match &r.end {
                            Some(e) => self.eval_usize(e)?,
                            None => len,
                        };
                        if matches!(r.limits, RangeLimits::Closed(_)) {
                            b += 1;
                        }
                        if a > b {
                            return panic_err(format!("slice index starts at {a} but ends at {b}"));
                        }
                        if b > len {
                            return panic_err(format!("range end index {b} out of range for slice of length {len}"));
                        }
                        Proj::Range(a, b)
                    }
                    e => {
                        if matches!(self.got(&base.place)?, Got::Str(_) | Got::Val(Value::Str(_) | Value::StaticStr(_))) {
                            return compile_err(
                                "the type `str` cannot be indexed by `{integer}` — Rust strings are UTF-8, so use .chars() or a byte-range slice like &s[0..1] (E0277)",
                            );
                        }
                        let i = self.eval_usize(e)?;
                        if i >= len {
                            return panic_err(format!("index out of bounds: the len is {len} but the index is {i}"));
                        }
                        Proj::Index(i)
                    }
                };
                push_proj(&mut base.place, proj);
                if let Err(e) = resolve(&self.stack, &self.heap, &base.place) {
                    return Err(match e {
                        MemoryError::IndexOutOfBounds { .. } => InterpError::Panic("byte index is not on a char boundary".into()).into(),
                        e => e.into(),
                    });
                }
                Ok(Some(base))
            }
            Expr::Unary(u) if matches!(u.op, UnOp::Deref(_)) => {
                let inner = self.place_or_temp(&u.expr)?;
                if inner.via.is_none() {
                    self.access(inner.place.frame, inner.place.slot, &inner.place.path, Access::Read)?;
                }
                match self.got(&inner.place)? {
                    Got::Val(Value::Ref(target, kind)) => Ok(Some(PlaceRef { place: target, via: Some(combine(inner.via, kind)) })),
                    Got::Val(Value::Box(_)) => {
                        let mut p = inner;
                        p.place.path.push(Proj::Deref);
                        Ok(Some(p))
                    }
                    Got::Val(v) => compile_err(format!("type `{}` cannot be dereferenced (E0614)", v.type_name())),
                    _ => compile_err("cannot dereference a slice here"),
                }
            }
            _ => Ok(None),
        }
    }

    fn place_or_temp(&mut self, expr: &Expr) -> R<PlaceRef> {
        if let Some(p) = self.eval_place(expr)? {
            return Ok(p);
        }
        let v = self.eval_expr(expr)?;
        Ok(self.temp_place(v))
    }

    /// Follows references (and boxes) until reaching a non-pointer value —
    /// what `.` does implicitly for field access and method calls.
    fn auto_deref(&mut self, mut p: PlaceRef) -> R<PlaceRef> {
        loop {
            match self.got(&p.place)? {
                Got::Val(Value::Ref(target, kind)) => {
                    if p.via.is_none() {
                        self.access(p.place.frame, p.place.slot, &p.place.path, Access::Read)?;
                    }
                    p = PlaceRef { place: target, via: Some(combine(p.via, kind)) };
                }
                Got::Val(Value::Box(_)) => p.place.path.push(Proj::Deref),
                _ => return Ok(p),
            }
        }
    }

    fn got(&self, place: &Place) -> R<Got> {
        Ok(match resolve(&self.stack, &self.heap, place)? {
            Resolved::Val(v) => Got::Val(v.clone()),
            Resolved::Str(s) => Got::Str(s.to_string()),
            Resolved::Slice(items) => Got::Slice(items.to_vec()),
            Resolved::Bytes(b) => Got::Slice(b.iter().map(|x| Value::Int(*x as i64)).collect()),
            Resolved::Byte(b) => Got::Val(Value::Int(b as i64)),
        })
    }

    fn len_of(&self, place: &Place) -> R<usize> {
        Ok(match self.got(place)? {
            Got::Str(s) => s.len(),
            Got::Slice(items) => items.len(),
            Got::Val(Value::Array(items)) => items.len(),
            Got::Val(Value::StaticStr(s)) => s.len(),
            Got::Val(Value::Vec(id)) => match self.heap.get(id)? {
                HeapObject::Vec { items, .. } => items.len(),
                _ => 0,
            },
            Got::Val(Value::Str(id)) => match self.heap.get(id)? {
                HeapObject::Str { s, .. } => s.len(),
                _ => 0,
            },
            Got::Val(Value::Map(id)) => match self.heap.get(id)? {
                HeapObject::Map { entries, .. } => entries.len(),
                _ => 0,
            },
            Got::Val(v) => return compile_err(format!("cannot index into a value of type `{}` (E0608)", v.type_name())),
        })
    }

    /// Using a place by value: a copy for `Copy` types, otherwise a move —
    /// mirroring exactly what reading `s` (a `String`) vs. `n` (an `i32`)
    /// does in real Rust.
    fn read_place(&mut self, p: &PlaceRef) -> R<Value> {
        if p.via.is_none() {
            self.access(p.place.frame, p.place.slot, &p.place.path, Access::Read)?;
        }
        let v = match self.got(&p.place)? {
            Got::Val(v) => v,
            _ => return compile_err("a `str`/slice can't be used by value (its size isn't known at compile time) — borrow it with `&` (E0277)"),
        };
        if v.is_copy() {
            self.on_copy(&v);
            return Ok(v);
        }
        let what = p.place.describe();
        if p.via.is_some() {
            return compile_err(format!(
                "cannot move out of `{what}`, which is behind a reference — borrow it with `&` or `.clone()` it instead (E0507)"
            ));
        }
        if p.place.path.iter().any(|x| matches!(x, Proj::Index(_))) {
            return compile_err(format!("cannot move out of index `{what}` — borrow the element with `&` or `.clone()` it (E0507)"));
        }
        // Moving out of a box (`*b`) consumes the whole box.
        let path: Vec<Proj> = if p.place.path.contains(&Proj::Deref) { vec![] } else { p.place.path.clone() };
        self.access(p.place.frame, p.place.slot, &path, Access::Move)?;
        if p.place.name != "<temp>" {
            self.pending.push(Event::Move { from: what });
        }
        Ok(v)
    }

    /// The access check for anything that writes to a place.
    fn check_write(&mut self, p: &PlaceRef, whole_assign: bool) -> R<()> {
        match p.via {
            Some(RefKind::Shared) => compile_err(format!(
                "cannot assign to/mutate `{}`, which is behind a `&` reference — it would need to be `&mut` (E0594/E0596)",
                p.place.describe()
            )),
            Some(RefKind::Mutable) => Ok(()),
            None => {
                let b = self.stack.binding(p.place.frame, p.place.slot).ok_or_else(|| MemoryError::Dangling(p.place.name.clone()))?;
                let deferred_init = b.deferred && b.state == OwnershipState::Moved && whole_assign;
                if !b.mutable && !deferred_init {
                    return Err(MemoryError::AssignToImmutable(b.name.clone()).into());
                }
                self.access(p.place.frame, p.place.slot, &p.place.path, Access::Write)
            }
        }
    }

    /// `&place` / `&mut place` — this is the borrow-checker rule enforced
    /// at our runtime: `access` returns an error if the place is already
    /// moved or borrowed incompatibly, exactly the "one mutable XOR many
    /// immutable" rule from real Rust.
    fn borrow_place(&mut self, p: PlaceRef, mutable: bool) -> R<Value> {
        let kind = if mutable { RefKind::Mutable } else { RefKind::Shared };
        let mut place = p.place.clone();
        match p.via {
            None => {
                let b = self.stack.binding(place.frame, place.slot).ok_or_else(|| MemoryError::Dangling(place.name.clone()))?;
                if mutable && !b.mutable && b.name != "<temp>" {
                    return compile_err(format!(
                        "cannot borrow `{}` as mutable, as it is not declared as mutable — use `let mut {}` (E0596)",
                        place.describe(),
                        b.name
                    ));
                }
                self.access(place.frame, place.slot, &place.path, if mutable { Access::Mut } else { Access::Shared })?;
                place.reborrow = false;
                if place.name != "<temp>" {
                    self.pending.push(Event::Borrow { name: place.describe(), mutable });
                }
            }
            Some(RefKind::Shared) => {
                if mutable {
                    return compile_err(format!(
                        "cannot borrow `{}` as mutable, because it is behind a `&` reference (E0596)",
                        place.describe()
                    ));
                }
                // Count it on the root, so the target stays borrowed even if
                // the reference we went through ends first (e.g. a `&str`
                // returned from a function that took `&String`).
                let b = self.stack.binding_mut(place.frame, place.slot).ok_or_else(|| MemoryError::Dangling(place.name.clone()))?;
                if matches!(b.state, OwnershipState::BorrowedShared(_)) {
                    b.borrow_shared()?;
                    place.reborrow = false;
                } else {
                    place.reborrow = true;
                }
            }
            Some(RefKind::Mutable) => place.reborrow = true,
        }
        Ok(Value::Ref(place, kind))
    }

    /// Evaluates for reading without taking ownership (format args,
    /// comparisons — both auto-reference their operands in real Rust).
    /// Returns whether the value is a fresh temporary the caller must drop.
    fn eval_peek(&mut self, expr: &Expr) -> R<(Value, bool)> {
        if let Some(p) = self.eval_place(expr)? {
            if p.via.is_none() {
                self.access(p.place.frame, p.place.slot, &p.place.path, Access::Read)?;
            }
            return Ok(match self.got(&p.place)? {
                Got::Val(v) => (v, false),
                Got::Str(s) => (Value::StaticStr(s), false),
                Got::Slice(items) => (Value::Array(items), false),
            });
        }
        Ok((self.eval_expr(expr)?, true))
    }

    /// Like `eval_peek`, but a place comes back as a (count-free) reference
    /// to it rather than a copy — `println!` really does take `&arg`, and a
    /// user `fmt` then gets `self` pointing at the actual variable.
    fn peek_ref(&mut self, expr: &Expr) -> R<(Value, bool)> {
        if let Some(p) = self.eval_place(expr)? {
            if p.via.is_none() {
                self.access(p.place.frame, p.place.slot, &p.place.path, Access::Read)?;
            }
            return Ok((Value::Ref(Place { reborrow: true, ..p.place }, RefKind::Shared), false));
        }
        Ok((self.eval_expr(expr)?, true))
    }

    /// A function argument. Passing a `&mut` binding reborrows it for the
    /// call instead of moving it, like real Rust's implicit reborrow.
    fn eval_arg(&mut self, expr: &Expr) -> R<Value> {
        if let Some(p) = self.eval_place(expr)?
            && let Got::Val(Value::Ref(target, RefKind::Mutable)) = self.got(&p.place)?
        {
            if p.via.is_none() {
                self.access(p.place.frame, p.place.slot, &p.place.path, Access::Read)?;
            }
            return Ok(Value::Ref(Place { reborrow: true, ..target }, RefKind::Mutable));
        }
        self.eval_expr(expr)
    }

    // ───────────────────────── statements ──────────────────────────

    fn exec_stmts(&mut self, stmts: &[Stmt]) -> R<Value> {
        let tracked = if self.lookahead {
            let sets: Vec<HashSet<String>> = stmts.iter().map(idents_of).collect();
            let mut suffix = vec![HashSet::new(); stmts.len() + 1];
            for i in (0..stmts.len()).rev() {
                suffix[i] = suffix[i + 1].union(&sets[i]).cloned().collect();
            }
            let frame = self.stack.frames.len() - 1;
            self.live.push(LiveCtx { inv: self.cur_inv(), frame, idents: HashSet::new() });
            Some(suffix)
        } else {
            None
        };
        let live_idx = self.live.len().wrapping_sub(1);
        let mut result = Ok(Value::Unit);
        for (i, stmt) in stmts.iter().enumerate() {
            if let Some(suffix) = &tracked {
                // Include the running statement itself (conservative for
                // nested blocks inside it), then drop it once it's done.
                self.live[live_idx].idents = suffix[i].clone();
            }
            let is_tail = i + 1 == stmts.len() && matches!(stmt, Stmt::Expr(_, None));
            let r = match stmt {
                Stmt::Expr(e, None) if is_tail => self.exec_expr_stmt(e, false),
                _ => self.exec_stmt(stmt).map(|_| Value::Unit),
            };
            match r {
                Ok(v) => {
                    if is_tail {
                        result = Ok(v);
                    }
                }
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
            if let Some(suffix) = &tracked {
                self.live[live_idx].idents = suffix[i + 1].clone();
            }
            self.expire_borrows();
            self.amend_last_step();
        }
        if tracked.is_some() {
            self.live.truncate(live_idx);
        }
        result
    }

    fn exec_stmt(&mut self, stmt: &Stmt) -> R<()> {
        self.cur_line = self.line_of(stmt);
        match stmt {
            Stmt::Local(local) => {
                let mark = self.temps.len();
                self.exec_local(local)?;
                self.drop_temps_from(mark)?;
                self.record_step(self.line_of(local));
                Ok(())
            }
            Stmt::Expr(e, _) => {
                let v = self.exec_expr_stmt(e, true)?;
                self.drop_value(&v)
            }
            // A macro call used directly as a statement (`println!(...);`,
            // not bound to anything) is a distinct `Stmt` variant from a
            // macro call that's part of a larger expression.
            Stmt::Macro(m) => {
                let line = self.line_of(m);
                let mark = self.temps.len();
                let v = self.eval_macro(&m.mac, line)?;
                self.drop_value(&v)?;
                self.drop_temps_from(mark)?;
                self.record_step(line);
                Ok(())
            }
            Stmt::Item(item) => self.register_item(item),
        }
    }

    /// An expression in statement position. Block-like expressions (`if`,
    /// loops, …) record their own inner steps; anything else is one step.
    /// With `discard`, the value is dropped here (a `;`-terminated
    /// statement) — otherwise it's returned (a block's tail value).
    fn exec_expr_stmt(&mut self, e: &Expr, discard: bool) -> R<Value> {
        self.cur_line = self.line_of(e);
        let line = self.cur_line;
        let mark = self.temps.len();
        let v = self.eval_expr(e)?;
        let v = if discard {
            self.drop_value(&v)?;
            Value::Unit
        } else {
            v
        };
        self.drop_temps_from(mark)?;
        let block_like = matches!(e, Expr::If(_) | Expr::While(_) | Expr::ForLoop(_) | Expr::Loop(_) | Expr::Block(_) | Expr::Match(_));
        if !block_like || !self.pending.is_empty() {
            self.record_step(line);
        }
        Ok(v)
    }

    fn exec_local(&mut self, local: &Local) -> R<()> {
        let (pat, _) = strip_type(&local.pat);
        if let Pat::Ident(p) = pat
            && p.subpat.is_none()
            && p.by_ref.is_none()
        {
            let name = p.ident.to_string();
            let mutable = p.mutability.is_some();
            match &local.init {
                Some(init) => {
                    self.collect_hint = strip_type(&local.pat).1.map(|t| t.to_token_stream().to_string());
                    let value = self.eval_expr(&init.expr);
                    self.collect_hint = None;
                    let value = value?;
                    self.declare(name, value, mutable);
                }
                None => {
                    self.stack.current_frame_mut().declare(name.clone(), Value::Unit, mutable);
                    let b = self.stack.current_frame_mut().bindings.last_mut().unwrap();
                    b.state = OwnershipState::Moved;
                    b.deferred = true;
                    self.pending.push(Event::Note(format!("`{name}` declared, not initialized yet")));
                }
            }
            return Ok(());
        }
        let Some(init) = &local.init else {
            return Err(self.unsupported("destructuring `let` without an initializer"));
        };
        let subject = self.place_or_temp(&init.expr)?;
        let mut binds = Vec::new();
        if !self.match_pat(pat, &subject, None, &mut binds)? {
            match &init.diverge {
                Some((_, else_expr)) => {
                    self.eval_expr(else_expr)?;
                    return compile_err("`let ... else` block must diverge (return/break/continue/panic) (E0308)");
                }
                None => return compile_err("refutable pattern in local binding — use `if let` or `match` (E0005)"),
            }
        }
        let vals = self.commit(binds)?;
        for (n, v, m) in vals {
            self.declare(n, v, m);
        }
        Ok(())
    }

    /// Runs a `{ ... }` block as its own scope, optionally with bindings
    /// (from a pattern) already in it.
    fn exec_block(&mut self, block: &Block, scope: &str, binds: Vec<(String, Value, bool)>) -> R<Value> {
        self.push_scope(scope);
        for (n, v, m) in binds {
            self.declare(n, v, m);
        }
        let r = self.exec_stmts(&block.stmts);
        if let Err(Flow::Err(_)) = r {
            return r; // keep the state at the error visible
        }
        // A tail value that borrows something declared inside this scope
        // would dangle once the scope is popped.
        if let Ok(v) = &r {
            let top = self.stack.frames.len() - 1;
            if let Some(p) = ref_into_frames(v, top) {
                return compile_err(format!("`{}` does not live long enough — it's dropped at the end of this block while still borrowed (E0597)", p.name));
            }
        }
        self.pop_frame(self.close_line(block))?;
        r
    }

    // ───────────────────────── expressions ─────────────────────────

    fn eval_expr(&mut self, expr: &Expr) -> R<Value> {
        match expr {
            Expr::Lit(ExprLit { lit, .. }) => self.eval_lit(lit),
            Expr::Path(p) => match self.eval_place(expr)? {
                Some(pr) => self.read_place(&pr),
                None => self.eval_path_value(&p.path),
            },
            Expr::Unary(u) => match u.op {
                UnOp::Deref(_) => {
                    let p = self.eval_place(expr)?.expect("deref is a place");
                    self.read_place(&p)
                }
                _ => {
                    let v = self.eval_expr(&u.expr)?;
                    let v = self.take_val(v)?;
                    match (&u.op, v) {
                        (UnOp::Neg(_), Value::Int(i)) => Ok(Value::Int(-i)),
                        (UnOp::Neg(_), Value::Float(x)) => Ok(Value::Float(-x)),
                        (UnOp::Not(_), Value::Bool(b)) => Ok(Value::Bool(!b)),
                        (UnOp::Not(_), Value::Int(i)) => Ok(Value::Int(!i)),
                        (_, v) => compile_err(format!("cannot apply this unary operator to type `{}` (E0600)", v.type_name())),
                    }
                }
            },
            Expr::Paren(p) => self.eval_expr(&p.expr),
            Expr::Group(g) => self.eval_expr(&g.expr),
            Expr::Binary(b) => self.eval_binary(b),
            Expr::Assign(a) => self.eval_assign(a),
            Expr::Macro(m) => self.eval_macro(&m.mac, self.line_of(expr)),
            Expr::Call(c) => self.eval_call(c),
            Expr::MethodCall(m) => self.eval_method_call(m),
            Expr::Index(_) | Expr::Field(_) => {
                let p = self.eval_place(expr)?.expect("index/field is a place");
                self.read_place(&p)
            }
            Expr::Reference(r) => {
                let p = self.place_or_temp(&r.expr)?;
                // Slicing a `&'static str` just narrows a pointer into the
                // binary's read-only data — still `&'static str`, borrowing
                // nothing on the stack.
                if let Some(Proj::Range(a, b)) = p.place.path.last() {
                    let mut base = p.place.clone();
                    base.path.pop();
                    if let Got::Val(Value::StaticStr(text)) = self.got(&base)? {
                        return Ok(Value::StaticStr(text[*a..*b].to_string()));
                    }
                }
                self.borrow_place(p, r.mutability.is_some())
            }
            Expr::Array(arr) => {
                let mut items = Vec::new();
                for elem in &arr.elems {
                    items.push(self.eval_expr(elem)?);
                }
                // Inline, not heap-allocated — no HeapAlloc event, since
                // nothing was allocated.
                Ok(Value::Array(items))
            }
            Expr::Repeat(r) => {
                let v = self.eval_expr(&r.expr)?;
                let n = self.eval_usize(&r.len)?;
                if !v.is_copy() && n > 1 {
                    return compile_err("`[x; N]` needs `x` to be `Copy` (E0277)");
                }
                Ok(Value::Array(vec![v; n]))
            }
            Expr::Tuple(t) => {
                if t.elems.is_empty() {
                    return Ok(Value::Unit);
                }
                let mut items = Vec::new();
                for e in &t.elems {
                    items.push(self.eval_expr(e)?);
                }
                Ok(Value::Tuple(items))
            }
            Expr::Struct(s) => self.eval_struct_lit(s),
            Expr::Block(b) => {
                if let Some(label) = &b.label {
                    let name = label.name.ident.to_string();
                    return match self.exec_block(&b.block, "block", vec![]) {
                        Err(Flow::Break(Some(l), v)) if l == name => Ok(v),
                        r => r,
                    };
                }
                self.exec_block(&b.block, "block", vec![])
            }
            Expr::If(i) => self.eval_if(i),
            Expr::While(w) => self.eval_while(w),
            Expr::Loop(l) => self.eval_loop(l),
            Expr::ForLoop(f) => self.eval_for(f),
            Expr::Match(m) => self.eval_match(m),
            Expr::Break(b) => {
                let v = match &b.expr {
                    Some(e) => self.eval_expr(e)?,
                    None => Value::Unit,
                };
                Err(Flow::Break(b.label.as_ref().map(|l| l.ident.to_string()), v))
            }
            Expr::Continue(c) => Err(Flow::Continue(c.label.as_ref().map(|l| l.ident.to_string()))),
            Expr::Return(r) => {
                let v = match &r.expr {
                    Some(e) => self.eval_expr(e)?,
                    None => Value::Unit,
                };
                Err(Flow::Return(v))
            }
            Expr::Cast(c) => {
                let v = self.eval_expr(&c.expr)?;
                let v = self.take_val(v)?;
                let ty = c.ty.to_token_stream().to_string();
                cast(v, &ty)
            }
            Expr::Range(r) => self.eval_range(r),
            Expr::Closure(c) => self.make_closure(c),
            Expr::Try(t) => {
                // `x?`: unwrap `Ok`/`Some`, or return the `Err`/`None` early.
                let v = self.eval_expr(&t.expr)?;
                match v {
                    Value::Enum { ty, variant, fields, .. } if ty == "Result" || ty == "Option" => match variant.as_str() {
                        "Ok" | "Some" => Ok(fields.into_iter().next().map(|(_, x)| x).unwrap_or(Value::Unit)),
                        _ => {
                            let early = Value::Enum { ty, variant, fields, copy: false };
                            self.pending.push(Event::Note("`?` hit an error → returning it early".into()));
                            Err(Flow::Return(early))
                        }
                    },
                    v => compile_err(format!("the `?` operator can only be applied to `Result`/`Option`, not `{}` (E0277)", v.type_name())),
                }
            }
            Expr::Let(_) => compile_err("`let` expressions are only allowed in `if let`/`while let`"),
            other => Err(self.unsupported(format!("this kind of expression ({})", expr_kind(other)))),
        }
    }

    fn eval_lit(&mut self, lit: &Lit) -> R<Value> {
        Ok(match lit {
            Lit::Int(i) => Value::Int(i.base10_parse::<i64>().map_err(|e| InterpError::Parse(e.to_string()))?),
            Lit::Float(f) => Value::Float(f.base10_parse::<f64>().map_err(|e| InterpError::Parse(e.to_string()))?),
            Lit::Bool(b) => Value::Bool(b.value),
            Lit::Str(s) => Value::StaticStr(s.value()),
            Lit::Char(c) => Value::Char(c.value()),
            Lit::Byte(b) => Value::Int(b.value() as i64),
            _ => return Err(self.unsupported("this kind of literal")),
        })
    }

    fn eval_usize(&mut self, e: &Expr) -> R<usize> {
        let v = self.eval_expr(e)?;
        match self.take_val(v)? {
            Value::Int(i) if i >= 0 => Ok(i as usize),
            Value::Int(i) => panic_err(format!("index {i} is negative")),
            v => compile_err(format!("expected an integer index, found `{}` (E0277)", v.type_name())),
        }
    }

    /// Paths that aren't local variables: constants, `None`, unit structs,
    /// unit enum variants, `i32::MAX`.
    fn eval_path_value(&mut self, path: &syn::Path) -> R<Value> {
        let segs = self.resolve_path_segments(path);
        if segs.len() > 1 && self.modules.contains_key(&segs[0]) {
            self.check_visible(&segs)?;
            let key = segs.join("::");
            if let Some(v) = self.consts.get(&key) {
                return Ok(v.clone());
            }
            if self.fns.contains_key(&key) {
                return Ok(Value::FnItem(key));
            }
        }
        match segs.as_slice() {
            [n] if n == "None" => Ok(option_none()),
            [n] => {
                if let Some(v) = self.consts.get(n) {
                    return Ok(v.clone());
                }
                if let Some(def) = self.structs.get(n)
                    && def.shape == Shape::Unit
                {
                    return Ok(Value::Struct { ty: n.clone(), fields: vec![], copy: def.derives.copy });
                }
                if self.fns.contains_key(n) || matches!(n.as_str(), "Some" | "Ok" | "Err" | "drop") {
                    return Ok(Value::FnItem(n.clone()));
                }
                Err(MemoryError::UnknownVariable(n.clone()).into())
            }
            [ty, c] => {
                let full = format!("{ty}::{c}");
                if self.fns.contains_key(&full) || matches!(full.as_str(), "String::from" | "Box::new") {
                    return Ok(Value::FnItem(full));
                }
                if let Some(def) = self.enums.get(ty) {
                    return match def.variants.iter().find(|(v, ..)| v == c) {
                        Some((_, Shape::Unit, _)) => Ok(Value::Enum { ty: ty.clone(), variant: c.clone(), fields: vec![], copy: def.derives.copy }),
                        Some(_) => Err(self.unsupported("using a tuple/struct enum variant as a function value")),
                        None => compile_err(format!("no variant `{c}` in enum `{ty}` (E0599)")),
                    };
                }
                let v = match (ty.as_str(), c.as_str()) {
                    ("i8", "MAX") => i8::MAX as i64,
                    ("i8", "MIN") => i8::MIN as i64,
                    ("u8", "MAX") => u8::MAX as i64,
                    ("i16", "MAX") => i16::MAX as i64,
                    ("u16", "MAX") => u16::MAX as i64,
                    ("i32", "MAX") => i32::MAX as i64,
                    ("i32", "MIN") => i32::MIN as i64,
                    ("u32", "MAX") => u32::MAX as i64,
                    ("i64" | "isize", "MAX") => i64::MAX,
                    ("i64" | "isize", "MIN") => i64::MIN,
                    ("u8" | "u16" | "u32" | "u64" | "usize", "MIN") => 0,
                    _ => return Err(self.unsupported(format!("the path `{ty}::{c}`"))),
                };
                Ok(Value::Int(v))
            }
            _ => Err(self.unsupported(format!("the path `{}`", segs.join("::")))),
        }
    }

    /// Follows references to the value they point at (for arithmetic and
    /// comparison operands — real Rust implements these ops for `&i32`
    /// etc. too). A `&str` slice comes back as a `StaticStr` carrying its text.
    fn deref_val(&self, mut v: Value) -> R<Value> {
        loop {
            match v {
                Value::Ref(p, _) => {
                    v = match self.got(&p)? {
                        Got::Val(x) => x,
                        Got::Str(s) => return Ok(Value::StaticStr(s)),
                        Got::Slice(items) => return Ok(Value::Array(items)),
                    }
                }
                v => return Ok(v),
            }
        }
    }

    /// `deref_val` for a freshly evaluated temporary that's consumed here —
    /// if it was a copied reference, its share of the borrow ends now.
    fn take_val(&mut self, v: Value) -> R<Value> {
        let d = self.deref_val(v.clone())?;
        if matches!(v, Value::Ref(..)) {
            self.drop_value(&v)?;
        }
        Ok(d)
    }

    /// The text of any string-ish value (`String`, `&str`, `&String`, …).
    fn text_of(&self, v: &Value) -> R<Option<String>> {
        Ok(match self.deref_val(v.clone())? {
            Value::StaticStr(s) => Some(s),
            Value::Str(id) => match self.heap.get(id)? {
                HeapObject::Str { s, .. } => Some(s.clone()),
                _ => None,
            },
            _ => None,
        })
    }

    fn eval_binary(&mut self, b: &syn::ExprBinary) -> R<Value> {
        use BinOp::*;
        match b.op {
            And(_) | Or(_) => {
                // Short-circuits, like real Rust: `i < v.len() && v[i] > 0`
                // must not index when the left side is false.
                let l = self.eval_expr(&b.left)?;
                let l = match self.take_val(l)? {
                    Value::Bool(x) => x,
                    v => return compile_err(format!("expected `bool`, found `{}` (E0308)", v.type_name())),
                };
                if matches!(b.op, And(_)) != l {
                    return Ok(Value::Bool(l));
                }
                let r = self.eval_expr(&b.right)?;
                match self.take_val(r)? {
                    Value::Bool(x) => Ok(Value::Bool(x)),
                    v => compile_err(format!("expected `bool`, found `{}` (E0308)", v.type_name())),
                }
            }
            AddAssign(_) | SubAssign(_) | MulAssign(_) | DivAssign(_) | RemAssign(_) | BitAndAssign(_) | BitOrAssign(_)
            | BitXorAssign(_) | ShlAssign(_) | ShrAssign(_) => self.eval_compound_assign(b),
            Eq(_) | Ne(_) | Lt(_) | Le(_) | Gt(_) | Ge(_) => {
                let (l, lt) = self.eval_peek(&b.left)?;
                let (r, rt) = self.eval_peek(&b.right)?;
                let out = self.binop(&b.op, l.clone(), r.clone());
                if lt {
                    self.drop_value(&l)?;
                }
                if rt {
                    self.drop_value(&r)?;
                }
                out
            }
            _ => {
                let l = self.eval_expr(&b.left)?;
                let r = self.eval_expr(&b.right)?;
                let out = self.binop(&b.op, l.clone(), r.clone());
                // Operands that were reference copies (`v + 1` with `v: &i32`,
                // `s1 + &s2`) are temporaries: their borrow ends here.
                if matches!(l, Value::Ref(..)) {
                    self.drop_value(&l)?;
                }
                if !matches!(r, Value::Int(_) | Value::Float(_)) {
                    self.drop_value(&r)?;
                }
                out
            }
        }
    }

    fn binop(&mut self, op: &BinOp, l: Value, r: Value) -> R<Value> {
        use BinOp::*;
        // `String + &str` — consumes the left String and appends into its
        // existing buffer (growing it if needed); no new String is created.
        if let (Add(_), Value::Str(id)) = (op, &l) {
            let Some(text) = self.text_of(&r)? else {
                return compile_err("can only add a `&str` to a `String` (`String + &str`) (E0308)");
            };
            self.str_append(*id, &text)?;
            return Ok(l);
        }
        let l = self.deref_val(l)?;
        let r = self.deref_val(r)?;
        let overflow = |what: &str| panic_err(format!("attempt to {what} with overflow"));
        Ok(match (op, &l, &r) {
            (Add(_), Value::Int(a), Value::Int(b)) => Value::Int(a.checked_add(*b).map_or_else(|| overflow("add"), Ok)?),
            (Sub(_), Value::Int(a), Value::Int(b)) => Value::Int(a.checked_sub(*b).map_or_else(|| overflow("subtract"), Ok)?),
            (Mul(_), Value::Int(a), Value::Int(b)) => Value::Int(a.checked_mul(*b).map_or_else(|| overflow("multiply"), Ok)?),
            (Div(_), Value::Int(_), Value::Int(0)) => return panic_err("attempt to divide by zero"),
            (Rem(_), Value::Int(_), Value::Int(0)) => return panic_err("attempt to calculate the remainder with a divisor of zero"),
            (Div(_), Value::Int(a), Value::Int(b)) => Value::Int(a / b),
            (Rem(_), Value::Int(a), Value::Int(b)) => Value::Int(a % b),
            (BitAnd(_), Value::Int(a), Value::Int(b)) => Value::Int(a & b),
            (BitOr(_), Value::Int(a), Value::Int(b)) => Value::Int(a | b),
            (BitXor(_), Value::Int(a), Value::Int(b)) => Value::Int(a ^ b),
            (Shl(_), Value::Int(a), Value::Int(b)) => Value::Int(a << b),
            (Shr(_), Value::Int(a), Value::Int(b)) => Value::Int(a >> b),
            (Add(_), Value::Float(a), Value::Float(b)) => Value::Float(a + b),
            (Sub(_), Value::Float(a), Value::Float(b)) => Value::Float(a - b),
            (Mul(_), Value::Float(a), Value::Float(b)) => Value::Float(a * b),
            (Div(_), Value::Float(a), Value::Float(b)) => Value::Float(a / b),
            (Rem(_), Value::Float(a), Value::Float(b)) => Value::Float(a % b),
            (BitAnd(_), Value::Bool(a), Value::Bool(b)) => Value::Bool(a & b),
            (BitOr(_), Value::Bool(a), Value::Bool(b)) => Value::Bool(a | b),
            (BitXor(_), Value::Bool(a), Value::Bool(b)) => Value::Bool(a ^ b),
            (Eq(_) | Ne(_) | Lt(_) | Le(_) | Gt(_) | Ge(_), _, _) => {
                let ord = self.compare(&l, &r)?;
                let Some(ord) = ord else {
                    return compile_err(format!("can't compare `{}` with `{}` (E0308/E0369)", l.type_name(), r.type_name()));
                };
                use std::cmp::Ordering::*;
                Value::Bool(match op {
                    Eq(_) => ord == Equal,
                    Ne(_) => ord != Equal,
                    Lt(_) => ord == Less,
                    Le(_) => ord != Greater,
                    Gt(_) => ord == Greater,
                    _ => ord != Less,
                })
            }
            _ => {
                return compile_err(format!(
                    "mismatched types: no implementation for `{} {} {}` (E0369/E0277)",
                    l.type_name(),
                    op.to_token_stream(),
                    r.type_name()
                ));
            }
        })
    }

    fn compare(&self, l: &Value, r: &Value) -> R<Option<std::cmp::Ordering>> {
        if let (Some(a), Some(b)) = (self.text_of(l)?, self.text_of(r)?) {
            return Ok(Some(a.cmp(&b)));
        }
        Ok(match (l, r) {
            (Value::Int(a), Value::Int(b)) => Some(a.cmp(b)),
            (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
            (Value::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
            (Value::Char(a), Value::Char(b)) => Some(a.cmp(b)),
            (Value::Unit, Value::Unit) => Some(std::cmp::Ordering::Equal),
            // Tuples/arrays/Vecs compare element by element (lexicographic),
            // so `("b", 10) > ("b", 9)`.
            (Value::Tuple(_) | Value::Array(_) | Value::Vec(_), Value::Tuple(_) | Value::Array(_) | Value::Vec(_)) => {
                let items = |v: &Value| -> R<Vec<Value>> {
                    Ok(match v {
                        Value::Tuple(x) | Value::Array(x) => x.clone(),
                        Value::Vec(id) => match self.heap.get(*id)? {
                            HeapObject::Vec { items, .. } => items.clone(),
                            _ => vec![],
                        },
                        _ => vec![],
                    })
                };
                let (a, b) = (items(l)?, items(r)?);
                for (x, y) in a.iter().zip(&b) {
                    match self.compare(&self.deref_val(x.clone())?, &self.deref_val(y.clone())?)? {
                        Some(std::cmp::Ordering::Equal) => {}
                        o => return Ok(o),
                    }
                }
                Some(a.len().cmp(&b.len()))
            }
            (Value::Struct { ty: a, .. }, Value::Struct { ty: b, .. }) | (Value::Enum { ty: a, .. }, Value::Enum { ty: b, .. }) if a != b => None,
            // Structural equality for compound values. Their derived
            // `Debug` text is a faithful structural fingerprint.
            (a, b) if std::mem::discriminant(a) == std::mem::discriminant(b) || matches!((a, b), (Value::Vec(_) | Value::Array(_), Value::Vec(_) | Value::Array(_))) => {
                let (da, db) = (debug_value(&self.stack, &self.heap, a, false), debug_value(&self.stack, &self.heap, b, false));
                if da == db { Some(std::cmp::Ordering::Equal) } else { Some(da.cmp(&db)).filter(|o| o.is_ne()) }
            }
            _ => None,
        })
    }

    fn eval_assign(&mut self, a: &syn::ExprAssign) -> R<Value> {
        if let Expr::Infer(_) = &*a.left {
            let v = self.eval_expr(&a.right)?;
            self.drop_value(&v)?;
            return Ok(Value::Unit);
        }
        let val = self.eval_expr(&a.right)?;
        let Some(p) = self.eval_place(&a.left)? else {
            return compile_err("invalid left-hand side of assignment (E0070)");
        };
        let whole = p.via.is_none() && p.place.path.is_empty();
        self.check_write(&p, whole)?;
        let (was_moved, expired, moved_paths) = {
            let b = self.stack.binding(p.place.frame, p.place.slot).unwrap();
            (b.state == OwnershipState::Moved, b.expired, b.moved_paths.clone())
        };
        let slot = resolve_mut(&mut self.stack, &mut self.heap, &p.place)?;
        let old = std::mem::replace(slot, val);
        // RAII: overwriting an owned value drops it right here, same as
        // real Rust running the old value's destructor at the point of
        // reassignment rather than waiting for the binding's scope to end.
        let skip_drop = whole && (was_moved || (expired && matches!(old, Value::Ref(..))));
        let prev_keep = std::mem::replace(&mut self.keep_refs, whole && expired);
        if !skip_drop {
            let rel: Vec<Vec<Proj>> = moved_paths.iter().filter_map(|m| m.strip_prefix(p.place.path.as_slice()).map(<[Proj]>::to_vec)).collect();
            let had_heap = { let mut ids = Vec::new(); owned_heap_ids(&self.heap, &old, &mut ids); !ids.is_empty() };
            let r = self.drop_value_except(&old, &rel, &mut Vec::new());
            self.keep_refs = prev_keep;
            r?;
            if had_heap {
                self.pending.push(Event::Note(format!("the old value of `{}` is dropped right here, at the assignment", p.place.describe())));
            }
        }
        self.keep_refs = prev_keep;
        if p.via.is_none() {
            let b = self.stack.binding_mut(p.place.frame, p.place.slot).unwrap();
            if whole {
                b.state = OwnershipState::Owned;
                b.expired = false;
                b.deferred = false;
                b.moved_paths.clear();
            } else {
                b.moved_paths.retain(|m| !m.starts_with(&p.place.path));
            }
        }
        self.pending.push(Event::Assign { name: p.place.describe() });
        Ok(Value::Unit)
    }

    fn eval_compound_assign(&mut self, b: &syn::ExprBinary) -> R<Value> {
        use BinOp::*;
        let rhs = self.eval_expr(&b.right)?;
        let Some(p) = self.eval_place(&b.left)? else {
            return compile_err("invalid left-hand side of assignment (E0067)");
        };
        self.check_write(&p, false)?;
        let cur = match self.got(&p.place)? {
            Got::Val(v) => v,
            _ => return compile_err("cannot assign to a slice"),
        };
        if let (AddAssign(_), Value::Str(id)) = (&b.op, &cur) {
            let Some(text) = self.text_of(&rhs)? else {
                return compile_err("`String += x` needs `x` to be a `&str` (E0308)");
            };
            self.str_append(*id, &text)?;
            self.drop_value(&rhs)?;
            self.pending.push(Event::Mutate { name: p.place.describe() });
            return Ok(Value::Unit);
        }
        let op = match b.op {
            AddAssign(t) => Add(syn::Token![+](t.spans[0])),
            SubAssign(t) => Sub(syn::Token![-](t.spans[0])),
            MulAssign(t) => Mul(syn::Token![*](t.spans[0])),
            DivAssign(t) => Div(syn::Token![/](t.spans[0])),
            RemAssign(t) => Rem(syn::Token![%](t.spans[0])),
            BitAndAssign(t) => BitAnd(syn::Token![&](t.spans[0])),
            BitOrAssign(t) => BitOr(syn::Token![|](t.spans[0])),
            BitXorAssign(t) => BitXor(syn::Token![^](t.spans[0])),
            ShlAssign(t) => Shl(syn::Token![<<]([t.spans[0], t.spans[1]])),
            ShrAssign(t) => Shr(syn::Token![>>]([t.spans[0], t.spans[1]])),
            _ => unreachable!(),
        };
        let new = self.binop(&op, cur, rhs)?;
        *resolve_mut(&mut self.stack, &mut self.heap, &p.place)? = new;
        self.pending.push(Event::Assign { name: p.place.describe() });
        Ok(Value::Unit)
    }

    // ───────────────────────── heap buffers ────────────────────────

    fn alloc_str(&mut self, s: String) -> Value {
        let cap = s.len();
        let id = self.heap.alloc(HeapObject::Str { s, cap });
        if cap > 0 {
            self.pending.push(Event::HeapAlloc { id, what: format!("String buffer, {cap} bytes") });
        }
        Value::Str(id)
    }

    fn alloc_vec(&mut self, items: Vec<Value>, cap: usize) -> Value {
        let id = self.heap.alloc(HeapObject::Vec { items, cap });
        if cap > 0 {
            self.pending.push(Event::HeapAlloc { id, what: format!("Vec buffer, room for {cap} element(s)") });
        }
        Value::Vec(id)
    }

    /// Makes room for `needed` elements/bytes, growing exactly like real
    /// `RawVec::grow_amortized`: `max(2 * cap, needed, MIN_NON_ZERO_CAP)`,
    /// where the minimum is 8 for byte buffers (`String`) and 4 for most
    /// element types. Verified against rustc's `.capacity()` (see NOTES.md).
    fn reserve(&mut self, id: HeapId, needed: usize) -> R<()> {
        let (cap, min) = match self.heap.get_mut(id)? {
            HeapObject::Vec { cap, .. } => (cap, 4),
            HeapObject::Str { cap, .. } => (cap, 8),
            HeapObject::Box(_) | HeapObject::Map { .. } => return Ok(()),
        };
        if needed <= *cap {
            return Ok(());
        }
        let old = *cap;
        let new = (old * 2).max(needed).max(min);
        *cap = new;
        self.pending.push(if old == 0 {
            Event::HeapAlloc { id, what: format!("first buffer, capacity {new}") }
        } else {
            Event::HeapRealloc { id, old_cap: old, new_cap: new }
        });
        Ok(())
    }

    fn str_append(&mut self, id: HeapId, text: &str) -> R<()> {
        let len = match self.heap.get(id)? {
            HeapObject::Str { s, .. } => s.len(),
            _ => 0,
        };
        self.reserve(id, len + text.len())?;
        if let HeapObject::Str { s, .. } = self.heap.get_mut(id)? {
            s.push_str(text);
        }
        Ok(())
    }

    /// `.clone()`: a deep copy — every heap buffer gets a brand-new
    /// allocation, sized exactly to the contents (capacity == len).
    fn deep_clone(&mut self, v: &Value) -> R<Value> {
        Ok(match v {
            Value::Str(id) => {
                let s = match self.heap.get(*id)? {
                    HeapObject::Str { s, .. } => s.clone(),
                    _ => String::new(),
                };
                self.alloc_str(s)
            }
            Value::Vec(id) => {
                let items = match self.heap.get(*id)? {
                    HeapObject::Vec { items, .. } => items.clone(),
                    _ => vec![],
                };
                let mut cloned = Vec::new();
                for x in &items {
                    cloned.push(self.deep_clone(x)?);
                }
                let n = cloned.len();
                self.alloc_vec(cloned, n)
            }
            Value::Map(id) => {
                // A clone gets a same-sized table (same bucket count).
                let (entries, buckets) = match self.heap.get(*id)? {
                    HeapObject::Map { entries, buckets } => (entries.clone(), *buckets),
                    _ => (vec![], 0),
                };
                let mut cloned = Vec::new();
                for (k, x) in &entries {
                    cloned.push((self.deep_clone(k)?, self.deep_clone(x)?));
                }
                let id = self.heap.alloc(HeapObject::Map { entries: cloned, buckets });
                if buckets > 0 {
                    self.pending.push(Event::HeapAlloc { id, what: format!("hash table (cloned), {buckets} buckets") });
                }
                Value::Map(id)
            }
            Value::Box(id) => {
                let inner = match self.heap.get(*id)? {
                    HeapObject::Box(x) => x.clone(),
                    _ => Value::Unit,
                };
                let inner = self.deep_clone(&inner)?;
                let id = self.heap.alloc(HeapObject::Box(inner));
                self.pending.push(Event::HeapAlloc { id, what: "Box (cloned)".into() });
                Value::Box(id)
            }
            Value::Struct { ty, fields, copy } | Value::Enum { ty, fields, copy, .. } => {
                let clonable = match (self.structs.get(ty), self.enums.get(ty)) {
                    (Some(d), _) => d.derives.clone,
                    (_, Some(d)) => d.derives.clone,
                    _ => true, // Option/Result
                };
                if !clonable {
                    return compile_err(format!("no method named `clone` found for `{ty}` — add #[derive(Clone)] (E0599)"));
                }
                let mut out = Vec::new();
                for (n, x) in fields {
                    out.push((n.clone(), self.deep_clone(x)?));
                }
                match v {
                    Value::Enum { variant, .. } => Value::Enum { ty: ty.clone(), variant: variant.clone(), fields: out, copy: *copy },
                    _ => Value::Struct { ty: ty.clone(), fields: out, copy: *copy },
                }
            }
            Value::Array(items) | Value::Tuple(items) => {
                let mut out = Vec::new();
                for x in items {
                    out.push(self.deep_clone(x)?);
                }
                if matches!(v, Value::Array(_)) { Value::Array(out) } else { Value::Tuple(out) }
            }
            Value::Ref(..) => {
                self.on_copy(v);
                v.clone()
            }
            Value::Closure { id, captures, modes, kind } => {
                let mut out = Vec::new();
                for (n, x) in captures {
                    if matches!(x, Value::Ref(_, RefKind::Mutable)) {
                        return compile_err("a closure that captured by `&mut` can't be cloned (E0599)");
                    }
                    out.push((n.clone(), self.deep_clone(x)?));
                }
                Value::Closure { id: *id, captures: out, modes: modes.clone(), kind: *kind }
            }
            Value::Iter(_) => return Err(self.unsupported("cloning an iterator")),
            other => other.clone(),
        })
    }

    // ──────────────────────────── macros ───────────────────────────

    fn eval_macro(&mut self, mac: &syn::Macro, line: usize) -> R<Value> {
        let name = mac.path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default();
        let parse_args = || {
            mac.parse_body_with(syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated)
                .map_err(|e| Flow::Err(InterpError::Parse(e.to_string())))
        };
        match name.as_str() {
            "println" | "print" | "format" | "panic" | "eprintln" | "eprint" => {
                let args: Vec<Expr> = parse_args()?.into_iter().collect();
                let text = if args.is_empty() { String::new() } else { self.format_macro_args(&args)? };
                match name.as_str() {
                    "println" => {
                        self.output.push_str(&text);
                        self.output.push('\n');
                    }
                    "print" => self.output.push_str(&text),
                    "format" => return Ok(self.alloc_str(text)),
                    "panic" => return panic_err(if args.is_empty() { "explicit panic".to_string() } else { text }),
                    _ => self.pending.push(Event::Note(format!("(stderr) {text}"))),
                }
                Ok(Value::Unit)
            }
            "vec" => {
                // `vec![x; n]` vs `vec![a, b, c]`.
                if let Ok((elem, n)) = mac.parse_body_with(|input: syn::parse::ParseStream| {
                    let e: Expr = input.parse()?;
                    input.parse::<syn::Token![;]>()?;
                    let n: Expr = input.parse()?;
                    Ok((e, n))
                }) {
                    let v = self.eval_expr(&elem)?;
                    let n = self.eval_usize(&n)?;
                    let mut items = Vec::new();
                    for _ in 0..n {
                        items.push(self.deep_clone(&v)?);
                    }
                    self.drop_value(&v)?;
                    return Ok(self.alloc_vec(items, n));
                }
                let mut items = Vec::new();
                for e in &parse_args()? {
                    items.push(self.eval_expr(e)?);
                }
                let n = items.len();
                Ok(self.alloc_vec(items, n))
            }
            "assert" | "assert_eq" | "assert_ne" => {
                let args: Vec<Expr> = parse_args()?.into_iter().collect();
                let ok = if name == "assert" {
                    let c = self.eval_expr(args.first().ok_or_else(|| self.unsupported("assert! with no condition"))?)?;
                    matches!(self.take_val(c)?, Value::Bool(true))
                } else {
                    let (l, _) = self.eval_peek(&args[0])?;
                    let (r, _) = self.eval_peek(&args[1])?;
                    let eq = self.compare(&self.deref_val(l.clone())?, &self.deref_val(r.clone())?)? == Some(std::cmp::Ordering::Equal);
                    if eq != (name == "assert_eq") {
                        let (ld, rd) = (debug_value(&self.stack, &self.heap, &l, false), debug_value(&self.stack, &self.heap, &r, false));
                        let op = if name == "assert_eq" { "==" } else { "!=" };
                        return panic_err(format!("assertion `left {op} right` failed\n  left: {ld}\n right: {rd}"));
                    }
                    true
                };
                if !ok {
                    let msg = if args.len() > 1 { self.format_macro_args(&args[1..])? } else { format!("assertion failed: {}", args[0].to_token_stream()) };
                    return panic_err(msg);
                }
                Ok(Value::Unit)
            }
            "write" | "writeln" => {
                let args: Vec<Expr> = parse_args()?.into_iter().collect();
                let Some(dest) = args.first() else { return compile_err("`write!` needs a destination") };
                let mut text = if args.len() > 1 { self.format_macro_args(&args[1..])? } else { String::new() };
                if name == "writeln" {
                    text.push('\n');
                }
                let (d, temp) = self.eval_peek(dest)?;
                match self.deref_val(d.clone())? {
                    Value::Formatter(i) => self.fmt_bufs[i].push_str(&text),
                    Value::Str(id) => {
                        // `write!` into a `String` (needs `use std::fmt::Write`).
                        if let Some(p) = self.eval_place(dest)? {
                            self.check_write(&p, false)?;
                        }
                        self.str_append(id, &text)?;
                        self.pending.push(Event::Mutate { name: dest.to_token_stream().to_string() });
                    }
                    v => return compile_err(format!("cannot `write!` into a `{}` (E0599)", v.type_name())),
                }
                if temp {
                    self.drop_value(&d)?;
                }
                Ok(result_ok())
            }
            "unreachable" => panic_err("internal error: entered unreachable code"),
            "todo" => panic_err("not yet implemented"),
            other => Err(Flow::Err(InterpError::Unsupported { what: format!("the {other}! macro"), line })),
        }
    }

    /// `println!`-family formatting: `{}`, `{0}`, `{name}` (named args and
    /// inline captures), and specs like `{:?}`, `{:#?}`, `{:>5}`, `{:.2}`.
    /// Arguments are only *borrowed* — `println!("{}", s)` never moves `s`.
    fn format_macro_args(&mut self, args: &[Expr]) -> R<String> {
        let fmt = match &args[0] {
            Expr::Lit(ExprLit { lit: Lit::Str(s), .. }) => s.value(),
            // This matches a real rustc requirement, not a gap in this
            // interpreter: the first argument must be a literal format
            // string, checked before the program even runs.
            _ => {
                return compile_err(
                    "format argument must be a string literal (e.g. println!(\"{}\", x) instead of println!(x)) — rustc requires this",
                );
            }
        };
        let pieces = parse_format(&fmt).map_err(|e| Flow::Err(InterpError::CompileError(e)))?;
        let mut positional = Vec::new();
        let mut named = HashMap::new();
        let mut temps = Vec::new();
        for e in &args[1..] {
            if let Expr::Assign(a) = e
                && let Expr::Path(p) = &*a.left
                && let Some(n) = path_ident(&p.path)
            {
                let (v, t) = self.peek_ref(&a.right)?;
                if t {
                    temps.push(v.clone());
                }
                named.insert(n, v);
            } else {
                let (v, t) = self.peek_ref(e)?;
                if t {
                    temps.push(v.clone());
                }
                positional.push(v);
            }
        }
        let mut out = String::new();
        let mut next = 0;
        for piece in &pieces {
            match piece {
                Piece::Lit(s) => out.push_str(s),
                Piece::Arg { arg, spec } => {
                    let v = if arg.is_empty() {
                        next += 1;
                        positional.get(next - 1).cloned()
                    } else if let Ok(i) = arg.parse::<usize>() {
                        positional.get(i).cloned()
                    } else if let Some(v) = named.get(arg) {
                        Some(v.clone())
                    } else {
                        let (f, s) = self.stack.lookup_slot(arg).ok_or_else(|| MemoryError::UnknownVariable(arg.clone()))?;
                        self.access(f, s, &[], Access::Read)?;
                        Some(Value::Ref(Place { frame: f, slot: s, name: arg.clone(), path: vec![], reborrow: true }, RefKind::Shared))
                    };
                    let v = v.ok_or_else(|| Flow::Err(InterpError::CompileError(format!("format string has more placeholders than arguments: `{fmt}`"))))?;
                    out.push_str(&self.render(&v, spec)?);
                }
            }
        }
        for t in temps {
            self.drop_value(&t)?;
        }
        Ok(out)
    }

    fn render(&mut self, v: &Value, spec: &Spec) -> R<String> {
        // A user `impl Display`/`impl Debug`: run their `fmt`. Width/fill
        // are deliberately ignored, as in real Rust — std only pads when
        // the impl itself calls `f.pad(..)`, which `write!` doesn't.
        let trait_name = if spec.is_debug() { "Debug" } else { "Display" };
        if let Some(text) = self.user_fmt(v, trait_name)? {
            return Ok(text);
        }
        if spec.is_debug() {
            self.check_debug(v)?;
            let s = debug_value(&self.stack, &self.heap, v, spec.alt);
            return Ok(pad(s, spec, false));
        }
        let target = self.deref_val(v.clone())?;
        let target = match target {
            Value::Box(id) => match self.heap.get(id)? {
                HeapObject::Box(inner) => self.deref_val(inner.clone())?,
                _ => target,
            },
            t => t,
        };
        let numeric = matches!(target, Value::Int(_) | Value::Float(_));
        let s = match (&target, spec.ty.as_str()) {
            (Value::Int(i), "x") => if spec.alt { format!("{i:#x}") } else { format!("{i:x}") },
            (Value::Int(i), "X") => if spec.alt { format!("{i:#X}") } else { format!("{i:X}") },
            (Value::Int(i), "b") => if spec.alt { format!("{i:#b}") } else { format!("{i:b}") },
            (Value::Int(i), "o") => if spec.alt { format!("{i:#o}") } else { format!("{i:o}") },
            (Value::Float(x), "e") => format!("{x:e}"),
            (Value::Float(x), "") => match spec.precision {
                Some(p) => format!("{x:.p$}"),
                None => x.to_string(),
            },
            (Value::Int(_) | Value::Bool(_) | Value::Char(_) | Value::Str(_) | Value::StaticStr(_), "") => {
                let s = display_value(&self.stack, &self.heap, &target);
                match spec.precision {
                    Some(p) if !numeric => s.chars().take(p).collect(),
                    _ => s,
                }
            }
            (t, "") => {
                return compile_err(format!(
                    "`{}` doesn't implement `std::fmt::Display` — use `{{:?}}` (with #[derive(Debug)] on your own types) instead (E0277)",
                    t.type_name()
                ));
            }
            (_, ty) => return Err(self.unsupported(format!("the `{{:{ty}}}` format type"))),
        };
        Ok(pad(s, spec, numeric))
    }

    /// Formats `v` with the program's own `impl <trait_name> for T`, if it
    /// has one — a real call to `T::fmt`, with its own stack frame.
    fn user_fmt(&mut self, v: &Value, trait_name: &str) -> R<Option<String>> {
        let target = self.deref_val(v.clone())?;
        let (Value::Struct { ty, .. } | Value::Enum { ty, .. }) = &target else { return Ok(None) };
        let Some(def) = self.fns.get(&format!("{ty}::{trait_name}::fmt")).cloned() else { return Ok(None) };
        self.pending.push(Event::Note(format!("formatting a `{ty}` calls your `impl {trait_name} for {ty}` → `{ty}::fmt`")));
        // `self` is a reference to the value: to the real variable when the
        // argument was one. A temporary (`println!("{}", Shape::Circle(3))`)
        // gets a hidden slot that's never dropped here — the temporary's
        // own drop happens at the end of the statement as usual.
        let mut real = None;
        let mut cur = v.clone();
        while let Value::Ref(p, _) = &cur {
            real = Some(p.clone());
            cur = match self.got(p)? {
                Got::Val(x) => x,
                _ => break,
            };
        }
        let (alias, place) = match real {
            Some(p) => (None, p),
            None => {
                let a = self.temp_place(target.clone());
                (Some(a.clone()), a.place)
            }
        };
        let self_ref = Value::Ref(Place { reborrow: true, ..place }, RefKind::Shared);
        self.fmt_bufs.push(String::new());
        let fid = self.fmt_bufs.len() - 1;
        let line = self.cur_line;
        let r = self.call_fn(def, Some(self_ref), vec![Value::Formatter(fid)], line);
        let text = self.fmt_bufs.pop().unwrap_or_default();
        if let Some(a) = alias
            && let Some(b) = self.stack.binding_mut(a.place.frame, a.place.slot)
        {
            b.state = OwnershipState::Moved;
        }
        match r? {
            Value::Enum { variant, .. } if variant == "Err" => panic_err(format!("a {trait_name} implementation returned an error unexpectedly")),
            _ => Ok(Some(text)),
        }
    }

    fn check_debug(&self, v: &Value) -> R<()> {
        match v {
            Value::Closure { .. } => compile_err("closures don't implement `Debug` (E0277)"),
            Value::Struct { ty, fields, .. } | Value::Enum { ty, fields, .. } => {
                let ok = match (self.structs.get(ty), self.enums.get(ty)) {
                    (Some(d), _) => d.derives.debug,
                    (_, Some(d)) => d.derives.debug,
                    _ => true,
                } || self.fns.contains_key(&format!("{ty}::Debug::fmt"));
                if !ok {
                    return compile_err(format!("`{ty}` doesn't implement `Debug` — add #[derive(Debug)] above `{ty}` (E0277)"));
                }
                fields.iter().try_for_each(|(_, x)| self.check_debug(x))
            }
            Value::Array(items) | Value::Tuple(items) => items.iter().try_for_each(|x| self.check_debug(x)),
            Value::Vec(id) => match self.heap.get(*id)? {
                HeapObject::Vec { items, .. } => items.iter().try_for_each(|x| self.check_debug(x)),
                _ => Ok(()),
            },
            Value::Box(id) => match self.heap.get(*id)? {
                HeapObject::Box(x) => self.check_debug(x),
                _ => Ok(()),
            },
            Value::Map(id) => match self.heap.get(*id)? {
                HeapObject::Map { entries, .. } => entries.iter().try_for_each(|(k, x)| self.check_debug(k).and_then(|_| self.check_debug(x))),
                _ => Ok(()),
            },
            Value::Ref(p, _) => match self.got(p)? {
                Got::Val(x) => self.check_debug(&x),
                Got::Slice(items) => items.iter().try_for_each(|x| self.check_debug(x)),
                Got::Str(_) => Ok(()),
            },
            _ => Ok(()),
        }
    }

    // ───────────────────────── calls/methods ────────────────────────

    fn eval_call(&mut self, call: &syn::ExprCall) -> R<Value> {
        let line = self.line_of(call);
        // Calling something held in a variable or field — a closure or a
        // function value: `f(3)`, `(self.op)(x)`.
        let via_value = match &*call.func {
            Expr::Path(p) => path_ident(&p.path).is_some_and(|n| self.stack.lookup_slot(&n).is_some()),
            _ => true,
        };
        if via_value {
            let callee = self.place_or_temp(&call.func)?;
            let mut args = Vec::new();
            for a in &call.args {
                args.push(self.eval_arg(a)?);
            }
            return self.call_callable_place(callee, args, line);
        }
        let Expr::Path(p) = &*call.func else { unreachable!() };
        let segs = self.resolve_path_segments(&p.path);
        self.check_visible(&segs)?;
        let mut args = Vec::new();
        for a in &call.args {
            args.push(self.eval_arg(a)?);
        }
        self.call_path(segs, args, line)
    }

    /// Calls a function by path — user-defined, a supported std function,
    /// a tuple-struct/enum-variant constructor.
    fn call_path(&mut self, segs: Vec<String>, args: Vec<Value>, line: usize) -> R<Value> {
        let path = segs.join("::");
        match path.as_str() {
            "String::from" => {
                let arg = args.first().cloned().unwrap_or(Value::Unit);
                let Some(text) = self.text_of(&arg)? else {
                    return compile_err("`String::from` expects a `&str` (E0277)");
                };
                self.drop_value(&arg)?;
                return Ok(self.alloc_str(text));
            }
            "String::new" => {
                self.pending.push(Event::Note("String::new() doesn't allocate yet — capacity 0, no heap buffer".into()));
                return Ok(self.alloc_str(String::new()));
            }
            "Vec::new" => {
                self.pending.push(Event::Note("Vec::new() doesn't allocate yet — capacity 0, no heap buffer".into()));
                return Ok(self.alloc_vec(vec![], 0));
            }
            "String::with_capacity" | "Vec::with_capacity" => {
                let n = match self.deref_val(args.first().cloned().unwrap_or(Value::Int(0)))? {
                    Value::Int(n) if n >= 0 => n as usize,
                    _ => return compile_err("capacity must be a non-negative integer"),
                };
                if path.starts_with("Vec") {
                    return Ok(self.alloc_vec(vec![], n));
                }
                let id = self.heap.alloc(HeapObject::Str { s: String::new(), cap: n });
                if n > 0 {
                    self.pending.push(Event::HeapAlloc { id, what: format!("String buffer, {n} bytes (empty)") });
                }
                return Ok(Value::Str(id));
            }
            "HashMap::new" => {
                self.pending.push(Event::Note("HashMap::new() doesn't allocate yet — capacity 0, no heap table".into()));
                return Ok(self.map_new(0));
            }
            "HashMap::with_capacity" => {
                let n = self.arg_usize(&args, 0)?;
                return Ok(self.map_new(n));
            }
            "HashMap::from" => {
                let v = args.into_iter().next().unwrap_or(Value::Unit);
                let mut node = self.node_from(v)?;
                let m = self.map_new(0);
                let Value::Map(id) = m else { unreachable!() };
                self.map_extend(id, &mut node, line)?;
                self.drop_value(&Value::Iter(Box::new(node)))?;
                return Ok(m);
            }
            "Box::new" => {
                let v = args.into_iter().next().unwrap_or(Value::Unit);
                let desc = debug_value(&self.stack, &self.heap, &v, false);
                let id = self.heap.alloc(HeapObject::Box(v));
                self.pending.push(Event::HeapAlloc { id, what: format!("Box holding {desc}") });
                return Ok(Value::Box(id));
            }
            "drop" | "mem::drop" => {
                let v = args.into_iter().next().unwrap_or(Value::Unit);
                self.pending.push(Event::Note("drop(...) — the value is moved into `drop`, which ends its life right now".into()));
                self.drop_value(&v)?;
                return Ok(Value::Unit);
            }
            "Some" | "Ok" | "Err" => {
                let v = args.into_iter().next().unwrap_or(Value::Unit);
                let ty = if path == "Some" { "Option" } else { "Result" };
                let copy = v.is_copy();
                return Ok(Value::Enum { ty: ty.into(), variant: path, fields: vec![("0".into(), v)], copy });
            }
            _ => {}
        }
        if let Some(def) = self.fns.get(&path).cloned() {
            return self.call_fn(def, None, args, line);
        }
        if let [s] = segs.as_slice()
            && let Some(def) = self.structs.get(s)
            && def.shape == Shape::Tuple
        {
            if args.len() != def.fields.len() {
                return compile_err(format!("`{s}` takes {} field(s) but {} were supplied (E0061)", def.fields.len(), args.len()));
            }
            let fields = def.fields.iter().cloned().zip(args).collect();
            return Ok(Value::Struct { ty: s.clone(), fields, copy: def.derives.copy });
        }
        if let [ty, var] = segs.as_slice()
            && let Some(def) = self.enums.get(ty)
            && let Some((_, Shape::Tuple, names)) = def.variants.iter().find(|(v, ..)| v == var)
        {
            let fields = names.iter().cloned().zip(args).collect();
            return Ok(Value::Enum { ty: ty.clone(), variant: var.clone(), fields, copy: def.derives.copy });
        }
        Err(self.unsupported(format!("calling `{path}` (not a function defined in this program or a supported std function)")))
    }

    /// Calls a user-defined function: push a frame, move/copy each argument
    /// into its parameter, run the body, move the result back out, pop the
    /// frame (dropping whatever the parameters still own).
    fn call_fn(&mut self, def: Rc<FnDef>, self_arg: Option<Value>, args: Vec<Value>, call_line: usize) -> R<Value> {
        let fn_frames = self.stack.frames.iter().filter(|f| !f.is_scope).count();
        if fn_frames >= MAX_CALL_DEPTH {
            return panic_err(format!("stack overflow — more than {MAX_CALL_DEPTH} nested calls (infinite recursion?)"));
        }
        let typed: Vec<&syn::PatType> = def.sig.inputs.iter().filter_map(|a| if let FnArg::Typed(t) = a { Some(t) } else { None }).collect();
        if typed.len() != args.len() {
            return compile_err(format!("`{}` takes {} argument(s) but {} were supplied (E0061)", def.name, typed.len(), args.len()));
        }
        let inv = self.next_inv;
        self.next_inv += 1;
        self.stack.push_frame(def.name.clone());
        self.frame_inv.push(inv);
        let frame_idx = self.stack.frames.len() - 1;
        self.pending.push(Event::FrameEnter { name: def.name.clone() });
        let saved_offset = self.line_offset;
        self.line_offset = def.line_base;
        self.self_tys.push(def.self_ty.clone());
        self.mod_stack.push(def.module.clone());

        let result = (|| -> R<Value> {
            if let Some(sv) = self_arg {
                let mutable = def.sig.inputs.iter().any(|a| matches!(a, FnArg::Receiver(r) if r.mutability.is_some() && !matches!(r.kind, syn::ReceiverKind::Reference(..))));
                self.declare("self".into(), sv, mutable);
            }
            for (pt, v) in typed.iter().zip(args) {
                self.bind_value(&pt.pat, v)?;
            }
            self.record_step(call_line);
            self.cur_line = self.line_of(&def.block);
            match self.exec_stmts(&def.block.stmts) {
                Ok(v) | Err(Flow::Return(v)) => Ok(v),
                Err(Flow::Break(..) | Flow::Continue(_)) => compile_err("`break`/`continue` outside of a loop (E0268)"),
                Err(e) => Err(e),
            }
        })();
        let result = match result {
            Ok(v) => v,
            Err(e) => {
                self.line_offset = saved_offset;
                return Err(e);
            }
        };
        if let Some(p) = ref_into_frames(&result, frame_idx) {
            self.line_offset = saved_offset;
            return compile_err(format!(
                "`{}` returns a reference to `{}`, a local that's dropped when the function returns — it would dangle (E0106/E0515)",
                def.name, p.name
            ));
        }
        let close = self.close_line(&def.block);
        let mut ids = Vec::new();
        owned_heap_ids(&self.heap, &result, &mut ids);
        if !ids.is_empty() {
            self.pending.push(Event::Note(format!(
                "`{}` returns its result by move — the heap data isn't copied, ownership just passes to the caller",
                def.name
            )));
        }
        self.pop_frame(close)?;
        self.self_tys.pop();
        self.mod_stack.pop();
        self.line_offset = saved_offset;
        self.cur_line = call_line;
        Ok(result)
    }

    /// Binds a value moved into a parameter/`for` variable to a pattern.
    fn bind_value(&mut self, pat: &Pat, v: Value) -> R<()> {
        let (pat, _) = strip_type(pat);
        match pat {
            Pat::Ident(p) if p.subpat.is_none() && p.by_ref.is_none() => {
                self.declare(p.ident.to_string(), v, p.mutability.is_some());
                Ok(())
            }
            Pat::Wild(_) => self.drop_value(&v),
            _ => {
                let subject = self.temp_place(v);
                let mut binds = Vec::new();
                if !self.match_pat(pat, &subject, None, &mut binds)? {
                    return compile_err("refutable pattern in a parameter/`for` binding (E0005)");
                }
                for (n, v, m) in self.commit(binds)? {
                    self.declare(n, v, m);
                }
                Ok(())
            }
        }
    }

    fn eval_method_call(&mut self, m: &syn::ExprMethodCall) -> R<Value> {
        let line = self.line_of(m);
        let method = m.method.to_string();
        let recv = self.place_or_temp(&m.receiver)?;
        let recv = self.auto_deref(recv)?;
        let got = self.got(&recv.place)?;
        if let Got::Val(Value::Struct { ty, .. } | Value::Enum { ty, .. }) = &got
            && let Some((def, _)) = self.find_method(ty, &method)
        {
            let mut args = Vec::new();
            for a in &m.args {
                args.push(self.eval_arg(a)?);
            }
            let self_val = match def.sig.inputs.first() {
                Some(FnArg::Receiver(r)) => match &r.kind {
                    syn::ReceiverKind::Reference(_, _, mutability) => self.borrow_place(recv, mutability.is_some())?,
                    _ => self.read_place(&recv)?,
                },
                _ => return compile_err(format!("`{ty}::{method}` is an associated function, not a method — call it as `{ty}::{method}(...)` (E0599)")),
            };
            return self.call_fn(def, Some(self_val), args, line);
        }
        // Iterators, closures-taking methods (`Option::map`, `Vec::retain`…),
        // and iterator sources (`.iter()`, `.chars()`, `.as_bytes()`…).
        let turbofish = m.turbofish.as_ref().map(|t| t.to_token_stream().to_string());
        if let Some(v) = self.map_method(&method, &recv, &got, &m.args, line)? {
            return Ok(v);
        }
        if let Some(v) = self.iter_method(&method, &recv, &got, &m.args, turbofish, line)? {
            return Ok(v);
        }
        let mut args = Vec::new();
        for a in &m.args {
            args.push(self.eval_arg(a)?);
        }
        // Builtins only read their reference arguments, except the ones
        // that store or hand back an argument (or release it themselves).
        let borrowed: Vec<Value> = if matches!(method.as_str(), "push" | "insert" | "unwrap_or" | "push_str") {
            vec![]
        } else {
            args.iter().filter(|a| matches!(a, Value::Ref(..))).cloned().collect()
        };
        let out = self.builtin_method(&method, recv, got, args)?;
        for a in &borrowed {
            self.drop_value(a)?;
        }
        Ok(out)
    }

    fn arg_usize(&self, args: &[Value], i: usize) -> R<usize> {
        match self.deref_val(args.get(i).cloned().unwrap_or(Value::Unit))? {
            Value::Int(n) if n >= 0 => Ok(n as usize),
            v => compile_err(format!("expected a `usize` argument, found `{}` (E0308)", v.type_name())),
        }
    }

    fn builtin_method(&mut self, method: &str, recv: PlaceRef, got: Got, args: Vec<Value>) -> R<Value> {
        let name = recv.place.describe();
        let read = |this: &mut Self| -> R<()> {
            if recv.via.is_none() {
                this.access(recv.place.frame, recv.place.slot, &recv.place.path, Access::Read)?;
            }
            Ok(())
        };
        let no_method = |t: &str| compile_err(format!("no method named `{method}` found for `{t}` (or rust-viz doesn't model it yet) (E0599)"));
        match (&got, method) {
            // ── read-only, any string-ish/collection
            (_, "len") | (_, "is_empty") => {
                read(self)?;
                let n = self.len_of(&recv.place)?;
                Ok(if method == "len" { Value::Int(n as i64) } else { Value::Bool(n == 0) })
            }
            (Got::Val(Value::Vec(id) | Value::Str(id)), "capacity") => {
                read(self)?;
                Ok(Value::Int(match self.heap.get(*id)? {
                    HeapObject::Vec { cap, .. } | HeapObject::Str { cap, .. } => *cap as i64,
                    _ => 0,
                }))
            }
            (_, "clone") => {
                read(self)?;
                match got {
                    Got::Val(v) => self.deep_clone(&v),
                    Got::Str(s) => Ok(Value::StaticStr(s)),
                    Got::Slice(_) => Err(self.unsupported("cloning a slice")),
                }
            }
            (Got::Val(Value::Str(_) | Value::StaticStr(_)) | Got::Str(_), "to_string" | "to_owned" | "into") => {
                read(self)?;
                let t = match &got {
                    Got::Str(s) => s.clone(),
                    Got::Val(v) => self.text_of(v)?.unwrap_or_default(),
                    _ => unreachable!(),
                };
                Ok(self.alloc_str(t))
            }
            (Got::Val(Value::Formatter(i)), "write_str") => {
                let i = *i;
                let text = match args.first() {
                    Some(a) => self.text_of(a)?.unwrap_or_default(),
                    None => String::new(),
                };
                self.fmt_bufs[i].push_str(&text);
                Ok(result_ok())
            }
            (Got::Val(v @ (Value::Struct { .. } | Value::Enum { .. })), "to_string") => {
                let ty = v.type_name();
                let v = Value::Ref(Place { reborrow: true, ..recv.place.clone() }, RefKind::Shared);
                match self.user_fmt(&v, "Display")? {
                    Some(t) => Ok(self.alloc_str(t)),
                    None => compile_err(format!("`{ty}` doesn't implement `Display`, so it has no `to_string` (E0599)")),
                }
            }
            (Got::Val(v @ (Value::Int(_) | Value::Float(_) | Value::Bool(_) | Value::Char(_))), "to_string") => {
                let t = display_value(&self.stack, &self.heap, v);
                Ok(self.alloc_str(t))
            }
            (Got::Val(Value::Str(_)), "as_str") | (Got::Val(Value::Vec(_)), "as_slice") => {
                let len = self.len_of(&recv.place)?;
                let mut p = recv;
                push_proj(&mut p.place, Proj::Range(0, len));
                self.borrow_place(p, false)
            }
            (Got::Val(Value::Str(_) | Value::StaticStr(_)) | Got::Str(_), "to_uppercase" | "to_lowercase" | "trim" | "contains" | "starts_with" | "ends_with") => {
                read(self)?;
                let t = match &got {
                    Got::Str(s) => s.clone(),
                    Got::Val(v) => self.text_of(v)?.unwrap_or_default(),
                    _ => unreachable!(),
                };
                match method {
                    "to_uppercase" => Ok(self.alloc_str(t.to_uppercase())),
                    "to_lowercase" => Ok(self.alloc_str(t.to_lowercase())),
                    "trim" => {
                        let start = t.len() - t.trim_start().len();
                        let end = t.trim_end().len();
                        let mut p = recv;
                        push_proj(&mut p.place, Proj::Range(start, end.max(start)));
                        self.borrow_place(p, false)
                    }
                    _ => {
                        let pat = match args.first() {
                            Some(Value::Char(c)) => c.to_string(),
                            Some(a) => self.text_of(a)?.unwrap_or_default(),
                            None => String::new(),
                        };
                        Ok(Value::Bool(match method {
                            "contains" => t.contains(&pat),
                            "starts_with" => t.starts_with(&pat),
                            _ => t.ends_with(&pat),
                        }))
                    }
                }
            }
            (Got::Val(Value::Vec(_) | Value::Array(_)) | Got::Slice(_), "contains") => {
                read(self)?;
                let items = self.items_of(&got)?;
                let needle = self.deref_val(args.first().cloned().unwrap_or(Value::Unit))?;
                for x in items {
                    let x = self.deref_val(x)?;
                    if self.compare(&x, &needle)? == Some(std::cmp::Ordering::Equal) {
                        return Ok(Value::Bool(true));
                    }
                }
                Ok(Value::Bool(false))
            }
            // ── mutating String
            (Got::Val(Value::Str(id)), "push_str" | "push") => {
                let id = *id;
                self.check_write(&recv, false)?;
                let text = match args.first() {
                    Some(Value::Char(c)) if method == "push" => c.to_string(),
                    Some(a) if method == "push_str" => match self.text_of(a)? {
                        Some(t) => t,
                        None => return compile_err("`push_str` expects a `&str` (E0308)"),
                    },
                    _ => return compile_err(format!("wrong argument type for `String::{method}` (E0308)")),
                };
                self.str_append(id, &text)?;
                if let Some(a) = args.first() {
                    self.drop_value(a)?;
                }
                self.pending.push(Event::Mutate { name });
                Ok(Value::Unit)
            }
            (Got::Val(Value::Str(id)), "pop" | "clear") => {
                let id = *id;
                self.check_write(&recv, false)?;
                let HeapObject::Str { s, .. } = self.heap.get_mut(id)? else { unreachable!() };
                let out = if method == "pop" { s.pop().map(Value::Char).map_or_else(option_none, option_some) } else {
                    s.clear();
                    Value::Unit
                };
                self.pending.push(Event::Mutate { name });
                Ok(out)
            }
            // ── mutating Vec
            (Got::Val(Value::Vec(id)), "push" | "insert") => {
                let id = *id;
                self.check_write(&recv, false)?;
                let len = match self.heap.get(id)? {
                    HeapObject::Vec { items, .. } => items.len(),
                    _ => 0,
                };
                let (index, v) = if method == "push" {
                    (len, args.into_iter().next().unwrap_or(Value::Unit))
                } else {
                    let i = self.arg_usize(&args, 0)?;
                    if i > len {
                        return panic_err(format!("insertion index (is {i}) should be <= len (is {len})"));
                    }
                    (i, args.into_iter().nth(1).unwrap_or(Value::Unit))
                };
                self.reserve(id, len + 1)?;
                if let HeapObject::Vec { items, .. } = self.heap.get_mut(id)? {
                    items.insert(index, v);
                }
                self.pending.push(Event::Mutate { name });
                Ok(Value::Unit)
            }
            (Got::Val(Value::Vec(id)), "pop" | "remove" | "clear" | "truncate") => {
                let id = *id;
                self.check_write(&recv, false)?;
                let i = if matches!(method, "remove" | "truncate") { Some(self.arg_usize(&args, 0)?) } else { None };
                let HeapObject::Vec { items, .. } = self.heap.get_mut(id)? else { unreachable!() };
                let len = items.len();
                let (out, dropped) = match method {
                    "pop" => (items.pop().map_or_else(option_none, option_some), vec![]),
                    "remove" => {
                        let i = i.unwrap();
                        if i >= len {
                            return panic_err(format!("removal index (is {i}) should be < len (is {len})"));
                        }
                        (items.remove(i), vec![])
                    }
                    "truncate" => (Value::Unit, items.split_off(i.unwrap().min(len))),
                    _ => (Value::Unit, std::mem::take(items)),
                };
                for d in &dropped {
                    self.drop_value(d)?;
                }
                if !dropped.is_empty() {
                    self.pending.push(Event::Note("removed elements are dropped; the buffer keeps its capacity".into()));
                }
                self.pending.push(Event::Mutate { name });
                Ok(out)
            }
            // ── Option
            (Got::Val(Value::Enum { ty, variant, fields, .. }), "is_some" | "is_none" | "is_ok" | "is_err") if ty == "Option" || ty == "Result" => {
                read(self)?;
                let _ = fields;
                Ok(Value::Bool(match method {
                    "is_some" => variant == "Some",
                    "is_none" => variant == "None",
                    "is_ok" => variant == "Ok",
                    _ => variant == "Err",
                }))
            }
            (Got::Val(Value::Enum { ty, .. }), "copied" | "cloned") if ty == "Option" => {
                let Value::Enum { fields, .. } = self.read_place(&recv)? else { unreachable!() };
                Ok(match fields.into_iter().next() {
                    Some((_, x)) => {
                        let t = self.deref_val(x.clone())?;
                        let t = if method == "cloned" { self.deep_clone(&t)? } else { t };
                        self.drop_value(&x)?;
                        option_some(t)
                    }
                    None => option_none(),
                })
            }
            (Got::Val(Value::Enum { ty, .. }), "unwrap" | "expect" | "unwrap_or") if ty == "Option" || ty == "Result" => {
                let v = self.read_place(&recv)?;
                let Value::Enum { variant, fields, .. } = v else { unreachable!() };
                if variant == "Some" || variant == "Ok" {
                    for a in &args {
                        self.drop_value(a)?;
                    }
                    return Ok(fields.into_iter().next().map(|(_, x)| x).unwrap_or(Value::Unit));
                }
                match method {
                    "unwrap_or" => Ok(args.into_iter().next().unwrap_or(Value::Unit)),
                    "expect" => {
                        let msg = args.first().map(|a| self.text_of(a)).transpose()?.flatten().unwrap_or_default();
                        panic_err(msg)
                    }
                    _ if variant == "None" => panic_err("called `Option::unwrap()` on a `None` value"),
                    _ => panic_err(format!("called `Result::unwrap()` on an `Err` value: {}", debug_value(&self.stack, &self.heap, &fields[0].1, false))),
                }
            }
            // ── chars
            (Got::Val(Value::Char(c)), _) => {
                let c = *c;
                Ok(match method {
                    "is_whitespace" => Value::Bool(c.is_whitespace()),
                    "is_alphabetic" => Value::Bool(c.is_alphabetic()),
                    "is_numeric" => Value::Bool(c.is_numeric()),
                    "is_alphanumeric" => Value::Bool(c.is_alphanumeric()),
                    "is_uppercase" => Value::Bool(c.is_uppercase()),
                    "is_lowercase" => Value::Bool(c.is_lowercase()),
                    "is_ascii_digit" => Value::Bool(c.is_ascii_digit()),
                    "is_ascii_punctuation" => Value::Bool(c.is_ascii_punctuation()),
                    "is_ascii" => Value::Bool(c.is_ascii()),
                    "to_ascii_uppercase" => Value::Char(c.to_ascii_uppercase()),
                    "to_ascii_lowercase" => Value::Char(c.to_ascii_lowercase()),
                    "len_utf8" => Value::Int(c.len_utf8() as i64),
                    "to_digit" => {
                        let radix = self.arg_usize(&args, 0)? as u32;
                        c.to_digit(radix).map(|d| Value::Int(d as i64)).map_or_else(option_none, option_some)
                    }
                    _ => return no_method("char"),
                })
            }
            // ── numbers
            (Got::Val(Value::Int(i)), _) => {
                let i = *i;
                let a = || args.first().cloned().unwrap_or(Value::Unit);
                Ok(Value::Int(match (method, self.deref_val(a())?) {
                    ("abs", _) => i.abs(),
                    ("pow", Value::Int(e)) => i.checked_pow(e as u32).map_or_else(|| panic_err("attempt to multiply with overflow"), Ok)?,
                    ("min", Value::Int(b)) => i.min(b),
                    ("max", Value::Int(b)) => i.max(b),
                    _ => return no_method("integer"),
                }))
            }
            (Got::Val(Value::Float(x)), _) => {
                let x = *x;
                let a = self.deref_val(args.first().cloned().unwrap_or(Value::Unit))?;
                Ok(Value::Float(match (method, a) {
                    ("sqrt", _) => x.sqrt(),
                    ("abs", _) => x.abs(),
                    ("round", _) => x.round(),
                    ("floor", _) => x.floor(),
                    ("ceil", _) => x.ceil(),
                    ("powi", Value::Int(e)) => x.powi(e as i32),
                    ("powf", Value::Float(e)) => x.powf(e),
                    ("min", Value::Float(b)) => x.min(b),
                    ("max", Value::Float(b)) => x.max(b),
                    _ => return no_method("float"),
                }))
            }
            (Got::Val(v), _) => no_method(&v.type_name()),
            (Got::Str(_), _) => no_method("&str"),
            (Got::Slice(_), _) => no_method("slice"),
        }
    }

    fn items_of(&self, got: &Got) -> R<Vec<Value>> {
        Ok(match got {
            Got::Slice(items) | Got::Val(Value::Array(items)) => items.clone(),
            Got::Val(Value::Vec(id)) => match self.heap.get(*id)? {
                HeapObject::Vec { items, .. } => items.clone(),
                _ => vec![],
            },
            _ => vec![],
        })
    }

    fn eval_struct_lit(&mut self, s: &syn::ExprStruct) -> R<Value> {
        let segs = self.resolve_path_segments(&s.path);
        let (ty, variant, expected, copy) = match segs.as_slice() {
            [ty] => match self.structs.get(ty) {
                Some(d) => (ty.clone(), None, d.fields.clone(), d.derives.copy),
                None => return compile_err(format!("cannot find struct `{ty}` in this scope (E0422)")),
            },
            [ty, var] => match self.enums.get(ty).and_then(|d| d.variants.iter().find(|(v, ..)| v == var).map(|(_, _, f)| (f.clone(), d.derives.copy))) {
                Some((f, copy)) => (ty.clone(), Some(var.clone()), f, copy),
                None => return compile_err(format!("cannot find `{}` (E0422)", segs.join("::"))),
            },
            _ => return Err(self.unsupported("this struct path")),
        };
        let mut given: HashMap<String, Value> = HashMap::new();
        for fv in &s.fields {
            let n = member_name(&fv.member);
            if !expected.contains(&n) {
                return compile_err(format!("struct `{ty}` has no field named `{n}` (E0560)"));
            }
            let v = self.eval_expr(&fv.expr)?;
            given.insert(n, v);
        }
        // `..base`: the remaining fields are moved/copied out of `base`
        // one by one — a *partial* move, so `base`'s other fields stay usable.
        let base = match &s.rest {
            Some(r) => Some(self.place_or_temp(r)?),
            None => None,
        };
        let mut fields = Vec::new();
        for n in &expected {
            let v = match given.remove(n) {
                Some(v) => v,
                None => match &base {
                    Some(b) => {
                        let mut fp = b.clone();
                        fp.place.path.push(Proj::Field(n.clone()));
                        self.read_place(&fp)?
                    }
                    None => return compile_err(format!("missing field `{n}` in initializer of `{ty}` (E0063)")),
                },
            };
            fields.push((n.clone(), v));
        }
        Ok(match variant {
            Some(variant) => Value::Enum { ty, variant, fields, copy },
            None => Value::Struct { ty, fields, copy },
        })
    }

    // ───────────────────────── control flow ────────────────────────

    fn eval_if(&mut self, e: &syn::ExprIf) -> R<Value> {
        let line = self.line_of(e);
        self.cur_line = line;
        let has_else = e.else_branch.is_some();
        let else_part = |this: &mut Self| -> R<Value> {
            match &e.else_branch {
                Some((_, else_expr)) => match &**else_expr {
                    Expr::Block(b) => this.exec_block(&b.block, "else", vec![]),
                    Expr::If(i) => this.eval_if(i),
                    other => this.eval_expr(other),
                },
                None => Ok(Value::Unit),
            }
        };
        if let Expr::Let(l) = &*e.cond {
            let mark = self.temps.len();
            let subject = self.place_or_temp(&l.expr)?;
            let mut binds = Vec::new();
            let matched = self.match_pat(&l.pat, &subject, None, &mut binds)?;
            let pat = l.pat.to_token_stream().to_string();
            self.pending.push(Event::Note(if matched {
                format!("if let: matches `{pat}` → taking the if branch")
            } else {
                format!("if let: doesn't match `{pat}` → {}", if has_else { "taking the else branch" } else { "skipping the block" })
            }));
            self.record_step(line);
            let r = if matched {
                let vals = self.commit(binds)?;
                self.exec_block(&e.then_branch, "if let", vals)
            } else {
                else_part(self)
            };
            self.drop_temps_from(mark)?;
            return r;
        }
        let c = self.eval_expr(&e.cond)?;
        let c = match self.take_val(c)? {
            Value::Bool(b) => b,
            v => return compile_err(format!("mismatched types: `if` condition must be a `bool`, found `{}` (E0308)", v.type_name())),
        };
        self.pending.push(Event::Note(format!(
            "if condition is {c} → {}",
            if c { "taking the if branch" } else if has_else { "taking the else branch" } else { "skipping the block" }
        )));
        self.record_step(line);
        if c { self.exec_block(&e.then_branch, "if", vec![]) } else { else_part(self) }
    }

    /// Runs one loop-body iteration; `Some(v)` means the loop is done.
    fn loop_body(&mut self, body: &Block, scope: &str, binds: Vec<(String, Value, bool)>, label: &Option<String>) -> R<Option<Value>> {
        match self.exec_block(body, scope, binds) {
            Ok(v) => {
                self.drop_value(&v)?;
                Ok(None)
            }
            Err(Flow::Break(l, v)) if l.is_none() || l == *label => Ok(Some(v)),
            Err(Flow::Continue(l)) if l.is_none() || l == *label => Ok(None),
            Err(e) => {
                // Unwinding past this loop (break/continue to an outer
                // label, return): the body's scope is still on the stack.
                if !matches!(e, Flow::Err(_)) && self.stack.frames.last().is_some_and(|f| f.is_scope && f.name == scope) {
                    self.pop_frame(self.close_line(body))?;
                }
                Err(e)
            }
        }
    }

    fn eval_while(&mut self, w: &syn::ExprWhile) -> R<Value> {
        let line = self.line_of(w);
        let label = w.label.as_ref().map(|l| l.name.ident.to_string());
        let tracked = self.push_live(w);
        let live_len = self.live.len();
        let r = (|| -> R<Value> {
            loop {
                self.check_step_budget()?;
                self.cur_line = line;
                let mark = self.temps.len();
                let binds = if let Expr::Let(l) = &*w.cond {
                    let subject = self.place_or_temp(&l.expr)?;
                    let mut binds = Vec::new();
                    let matched = self.match_pat(&l.pat, &subject, None, &mut binds)?;
                    let pat = l.pat.to_token_stream().to_string();
                    self.pending.push(Event::Note(if matched { format!("while let: matches `{pat}` → run the body") } else { format!("while let: doesn't match `{pat}` → loop ends") }));
                    self.record_step(line);
                    if !matched {
                        self.drop_temps_from(mark)?;
                        return Ok(Value::Unit);
                    }
                    self.commit(binds)?
                } else {
                    let c = self.eval_expr(&w.cond)?;
                    let c = match self.take_val(c)? {
                        Value::Bool(b) => b,
                        v => return compile_err(format!("`while` condition must be a `bool`, found `{}` (E0308)", v.type_name())),
                    };
                    self.drop_temps_from(mark)?;
                    self.pending.push(Event::Note(format!("while condition is {c} → {}", if c { "run the body" } else { "loop ends" })));
                    self.record_step(line);
                    if !c {
                        return Ok(Value::Unit);
                    }
                    vec![]
                };
                let done = self.loop_body(&w.body, "while body", binds, &label)?;
                self.drop_temps_from(mark)?;
                self.live.truncate(live_len);
                self.expire_borrows();
                if done.is_some() {
                    return Ok(Value::Unit);
                }
            }
        })();
        if tracked {
            self.live.truncate(live_len - 1);
        }
        r
    }

    fn eval_loop(&mut self, l: &syn::ExprLoop) -> R<Value> {
        let label = l.label.as_ref().map(|l| l.name.ident.to_string());
        let tracked = self.push_live(l);
        let live_len = self.live.len();
        let r = (|| -> R<Value> {
            loop {
                self.check_step_budget()?;
                if let Some(v) = self.loop_body(&l.body, "loop body", vec![], &label)? {
                    return Ok(v);
                }
                self.expire_borrows();
            }
        })();
        if tracked {
            self.live.truncate(live_len - 1);
        }
        r
    }

    fn eval_for(&mut self, f: &syn::ExprForLoop) -> R<Value> {
        let line = self.line_of(f);
        self.cur_line = line;
        let label = f.label.as_ref().map(|l| l.name.ident.to_string());
        let (mut src, enumerate) = self.iter_source(&f.expr)?;
        let n = match &src {
            IterSrc::Ints(v) => Some(v.len()),
            IterSrc::Chars(v) => Some(v.len()),
            IterSrc::Owned { items, .. } => Some(items.len()),
            IterSrc::Refs { indices, .. } => Some(indices.len()),
            IterSrc::Lazy(_) => None,
        };
        // The iterator itself holds the borrow for the whole loop — which is
        // why `v.push(..)` inside `for x in &v` is a borrow error.
        let mut iter_borrow = None;
        if let IterSrc::Refs { base, kind, .. } = &src
            && base.via.is_none() {
                let mutable = *kind == RefKind::Mutable;
                let r = self.borrow_place(base.clone(), mutable)?;
                self.pending.push(Event::Note(format!(
                    "the for loop's iterator {} `{}` until the loop ends",
                    if mutable { "mutably borrows" } else { "borrows" },
                    base.place.describe()
                )));
                iter_borrow = Some(r);
            }
        let tracked = self.push_live(f);
        let live_len = self.live.len();
        let pat_text = f.pat.to_token_stream().to_string();
        let mut consumed = 0;
        let r = (|| -> R<Value> {
            for i in 0.. {
                if n.is_some_and(|n| i >= n) {
                    break;
                }
                self.check_step_budget()?;
                let item = match &mut src {
                    IterSrc::Lazy(node) => match self.iter_next(node, line)? {
                        Some(x) => x,
                        None => break,
                    },
                    IterSrc::Ints(v) => Value::Int(v[i]),
                    IterSrc::Chars(v) => Value::Char(v[i]),
                    IterSrc::Owned { items, .. } => items[i].clone(),
                    IterSrc::Refs { base, indices, kind } => {
                        let mut p = base.place.clone();
                        push_proj(&mut p, Proj::Index(indices[i]));
                        p.reborrow = true;
                        Value::Ref(p, *kind)
                    }
                };
                consumed = i + 1;
                let item = if enumerate { Value::Tuple(vec![Value::Int(i as i64), item]) } else { item };
                let shown = debug_value(&self.stack, &self.heap, &item, false);
                let of = n.map(|n| format!(" of {n}")).unwrap_or_default();
                self.pending.push(Event::Note(format!("for: iteration {}{of} — `{pat_text}` = {shown}", i + 1)));
                self.cur_line = line;
                self.record_step(line);
                self.push_scope("for body");
                self.bind_value(&f.pat, item)?;
                let binds: Vec<(String, Value, bool)> = vec![];
                // bind_value already declared into the scope; run the body in it.
                let r = self.exec_stmts(&f.body.stmts);
                let done = match r {
                    Ok(v) => {
                        self.drop_value(&v)?;
                        false
                    }
                    Err(Flow::Break(l, _)) if l.is_none() || l == label => true,
                    Err(Flow::Continue(l)) if l.is_none() || l == label => false,
                    Err(Flow::Err(e)) => return Err(Flow::Err(e)),
                    Err(e) => {
                        self.pop_frame(self.close_line(&f.body))?;
                        return Err(e);
                    }
                };
                drop(binds);
                self.pop_frame(self.close_line(&f.body))?;
                self.live.truncate(live_len);
                self.expire_borrows();
                if done {
                    break;
                }
            }
            Ok(Value::Unit)
        })();
        if tracked {
            self.live.truncate(live_len - 1);
        }
        if r.is_ok() || matches!(r, Err(Flow::Return(_) | Flow::Break(..) | Flow::Continue(_))) {
            if let Some(b) = iter_borrow {
                self.drop_value(&b)?;
            }
            if let IterSrc::Owned { items, buffer } = &src {
                for x in &items[consumed..] {
                    self.drop_value(x)?;
                }
                if let Some(id) = buffer {
                    self.free(*id);
                }
            }
            if let IterSrc::Lazy(node) = src {
                self.drop_value(&Value::Iter(node))?;
            }
            if !self.pending.is_empty() {
                self.pending.insert(0, Event::Note("for loop finished".into()));
                self.record_step(line);
            }
        }
        r
    }

    fn iter_source(&mut self, expr: &Expr) -> R<(IterSrc, bool)> {
        match expr {
            Expr::Paren(p) => self.iter_source(&p.expr),
            Expr::Range(r) => {
                let start = match &r.start {
                    Some(e) => self.eval_int(e)?,
                    None => return Err(self.unsupported("`for` over a range with no start")),
                };
                let Some(end) = &r.end else { return Err(self.unsupported("`for` over an unbounded range")) };
                let end = self.eval_int(end)?;
                let end = if matches!(r.limits, RangeLimits::Closed(_)) { end + 1 } else { end };
                Ok((IterSrc::Ints((start..end).collect()), false))
            }
            Expr::MethodCall(m) => match m.method.to_string().as_str() {
                "rev" => {
                    let (src, e) = self.iter_source(&m.receiver)?;
                    Ok((
                        match src {
                            IterSrc::Ints(mut v) => {
                                v.reverse();
                                IterSrc::Ints(v)
                            }
                            IterSrc::Chars(mut v) => {
                                v.reverse();
                                IterSrc::Chars(v)
                            }
                            IterSrc::Owned { mut items, buffer } => {
                                items.reverse();
                                IterSrc::Owned { items, buffer }
                            }
                            IterSrc::Refs { base, mut indices, kind } => {
                                indices.reverse();
                                IterSrc::Refs { base, indices, kind }
                            }
                            IterSrc::Lazy(node) => IterSrc::Lazy(Box::new(crate::value::IterNode::Rev(node))),
                        },
                        e,
                    ))
                }
                "enumerate" => Ok((self.iter_source(&m.receiver)?.0, true)),
                "iter" | "iter_mut" => {
                    let base = self.place_or_temp(&m.receiver)?;
                    let base = self.auto_deref(base)?;
                    let kind = if m.method == "iter_mut" { RefKind::Mutable } else { RefKind::Shared };
                    self.refs_source(base, kind)
                }
                "into_iter" => self.iter_source(&m.receiver),
                "chars" => {
                    let base = self.place_or_temp(&m.receiver)?;
                    let base = self.auto_deref(base)?;
                    let t = match self.got(&base.place)? {
                        Got::Str(s) => s,
                        Got::Val(v) => self.text_of(&v)?.ok_or_else(|| self.unsupported("`.chars()` on a non-string"))?,
                        Got::Slice(_) => return compile_err("no method `chars` on a slice"),
                    };
                    Ok((IterSrc::Chars(t.chars().collect()), false))
                }
                _ => {
                    let v = self.eval_expr(expr)?;
                    self.iter_src_from_value(v)
                }
            },
            Expr::Reference(r) => {
                let base = self.place_or_temp(&r.expr)?;
                let base = self.auto_deref(base)?;
                let kind = if r.mutability.is_some() { RefKind::Mutable } else { RefKind::Shared };
                self.refs_source(base, kind)
            }
            e => {
                let v = self.eval_expr(e)?;
                match v {
                    Value::Vec(id) => {
                        let items = match self.heap.get(id)? {
                            HeapObject::Vec { items, .. } => items.clone(),
                            _ => vec![],
                        };
                        self.pending.push(Event::Note("the for loop consumes the Vec: each element is moved out in turn, and the buffer is freed when the loop ends".into()));
                        Ok((IterSrc::Owned { items, buffer: Some(id) }, false))
                    }
                    Value::Array(items) => Ok((IterSrc::Owned { items, buffer: None }, false)),
                    Value::Ref(p, kind) => {
                        let base = self.auto_deref(PlaceRef { place: p, via: Some(kind) })?;
                        self.refs_source(base, kind)
                    }
                    Value::Iter(node) => Ok((IterSrc::Lazy(node), false)),
                    v => compile_err(format!("`{}` is not an iterator (E0277)", v.type_name())),
                }
            }
        }
    }

    fn refs_source(&mut self, base: PlaceRef, kind: RefKind) -> R<(IterSrc, bool)> {
        if let Got::Val(Value::Map(_)) = self.got(&base.place)? {
            let node = self.map_iter(base, kind == RefKind::Mutable, crate::value::MapPart::Both)?;
            return Ok((IterSrc::Lazy(Box::new(node)), false));
        }
        let len = self.len_of(&base.place)?;
        if matches!(self.got(&base.place)?, Got::Str(_) | Got::Val(Value::Str(_) | Value::StaticStr(_))) {
            return compile_err("`&str`/`String` is not an iterator — use `.chars()` (E0277)");
        }
        Ok((IterSrc::Refs { base, indices: (0..len).collect(), kind }, false))
    }

    fn eval_int(&mut self, e: &Expr) -> R<i64> {
        let v = self.eval_expr(e)?;
        match self.take_val(v)? {
            Value::Int(i) => Ok(i),
            v => compile_err(format!("expected an integer, found `{}` (E0308)", v.type_name())),
        }
    }

    fn eval_match(&mut self, m: &syn::ExprMatch) -> R<Value> {
        let line = self.line_of(m);
        let mark = self.temps.len();
        let subject = self.place_or_temp(&m.expr)?;
        for arm in &m.arms {
            let (pat, guard) = match &arm.pat {
                Pat::Guard(g) => (&*g.pat, Some(&g.guard)),
                p => (p, None),
            };
            let mut binds = Vec::new();
            if !self.match_pat(pat, &subject, None, &mut binds)? {
                continue;
            }
            if let Some(guard) = guard {
                // The guard sees the bindings by reference; nothing is
                // moved unless the arm is actually taken.
                self.push_scope("match guard");
                for b in &binds {
                    let v = match b.by_ref {
                        Some(kind) => Value::Ref(Place { reborrow: true, ..b.src.place.clone() }, kind),
                        None => match self.got(&b.src.place)? {
                            Got::Val(v) => v,
                            _ => continue,
                        },
                    };
                    self.stack.current_frame_mut().declare(b.name.clone(), v, false);
                }
                let g = self.eval_expr(guard);
                self.pop_frame_silently();
                if !matches!(self.take_val(g?)?, Value::Bool(true)) {
                    continue;
                }
            }
            self.pending.push(Event::Note(format!("match: arm `{}` matches", arm.pat.to_token_stream())));
            self.cur_line = line;
            self.record_step(line);
            let vals = self.commit(binds)?;
            self.push_scope("match arm");
            for (n, v, mu) in vals {
                self.declare(n, v, mu);
            }
            let r = match &*arm.body {
                Expr::Block(b) if b.label.is_none() => self.exec_stmts(&b.block.stmts),
                e => self.eval_expr(e),
            };
            if let Err(Flow::Err(_)) = r {
                return r;
            }
            if let Ok(v) = &r
                && let Some(p) = ref_into_frames(v, self.stack.frames.len() - 1)
            {
                return compile_err(format!("`{}` does not live long enough (E0597)", p.name));
            }
            let end = self.line_of(&arm.body) + arm.body.to_token_stream().to_string().matches('\n').count();
            self.pop_frame(end)?;
            self.drop_temps_from(mark)?;
            return r;
        }
        compile_err("non-exhaustive patterns: no `match` arm matched this value (E0004)")
    }

    // ─────────────────────────── patterns ──────────────────────────

    /// Does `pat` match the value at `subject`? Collects the bindings it
    /// would create (not yet committed). `default_ref` is the binding mode
    /// once matching has gone through a reference (match ergonomics: a
    /// `Some(x)` pattern against `&Option<String>` binds `x` as `&String`).
    fn match_pat(&mut self, pat: &Pat, subject: &PlaceRef, default_ref: Option<RefKind>, out: &mut Vec<PatBind>) -> R<bool> {
        let got = self.got(&subject.place)?;
        // A bare identifier naming a unit variant (`None`, an imported
        // `Nil`) is a variant pattern, not a new binding.
        let variant_ident = match pat {
            Pat::Ident(p) if p.subpat.is_none() && p.by_ref.is_none() => {
                p.ident == "None" || self.resolve_path_segments(&syn::Path::from(p.ident.clone())).len() == 2
            }
            _ => false,
        };
        // Auto-deref for non-binding patterns.
        if let Got::Val(Value::Ref(target, kind)) = &got
            && (variant_ident || !matches!(pat, Pat::Ident(_) | Pat::Wild(_) | Pat::Reference(_) | Pat::Type(_)))
        {
            let next = PlaceRef { place: target.clone(), via: Some(combine(subject.via, *kind)) };
            let mode = Some(match default_ref {
                Some(RefKind::Shared) => RefKind::Shared,
                _ => *kind,
            });
            return self.match_pat(pat, &next, mode, out);
        }
        // Deref coercion (`&Box<List>` passed where `&List` is expected)
        // means a structural pattern sees through a `Box`.
        if let Got::Val(Value::Box(_)) = &got
            && (variant_ident || !matches!(pat, Pat::Ident(_) | Pat::Wild(_) | Pat::Type(_)))
        {
            let mut next = subject.clone();
            next.place.path.push(Proj::Deref);
            return self.match_pat(pat, &next, default_ref, out);
        }
        let sub = |p: &PlaceRef, f: String| {
            let mut n = p.clone();
            n.place.path.push(Proj::Field(f));
            n
        };
        match pat {
            Pat::Wild(_) | Pat::Rest(_) => Ok(true),
            Pat::Paren(p) => self.match_pat(&p.pat, subject, default_ref, out),
            Pat::Type(t) => self.match_pat(&t.pat, subject, default_ref, out),
            Pat::Ident(p) => {
                let name = p.ident.to_string();
                if name == "None" && p.subpat.is_none() {
                    return Ok(matches!(&got, Got::Val(Value::Enum { variant, .. }) if variant == "None"));
                }
                if p.subpat.is_none() && p.by_ref.is_none() {
                    let segs = self.resolve_path_segments(&syn::Path::from(p.ident.clone()));
                    if segs.len() == 2 {
                        return Ok(self.variant_matches(&got, &segs));
                    }
                }
                if let Some((_, sp)) = &p.subpat
                    && !self.match_pat(sp, subject, default_ref, out)?
                {
                    return Ok(false);
                }
                let by_ref = match &p.by_ref {
                    Some(_) => Some(if p.mutability.is_some() { RefKind::Mutable } else { RefKind::Shared }),
                    None => default_ref,
                };
                out.push(PatBind { name, mutable: p.mutability.is_some() && p.by_ref.is_none(), by_ref, src: subject.clone() });
                Ok(true)
            }
            Pat::Lit(l) => {
                let v = self.eval_lit(&l.lit)?;
                let Got::Val(cur) = &got else {
                    return Ok(matches!((&got, &v), (Got::Str(s), Value::StaticStr(t)) if s == t));
                };
                let cur = self.deref_val(cur.clone())?;
                Ok(self.compare(&cur, &v)? == Some(std::cmp::Ordering::Equal))
            }
            Pat::Range(r) => {
                let Got::Val(cur) = got else { return Ok(false) };
                let lo = match &r.start {
                    Some(e) => Some(self.eval_expr(e)?),
                    None => None,
                };
                let hi = match &r.end {
                    Some(e) => Some(self.eval_expr(e)?),
                    None => None,
                };
                use std::cmp::Ordering::*;
                let above = match &lo {
                    Some(lo) => matches!(self.compare(&cur, lo)?, Some(Greater | Equal)),
                    None => true,
                };
                let below = match &hi {
                    Some(hi) => matches!((self.compare(&cur, hi)?, &r.limits), (Some(Less), _) | (Some(Equal), RangeLimits::Closed(_))),
                    None => true,
                };
                Ok(above && below)
            }
            Pat::Or(o) => {
                for case in &o.cases {
                    let mut tmp = Vec::new();
                    if self.match_pat(case, subject, default_ref, &mut tmp)? {
                        out.extend(tmp);
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Pat::Tuple(t) => {
                let Got::Val(Value::Tuple(items)) = &got else { return Ok(false) };
                let n = items.len();
                self.match_seq(t.elems.iter(), n, subject, default_ref, out, &sub)
            }
            Pat::TupleStruct(ts) => {
                let segs = self.resolve_path_segments(&ts.path);
                if !self.variant_matches(&got, &segs) {
                    return Ok(false);
                }
                let n = match &got {
                    Got::Val(Value::Struct { fields, .. } | Value::Enum { fields, .. }) => fields.len(),
                    _ => 0,
                };
                self.match_seq(ts.elems.iter(), n, subject, default_ref, out, &sub)
            }
            Pat::Struct(ps) => {
                let segs = self.resolve_path_segments(&ps.path);
                if !self.variant_matches(&got, &segs) {
                    return Ok(false);
                }
                for fp in &ps.fields {
                    let next = sub(subject, member_name(&fp.member));
                    if !self.match_pat(&fp.pat, &next, default_ref, out)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            Pat::Path(pp) => {
                let segs = self.resolve_path_segments(&pp.path);
                if let Some(c) = self.consts.get(&segs.join("::")).cloned()
                {
                    let Got::Val(cur) = got else { return Ok(false) };
                    return Ok(self.compare(&cur, &c)? == Some(std::cmp::Ordering::Equal));
                }
                Ok(self.variant_matches(&got, &segs))
            }
            Pat::Reference(r) => {
                let Got::Val(Value::Ref(target, kind)) = &got else { return Ok(false) };
                let next = PlaceRef { place: target.clone(), via: Some(combine(subject.via, *kind)) };
                self.match_pat(&r.pat, &next, None, out)
            }
            _ => Err(self.unsupported(format!("this pattern (`{}`)", pat.to_token_stream()))),
        }
    }

    fn match_seq<'p>(
        &mut self,
        elems: impl Iterator<Item = &'p Pat>,
        n: usize,
        subject: &PlaceRef,
        default_ref: Option<RefKind>,
        out: &mut Vec<PatBind>,
        sub: &dyn Fn(&PlaceRef, String) -> PlaceRef,
    ) -> R<bool> {
        let elems: Vec<&Pat> = elems.collect();
        let rest = elems.iter().position(|p| matches!(p, Pat::Rest(_)));
        for (i, p) in elems.iter().enumerate() {
            if matches!(p, Pat::Rest(_)) {
                continue;
            }
            let idx = match rest {
                Some(r) if i > r => n - (elems.len() - i),
                _ => i,
            };
            if idx >= n {
                return compile_err("this pattern has more fields than the value (E0023)");
            }
            if !self.match_pat(p, &sub(subject, idx.to_string()), default_ref, out)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn variant_matches(&self, got: &Got, segs: &[String]) -> bool {
        let want = segs.last().map(String::as_str).unwrap_or("");
        match got {
            Got::Val(Value::Enum { ty, variant, .. }) => variant == want && (segs.len() == 1 || segs[segs.len() - 2] == *ty),
            Got::Val(Value::Struct { ty, .. }) => ty == want,
            _ => false,
        }
    }

    /// Performs the moves/copies/borrows for a matched pattern's bindings.
    fn commit(&mut self, binds: Vec<PatBind>) -> R<Vec<(String, Value, bool)>> {
        let mut out = Vec::new();
        for b in binds {
            let v = match b.by_ref {
                Some(kind) => self.borrow_place(b.src, kind == RefKind::Mutable)?,
                None => self.read_place(&b.src)?,
            };
            out.push((b.name, v, b.mutable));
        }
        Ok(out)
    }
}

/// Whether `source` is a whole program (a real `fn main` definition, not
/// just a mention of one in a comment) rather than a bare statement block.
pub fn defines_main(source: &str) -> bool {
    match syn::parse_file(source) {
        Ok(file) => file.items.iter().any(|i| matches!(i, Item::Fn(f) if f.sig.ident == "main")),
        // Unparseable as a file: a whole program only if it clearly tries
        // to be one (so its parse error gets reported as-is).
        Err(_) => source.lines().any(|l| l.trim_start().starts_with("fn main(")),
    }
}

fn combine(via: Option<RefKind>, kind: RefKind) -> RefKind {
    match (via, kind) {
        (Some(RefKind::Shared), _) | (_, RefKind::Shared) => RefKind::Shared,
        _ => RefKind::Mutable,
    }
}

/// Appends a projection, folding indexing/slicing of an existing sub-slice
/// into offsets on the underlying data.
fn push_proj(place: &mut Place, proj: Proj) {
    if let Some(Proj::Range(a, _)) = place.path.last().cloned() {
        match proj {
            Proj::Index(i) => {
                place.path.pop();
                place.path.push(Proj::Index(a + i));
                return;
            }
            Proj::Range(c, d) => {
                place.path.pop();
                place.path.push(Proj::Range(a + c, a + d));
                return;
            }
            _ => {}
        }
    }
    place.path.push(proj);
}

fn describe_path(name: &str, path: &[Proj]) -> String {
    Place { frame: 0, slot: 0, name: name.to_string(), path: path.to_vec(), reborrow: false }.describe()
}

/// Every borrow-holding reference inside `v` (top-level, in fields, in a
/// closure's captures, in an iterator's state).
fn refs_in(v: &Value, out: &mut Vec<(Place, RefKind)>) {
    match v {
        Value::Ref(p, k) if !p.reborrow => out.push((p.clone(), *k)),
        Value::Array(items) | Value::Tuple(items) => items.iter().for_each(|x| refs_in(x, out)),
        Value::Struct { fields, .. } | Value::Enum { fields, .. } | Value::Closure { captures: fields, .. } => {
            fields.iter().for_each(|(_, x)| refs_in(x, out))
        }
        Value::Iter(n) => n.held_values().into_iter().for_each(|x| refs_in(x, out)),
        _ => {}
    }
}

/// A reference inside `v` that points into frame `min_frame` or above.
fn ref_into_frames(v: &Value, min_frame: usize) -> Option<&Place> {
    match v {
        Value::Ref(p, _) if p.frame >= min_frame => Some(p),
        Value::Array(items) | Value::Tuple(items) => items.iter().find_map(|x| ref_into_frames(x, min_frame)),
        Value::Struct { fields, .. } | Value::Enum { fields, .. } => fields.iter().find_map(|(_, x)| ref_into_frames(x, min_frame)),
        _ => None,
    }
}

fn result_ok() -> Value {
    Value::Enum { ty: "Result".into(), variant: "Ok".into(), fields: vec![("0".into(), Value::Unit)], copy: true }
}

fn option_none() -> Value {
    Value::Enum { ty: "Option".into(), variant: "None".into(), fields: vec![], copy: true }
}

fn option_some(v: Value) -> Value {
    let copy = v.is_copy();
    Value::Enum { ty: "Option".into(), variant: "Some".into(), fields: vec![("0".into(), v)], copy }
}

fn cast(v: Value, ty: &str) -> R<Value> {
    let int_ty = matches!(ty, "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128" | "usize");
    Ok(match (v, ty) {
        (Value::Int(i), t) if int_ty => Value::Int(match t {
            "u8" => i as u8 as i64,
            "i8" => i as i8 as i64,
            "u16" => i as u16 as i64,
            "i16" => i as i16 as i64,
            "u32" => i as u32 as i64,
            "i32" => i as i32 as i64,
            _ => i,
        }),
        (Value::Int(i), "f64" | "f32") => Value::Float(i as f64),
        (Value::Float(x), _) if int_ty => Value::Int(x as i64),
        (Value::Float(x), "f64" | "f32") => Value::Float(x),
        (Value::Char(c), _) if int_ty => Value::Int(c as i64),
        (Value::Bool(b), _) if int_ty => Value::Int(b as i64),
        (Value::Int(i), "char") if (0..=255).contains(&i) => Value::Char(i as u8 as char),
        (v, t) => return compile_err(format!("non-primitive cast: `{}` as `{t}` (E0605)", v.type_name())),
    })
}

fn derives_of(attrs: &[syn::Attribute]) -> Derives {
    let mut d = Derives { copy: false, clone: false, debug: false };
    for a in attrs {
        if a.path().is_ident("derive") {
            let _ = a.parse_nested_meta(|m| {
                match m.path.segments.last().map(|s| s.ident.to_string()).as_deref() {
                    Some("Copy") => d.copy = true,
                    Some("Clone") => d.clone = true,
                    Some("Debug") => d.debug = true,
                    _ => {}
                }
                Ok(())
            });
        }
    }
    d
}

fn type_name(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(p) => p.path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default(),
        other => other.to_token_stream().to_string(),
    }
}

fn strip_type(pat: &Pat) -> (&Pat, Option<&syn::Type>) {
    match pat {
        Pat::Type(t) => (&t.pat, Some(&t.ty)),
        p => (p, None),
    }
}

fn member_name(m: &Member) -> String {
    match m {
        Member::Named(i) => i.to_string(),
        Member::Unnamed(i) => i.index.to_string(),
    }
}

fn path_ident(path: &syn::Path) -> Option<String> {
    if path.segments.len() == 1 { Some(path.segments[0].ident.to_string()) } else { None }
}

fn expr_kind(e: &Expr) -> &'static str {
    match e {
        Expr::Async(_) | Expr::Await(_) => "async",
        Expr::Unsafe(_) => "unsafe",
        Expr::Yield(_) => "yield",
        _ => "unknown",
    }
}

/// Every identifier mentioned in `node`, including `{name}` captures inside
/// string literals (format strings) — "could this code use `name`?".
fn idents_of<T: ToTokens>(node: &T) -> HashSet<String> {
    let mut out = HashSet::new();
    collect_idents(node.to_token_stream(), &mut out);
    out
}

fn collect_idents(ts: TokenStream, out: &mut HashSet<String>) {
    for tt in ts {
        match tt {
            TokenTree::Ident(i) => {
                out.insert(i.to_string());
            }
            TokenTree::Group(g) => collect_idents(g.stream(), out),
            TokenTree::Literal(l) => {
                let s = l.to_string();
                if s.starts_with('"') || s.starts_with('r') {
                    let mut rest = s.as_str();
                    while let Some(i) = rest.find('{') {
                        rest = &rest[i + 1..];
                        if let Some(stripped) = rest.strip_prefix('{') {
                            rest = stripped;
                            continue;
                        }
                        let end = rest.find(['}', ':']).unwrap_or(rest.len());
                        let name = rest[..end].trim();
                        if name.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_') {
                            out.insert(name.to_string());
                        }
                    }
                }
            }
            TokenTree::Punct(_) => {}
        }
    }
}

/// For the live REPL: is this buffer a complete statement ready to
/// `feed()`, or still being typed? Rather than inspecting `syn`'s parse
/// error text (which doesn't cleanly distinguish "incomplete" from
/// "actually wrong"), use a simple heuristic matching the subset we
/// support — every statement we handle is `;`-terminated — so a buffer is
/// ready once its brackets are balanced *and* it ends in `;`. Bracket
/// counting skips the contents of string/char literals so a `{`/`;` typed
/// inside a string doesn't trip it.
pub fn is_ready_to_feed(buffer: &str) -> bool {
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut in_char = false;
    let mut chars = buffer.chars();
    while let Some(c) = chars.next() {
        if in_string {
            match c {
                '\\' => {
                    chars.next();
                }
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        if in_char {
            match c {
                '\\' => {
                    chars.next();
                }
                '\'' => in_char = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '\'' => in_char = true,
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => depth -= 1,
            _ => {}
        }
    }
    // Mid string/char literal at the end means genuinely incomplete,
    // regardless of what the brackets/trailing character look like.
    if in_string || in_char || depth > 0 {
        return false;
    }
    // Most statements we support need `;`. Block-shaped ones (`if`, and
    // later `for`/`while`/a bare block) don't, in real Rust, as long as
    // they're used as a statement rather than an expression — so a
    // buffer ending in `}` (a real one, not one from inside a string,
    // which the `in_string`/depth checks above already ruled out) is
    // ready too.
    let trimmed = buffer.trim_end();
    trimmed.ends_with(';') || trimmed.ends_with('}')
}

#[cfg(test)]
mod readiness_tests {
    use super::is_ready_to_feed;

    #[test]
    fn simple_statement_is_ready() {
        assert!(is_ready_to_feed("let x = 5;"));
    }

    #[test]
    fn missing_semicolon_is_not_ready() {
        assert!(!is_ready_to_feed("let x = 5"));
    }

    #[test]
    fn unbalanced_brackets_not_ready_even_with_semicolon() {
        // the `;` here belongs to an inner statement inside an unfinished
        // block, not a terminator for the whole buffer.
        assert!(!is_ready_to_feed("if x > 0 { let y = 1;"));
    }

    #[test]
    fn multiline_accumulation_becomes_ready_once_closed() {
        assert!(is_ready_to_feed("let v = vec![\n1,\n2,\n3\n];"));
    }

    #[test]
    fn brace_inside_string_literal_is_ignored() {
        assert!(is_ready_to_feed(r#"println!("{}");"#));
    }

    #[test]
    fn semicolon_inside_string_literal_does_not_count_as_terminator() {
        assert!(!is_ready_to_feed(r#"let s = String::from(";"#));
    }

    #[test]
    fn block_statement_ready_without_semicolon() {
        assert!(is_ready_to_feed("if x > 0 { println!(\"yes\"); }"));
    }

    #[test]
    fn block_statement_not_ready_while_open() {
        assert!(!is_ready_to_feed("if x > 0 {"));
    }

    #[test]
    fn stray_brace_inside_unterminated_string_not_mistaken_for_ready() {
        assert!(!is_ready_to_feed(r#"let s = "foo}"#));
    }
}
