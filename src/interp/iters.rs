//! Closures and iterators.
//!
//! A closure is modeled the way rustc lowers it: an anonymous struct whose
//! fields are its captures. Each captured variable is captured by `&`,
//! `&mut`, or by value, decided from how the body uses it (the same
//! inference rustc does), and that in turn decides `Fn`/`FnMut`/`FnOnce`.
//!
//! Iterators are lazy pipelines of `IterNode`s stored inline like any
//! other value. Nothing runs until a consumer (`collect`, `sum`, a `for`
//! loop, `next()`) pulls items through — so the closure calls show up
//! interleaved, one item at a time, exactly in real execution order.

use super::*;
use crate::memory::map_order;
use crate::value::{Capture, ClosureKind, IterNode, MapPart};

pub(super) struct ClosureDef {
    params: Vec<Pat>,
    body: Expr,
    line_base: usize,
    self_ty: Option<String>,
    /// `|x| x * 2` — how the closure is named in frames/events.
    label: String,
}

/// How a closure body uses a captured variable, weakest first.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Use {
    Ref,
    Mut,
    Move,
}

/// Per captured name: its strongest use, and whether it's ever mutated
/// (tracked separately — `count += 1; count` both mutates and moves-reads
/// `count`, and the mutation is what makes the closure `FnMut`).
type Uses = Vec<(String, Use, bool)>;

fn record(out: &mut Uses, name: String, u: Use) {
    match out.iter_mut().find(|(n, ..)| *n == name) {
        Some(e) => {
            e.1 = e.1.max(u);
            e.2 |= u == Use::Mut;
        }
        None => out.push((name, u, u == Use::Mut)),
    }
}

/// Names a pattern binds (so they aren't mistaken for captures).
fn pat_idents(p: &Pat, out: &mut HashSet<String>) {
    match p {
        Pat::Ident(i) => {
            out.insert(i.ident.to_string());
            if let Some((_, sp)) = &i.subpat {
                pat_idents(sp, out);
            }
        }
        Pat::Tuple(t) => t.elems.iter().for_each(|e| pat_idents(e, out)),
        Pat::TupleStruct(t) => t.elems.iter().for_each(|e| pat_idents(e, out)),
        Pat::Struct(s) => s.fields.iter().for_each(|f| pat_idents(&f.pat, out)),
        Pat::Or(o) => o.cases.iter().for_each(|c| pat_idents(c, out)),
        Pat::Reference(r) => pat_idents(&r.pat, out),
        Pat::Type(t) => pat_idents(&t.pat, out),
        Pat::Paren(p) => pat_idents(&p.pat, out),
        Pat::Slice(s) => s.elems.iter().for_each(|e| pat_idents(e, out)),
        Pat::Guard(g) => pat_idents(&g.pat, out),
        _ => {}
    }
}

impl Interpreter {
    // ─────────────────────────── closures ──────────────────────────

    /// `|params| body`: works out what the body captures and how, then
    /// borrows/moves those variables into the new closure value.
    pub(super) fn make_closure(&mut self, c: &syn::ExprClosure) -> R<Value> {
        let params: Vec<Pat> = c.inputs.iter().cloned().collect();
        let mut locals = HashSet::new();
        params.iter().for_each(|p| pat_idents(p, &mut locals));
        let mut uses = Vec::new();
        self.uses_in(&c.body, Use::Move, &mut locals, &mut uses);
        let is_move = c.capture.is_some();

        let mut captures = Vec::new();
        let mut modes = Vec::new();
        let mut kind = ClosureKind::Fn;
        let mut notes = Vec::new();
        for (name, mut u, mut mutated) in uses {
            let e: Expr = syn::parse_str(&name).map_err(|e| Flow::Err(InterpError::Parse(e.to_string())))?;
            let Some(pr) = self.eval_place(&e)? else { continue }; // not a local
            let Got::Val(val) = self.got(&pr.place)? else { continue };
            // Calling a captured closure needs whatever that closure needs.
            if let Value::Closure { kind: inner, .. } = &val {
                u = u.max(match inner {
                    ClosureKind::Fn => Use::Ref,
                    ClosureKind::FnMut => Use::Mut,
                    ClosureKind::FnOnce => Use::Move,
                });
                mutated |= *inner == ClosureKind::FnMut;
            }
            let copy = val.is_copy();
            let moves_out = u == Use::Move && !copy;
            let mode = match (is_move, moves_out, mutated) {
                (true, ..) => Capture::ByValue,
                (false, true, _) => Capture::ByValue,
                (false, false, true) => Capture::ByMut,
                // Reads only — even a by-value use of a `Copy` value just
                // needs `&` (`|| x + 1` captures `&x`), unless it's `move`.
                (false, false, false) => Capture::ByRef,
            };
            if moves_out {
                kind = ClosureKind::FnOnce;
            } else if mutated && kind == ClosureKind::Fn {
                kind = ClosureKind::FnMut;
            }
            let v = match mode {
                Capture::ByRef => self.borrow_place(pr, false)?,
                Capture::ByMut => self.borrow_place(pr, true)?,
                Capture::ByValue => self.read_place(&pr)?,
            };
            notes.push(match mode {
                Capture::ByRef => format!("`{name}` by &"),
                Capture::ByMut => format!("`{name}` by &mut"),
                Capture::ByValue if copy => format!("`{name}` by copy"),
                Capture::ByValue => format!("`{name}` by move"),
            });
            captures.push((name, v));
            modes.push(mode);
        }
        let mut label = self.source_text(c).unwrap_or_else(|| c.to_token_stream().to_string());
        if label.chars().count() > 40 {
            label = format!("{}…", label.chars().take(39).collect::<String>());
        }
        let caps = if notes.is_empty() { "captures nothing".to_string() } else { format!("captures {}", notes.join(", ")) };
        self.pending.push(Event::Note(format!("closure `{label}` created — {kind:?}, {caps}")));
        self.closures.push(Rc::new(ClosureDef {
            params,
            body: (*c.body).clone(),
            line_base: self.line_offset,
            self_ty: self.current_self_ty(),
            label,
        }));
        Ok(Value::Closure { id: self.closures.len() - 1, captures, modes, kind })
    }

    /// How `e` (evaluated in context `u`) uses each outer variable.
    fn uses_in(&self, e: &Expr, u: Use, locals: &mut HashSet<String>, out: &mut Uses) {
        use Use::*;
        let down = |u: Use| if u == Mut { Mut } else { Ref };
        match e {
            Expr::Path(p) => {
                if let Some(n) = path_ident(&p.path)
                    && !locals.contains(&n)
                {
                    record(out, n, u);
                }
            }
            Expr::Reference(r) => self.uses_in(&r.expr, if r.mutability.is_some() { Mut } else { Ref }, locals, out),
            // Rust 2021 would capture just `p.x`; we capture all of `p`,
            // and treat a field read as a borrow (fields read are usually Copy).
            Expr::Field(f) => self.uses_in(&f.base, down(u), locals, out),
            Expr::Index(ix) => {
                self.uses_in(&ix.expr, down(u), locals, out);
                self.uses_in(&ix.index, Move, locals, out);
            }
            Expr::Unary(un) => match un.op {
                UnOp::Deref(_) => self.uses_in(&un.expr, down(u), locals, out),
                _ => self.uses_in(&un.expr, Move, locals, out),
            },
            Expr::Paren(p) => self.uses_in(&p.expr, u, locals, out),
            Expr::Group(g) => self.uses_in(&g.expr, u, locals, out),
            Expr::Assign(a) => {
                self.uses_in(&a.left, Mut, locals, out);
                self.uses_in(&a.right, Move, locals, out);
            }
            Expr::Binary(b) => {
                use BinOp::*;
                let (l, r) = match b.op {
                    AddAssign(_) | SubAssign(_) | MulAssign(_) | DivAssign(_) | RemAssign(_) | BitAndAssign(_) | BitOrAssign(_)
                    | BitXorAssign(_) | ShlAssign(_) | ShrAssign(_) => (Use::Mut, Use::Move),
                    Eq(_) | Ne(_) | Lt(_) | Le(_) | Gt(_) | Ge(_) => (Use::Ref, Use::Ref),
                    _ => (Use::Move, Use::Move),
                };
                self.uses_in(&b.left, l, locals, out);
                self.uses_in(&b.right, r, locals, out);
            }
            Expr::MethodCall(m) => {
                let ru = self.method_receiver_use(&m.method.to_string());
                self.uses_in(&m.receiver, ru, locals, out);
                m.args.iter().for_each(|a| self.uses_in(a, Move, locals, out));
            }
            Expr::Call(c) => {
                self.uses_in(&c.func, Ref, locals, out);
                c.args.iter().for_each(|a| self.uses_in(a, Move, locals, out));
            }
            Expr::Macro(m) => self.uses_in_macro(&m.mac, locals, out),
            Expr::Block(b) => self.uses_in_block(&b.block, locals, out),
            Expr::Unsafe(b) => self.uses_in_block(&b.block, locals, out),
            Expr::If(i) => {
                let mut inner = locals.clone();
                match &*i.cond {
                    Expr::Let(l) => {
                        self.uses_in(&l.expr, Ref, locals, out);
                        pat_idents(&l.pat, &mut inner);
                    }
                    c => self.uses_in(c, Move, locals, out),
                }
                self.uses_in_block(&i.then_branch, &mut inner, out);
                if let Some((_, e)) = &i.else_branch {
                    self.uses_in(e, Move, locals, out);
                }
            }
            Expr::While(w) => {
                let mut inner = locals.clone();
                match &*w.cond {
                    Expr::Let(l) => {
                        self.uses_in(&l.expr, Mut, locals, out);
                        pat_idents(&l.pat, &mut inner);
                    }
                    c => self.uses_in(c, Move, locals, out),
                }
                self.uses_in_block(&w.body, &mut inner, out);
            }
            Expr::Loop(l) => self.uses_in_block(&l.body, locals, out),
            Expr::ForLoop(f) => {
                self.uses_in(&f.expr, Move, locals, out);
                let mut inner = locals.clone();
                pat_idents(&f.pat, &mut inner);
                self.uses_in_block(&f.body, &mut inner, out);
            }
            Expr::Match(m) => {
                self.uses_in(&m.expr, Ref, locals, out);
                for arm in &m.arms {
                    let mut inner = locals.clone();
                    pat_idents(&arm.pat, &mut inner);
                    if let Pat::Guard(g) = &arm.pat {
                        self.uses_in(&g.guard, Ref, &mut inner, out);
                    }
                    self.uses_in(&arm.body, Move, &mut inner, out);
                }
            }
            Expr::Closure(c) => {
                let mut inner = locals.clone();
                c.inputs.iter().for_each(|p| pat_idents(p, &mut inner));
                let mut nested = Vec::new();
                self.uses_in(&c.body, Move, &mut inner, &mut nested);
                for (n, nu, nm) in nested {
                    record(out, n.clone(), if c.capture.is_some() { Move } else { nu });
                    if nm {
                        record(out, n, Mut);
                    }
                }
            }
            Expr::Return(r) => {
                if let Some(x) = &r.expr {
                    self.uses_in(x, Move, locals, out);
                }
            }
            Expr::Break(b) => {
                if let Some(x) = &b.expr {
                    self.uses_in(x, Move, locals, out);
                }
            }
            Expr::Tuple(t) => t.elems.iter().for_each(|x| self.uses_in(x, Move, locals, out)),
            Expr::Array(a) => a.elems.iter().for_each(|x| self.uses_in(x, Move, locals, out)),
            Expr::Repeat(r) => self.uses_in(&r.expr, Move, locals, out),
            Expr::Struct(s) => {
                s.fields.iter().for_each(|f| self.uses_in(&f.expr, Move, locals, out));
                if let Some(r) = &s.rest {
                    self.uses_in(r, Move, locals, out);
                }
            }
            Expr::Cast(c) => self.uses_in(&c.expr, Move, locals, out),
            Expr::Try(t) => self.uses_in(&t.expr, Move, locals, out),
            Expr::Let(l) => self.uses_in(&l.expr, Ref, locals, out),
            Expr::Range(r) => {
                for x in r.start.iter().chain(r.end.iter()) {
                    self.uses_in(x, Move, locals, out);
                }
            }
            Expr::Lit(_) => {}
            other => {
                for n in idents_of(other) {
                    if !locals.contains(&n) {
                        record(out, n, Ref);
                    }
                }
            }
        }
    }

    fn uses_in_block(&self, b: &Block, locals: &mut HashSet<String>, out: &mut Uses) {
        let mut inner = locals.clone();
        for st in &b.stmts {
            match st {
                Stmt::Local(l) => {
                    if let Some(init) = &l.init {
                        self.uses_in(&init.expr, Use::Move, &mut inner, out);
                        if let Some((_, d)) = &init.diverge {
                            self.uses_in(d, Use::Move, &mut inner, out);
                        }
                    }
                    pat_idents(&l.pat, &mut inner);
                }
                Stmt::Expr(e, _) => self.uses_in(e, Use::Move, &mut inner, out),
                Stmt::Macro(m) => self.uses_in_macro(&m.mac, &mut inner, out),
                Stmt::Item(_) => {}
            }
        }
    }

    fn uses_in_macro(&self, mac: &syn::Macro, locals: &mut HashSet<String>, out: &mut Uses) {
        let name = mac.path.segments.last().map(|s| s.ident.to_string()).unwrap_or_default();
        let args = mac.parse_body_with(syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated);
        match (name.as_str(), args) {
            ("println" | "print" | "eprintln" | "eprint" | "format" | "panic" | "assert" | "assert_eq" | "assert_ne" | "write" | "writeln", Ok(args)) => {
                for (i, a) in args.iter().enumerate() {
                    let u = if i == 0 && (name == "write" || name == "writeln") { Use::Mut } else { Use::Ref };
                    match a {
                        // The format string: `{name}` captures.
                        Expr::Lit(_) => {
                            for n in idents_of(a) {
                                if !locals.contains(&n) {
                                    record(out, n, Use::Ref);
                                }
                            }
                        }
                        Expr::Assign(named) => self.uses_in(&named.right, Use::Ref, locals, out),
                        a => self.uses_in(a, u, locals, out),
                    }
                }
            }
            ("vec", Ok(args)) => args.iter().for_each(|a| self.uses_in(a, Use::Move, locals, out)),
            _ => {
                for n in idents_of(&mac.tokens) {
                    if !locals.contains(&n) {
                        record(out, n, Use::Ref);
                    }
                }
            }
        }
    }

    fn method_receiver_use(&self, m: &str) -> Use {
        match m {
            "push" | "push_str" | "pop" | "insert" | "remove" | "clear" | "truncate" | "sort" | "sort_unstable" | "sort_by_key" | "reverse"
            | "retain" | "dedup" | "extend" | "next" | "iter_mut" | "append" | "drain" | "get_mut" => Use::Mut,
            "into_iter" | "unwrap" | "expect" | "unwrap_or" | "unwrap_or_else" | "unwrap_or_default" | "into" => Use::Move,
            _ => {
                let suffix = format!("::{m}");
                let user = self.fns.iter().find(|(k, _)| k.ends_with(&suffix)).and_then(|(_, d)| match d.sig.inputs.first() {
                    Some(FnArg::Receiver(r)) => Some(match &r.kind {
                        syn::ReceiverKind::Reference(_, _, Some(_)) => Use::Mut,
                        syn::ReceiverKind::Reference(..) => Use::Ref,
                        _ => Use::Move,
                    }),
                    _ => None,
                });
                user.unwrap_or(Use::Ref)
            }
        }
    }

    /// `f(args)` where `f` is a place holding a closure or function value
    /// (possibly behind references or a `Box`).
    pub(super) fn call_callable_place(&mut self, callee: PlaceRef, args: Vec<Value>, line: usize) -> R<Value> {
        let p = self.auto_deref(callee)?;
        match self.got(&p.place)? {
            Got::Val(Value::Closure { .. }) => self.call_closure(p, args, line),
            Got::Val(Value::FnItem(path)) => self.call_fn_item(&path, args, line),
            Got::Val(v) => compile_err(format!("expected a function, found `{}` (E0618)", v.type_name())),
            _ => compile_err("expected a function (E0618)"),
        }
    }

    fn call_fn_item(&mut self, path: &str, args: Vec<Value>, line: usize) -> R<Value> {
        self.call_path(path.split("::").map(String::from).collect(), args, line)
    }

    /// Runs a closure stored at `cp`. Captured names inside the body are
    /// aliases straight to the real storage: the outer variable for `&`/
    /// `&mut` captures, the closure's own field for by-value ones (so an
    /// `FnMut` counter's state persists between calls, inside the closure).
    fn call_closure(&mut self, cp: PlaceRef, args: Vec<Value>, line: usize) -> R<Value> {
        let Got::Val(Value::Closure { id, captures, modes, kind }) = self.got(&cp.place)? else { unreachable!() };
        let def = self.closures[id].clone();
        if def.params.len() != args.len() {
            return compile_err(format!("this closure takes {} argument(s) but {} were supplied (E0057)", def.params.len(), args.len()));
        }
        let who = if cp.place.name == "<temp>" { def.label.clone() } else { cp.place.describe() };
        let owned = match kind {
            ClosureKind::FnOnce => {
                self.pending.push(Event::Note(format!("`{who}` is FnOnce (it moves a captured value out) — calling it consumes it")));
                self.read_place(&cp)?;
                true
            }
            ClosureKind::FnMut => {
                match cp.via {
                    Some(RefKind::Shared) => {
                        return compile_err(format!(
                            "cannot call `{who}`: it's an FnMut closure (it mutates its captures), but it's only reachable through a `&` reference (E0596)"
                        ));
                    }
                    Some(RefKind::Mutable) => {}
                    None => {
                        let b = self.stack.binding(cp.place.frame, cp.place.slot).ok_or_else(|| MemoryError::Dangling(who.clone()))?;
                        if !b.mutable && b.name != "<temp>" {
                            return compile_err(format!(
                                "cannot borrow `{who}` as mutable — calling an FnMut closure mutates it, so declare it `let mut {}` (E0596)",
                                b.name
                            ));
                        }
                        // A call is a use (catches a borrow already invalidated in
                        // the REPL), and it mutates the closure's state.
                        self.access(cp.place.frame, cp.place.slot, &cp.place.path, Access::Read)?;
                        self.access(cp.place.frame, cp.place.slot, &cp.place.path, Access::Write)?;
                    }
                }
                false
            }
            ClosureKind::Fn => {
                if cp.via.is_none() {
                    self.access(cp.place.frame, cp.place.slot, &cp.place.path, Access::Read)?;
                }
                false
            }
        };
        let frame_name = format!("closure {who}");
        let inv = self.next_inv;
        self.next_inv += 1;
        self.stack.push_frame(frame_name.clone());
        self.frame_inv.push(inv);
        let frame_idx = self.stack.frames.len() - 1;
        self.pending.push(Event::FrameEnter { name: frame_name });
        let saved_offset = self.line_offset;
        self.line_offset = def.line_base;
        self.self_tys.push(def.self_ty.clone());

        let result = (|| -> R<Value> {
            for (i, (name, v)) in captures.iter().enumerate() {
                match modes[i] {
                    Capture::ByValue if owned => self.declare(name.clone(), v.clone(), true),
                    Capture::ByRef | Capture::ByMut => {
                        let Value::Ref(t, k) = v else { continue };
                        self.declare_alias(name, Value::Ref(Place { reborrow: true, ..t.clone() }, *k));
                    }
                    Capture::ByValue => {
                        let mut t = cp.place.clone();
                        t.path.push(Proj::Field(name.clone()));
                        t.reborrow = true;
                        let k = if kind == ClosureKind::FnMut && cp.via != Some(RefKind::Shared) { RefKind::Mutable } else { RefKind::Shared };
                        self.declare_alias(name, Value::Ref(t, k));
                    }
                }
            }
            for (pat, a) in def.params.iter().zip(args) {
                self.bind_value(pat, a)?;
            }
            self.record_step(line);
            match &def.body {
                Expr::Block(b) if b.label.is_none() => match self.exec_stmts(&b.block.stmts) {
                    Ok(v) | Err(Flow::Return(v)) => Ok(v),
                    Err(e) => Err(e),
                },
                e => {
                    self.cur_line = self.line_of(e);
                    match self.eval_expr(e) {
                        Ok(v) | Err(Flow::Return(v)) => Ok(v),
                        Err(e) => Err(e),
                    }
                }
            }
        })();
        let result = match result {
            Ok(v) => v,
            Err(Flow::Break(..) | Flow::Continue(_)) => {
                self.line_offset = saved_offset;
                return compile_err("`break`/`continue` inside a closure can't reach a loop outside it (E0267)");
            }
            Err(e) => {
                self.line_offset = saved_offset;
                return Err(e);
            }
        };
        if let Some(p) = ref_into_frames(&result, frame_idx) {
            self.line_offset = saved_offset;
            return compile_err(format!("the closure returns a reference to `{}`, which only lives inside the closure call (E0515)", p.name));
        }
        let end = match &def.body {
            Expr::Block(b) => self.close_line(&b.block),
            e => self.line_of(e),
        };
        self.pop_frame(end)?;
        self.self_tys.pop();
        self.line_offset = saved_offset;
        self.cur_line = line;
        // A consumed FnOnce closure: its by-value captures were dropped
        // with the call's frame; its borrows end now.
        if owned {
            for (i, (_, v)) in captures.iter().enumerate() {
                if modes[i] != Capture::ByValue {
                    self.drop_value(v)?;
                }
            }
        }
        Ok(result)
    }

    fn declare_alias(&mut self, name: &str, v: Value) {
        self.stack.current_frame_mut().declare(name, v, false);
        self.stack.current_frame_mut().bindings.last_mut().unwrap().alias = true;
    }

    /// Calls a closure/function value that lives inside some other value
    /// (an iterator adaptor, an `Option::map` argument). `need` is the
    /// least the caller requires: `map` calls repeatedly, so an `FnOnce`
    /// closure is a compile error there.
    pub(super) fn call_value(&mut self, f: &mut Value, args: Vec<Value>, line: usize, need: ClosureKind) -> R<Value> {
        match f {
            Value::FnItem(p) => {
                let p = p.clone();
                self.call_fn_item(&p, args, line)
            }
            Value::Closure { kind, .. } => {
                if *kind == ClosureKind::FnOnce && need != ClosureKind::FnOnce {
                    return compile_err(
                        "expected a closure that can be called more than once (`FnMut`), but this one only implements `FnOnce` — it moves a captured value out (E0525)",
                    );
                }
                let tmp = self.temp_place(f.clone());
                let r = self.call_closure(tmp.clone(), args, line);
                if let Some(b) = self.stack.binding_mut(tmp.place.frame, tmp.place.slot) {
                    if b.state == OwnershipState::Owned {
                        *f = b.value.clone(); // FnMut state changes stick
                    } else {
                        *f = Value::Unit; // consumed
                    }
                    b.state = OwnershipState::Moved;
                }
                r
            }
            Value::Ref(p, k) => {
                let pr = PlaceRef { place: p.clone(), via: Some(*k) };
                self.call_callable_place(pr, args, line)
            }
            v => compile_err(format!("expected a function or closure, found `{}` (E0618)", v.type_name())),
        }
    }

    /// Calls `f(&x)` (for `filter`, `find`, `retain`, … which take the item
    /// by reference) and returns the result.
    fn call_with_ref(&mut self, f: &mut Value, x: &Value, line: usize) -> R<Value> {
        let tmp = self.temp_place(x.clone());
        let r = Value::Ref(Place { reborrow: true, ..tmp.place.clone() }, RefKind::Shared);
        let out = self.call_value(f, vec![r], line, ClosureKind::FnMut);
        if let Some(b) = self.stack.binding_mut(tmp.place.frame, tmp.place.slot) {
            b.state = OwnershipState::Moved; // `x` still belongs to the caller
        }
        out
    }

    fn truthy(&mut self, v: Value) -> R<bool> {
        match self.take_val(v)? {
            Value::Bool(b) => Ok(b),
            v => compile_err(format!("expected `bool`, found `{}` (E0308)", v.type_name())),
        }
    }

    // ─────────────────────────── iterators ─────────────────────────

    pub(super) fn eval_range(&mut self, r: &syn::ExprRange) -> R<Value> {
        let start = match &r.start {
            Some(e) => self.eval_int(e)?,
            None => return compile_err("`..end` (RangeTo) is not an iterator — give it a start (E0277)"),
        };
        let back = match &r.end {
            Some(e) => {
                let end = self.eval_int(e)?;
                if matches!(r.limits, RangeLimits::Closed(_)) { end + 1 } else { end }
            }
            None => i64::MAX,
        };
        Ok(Value::Iter(Box::new(IterNode::Range { front: start, back })))
    }

    /// Anything a `for`/`zip`/`chain` accepts (`IntoIterator`) → a node.
    pub(super) fn node_from(&mut self, v: Value) -> R<IterNode> {
        Ok(match v {
            Value::Iter(n) => *n,
            Value::Vec(id) => {
                let items = match self.heap.get(id)? {
                    HeapObject::Vec { items, .. } => items.clone(),
                    _ => vec![],
                };
                let n = items.len();
                IterNode::Owned { items, front: 0, back: n, buffer: Some(id), borrow: None }
            }
            Value::Array(items) => {
                let n = items.len();
                IterNode::Owned { items, front: 0, back: n, buffer: None, borrow: None }
            }
            Value::Map(id) => {
                let entries = self.map_entries(id)?;
                let order = map_order(&self.stack, &self.heap, &entries);
                self.pending.push(Event::Note("into_iter() takes ownership of the HashMap's table — entries are moved out as (key, value) pairs".into()));
                let items = order.into_iter().map(|i| Value::Tuple(vec![entries[i].0.clone(), entries[i].1.clone()])).collect();
                IterNode::MapInto { items, front: 0, buffer: id }
            }
            Value::Ref(p, kind) if matches!(self.got(&p)?, Got::Val(Value::Map(_))) => {
                let Got::Val(Value::Map(id)) = self.got(&p)? else { unreachable!() };
                let entries = self.map_entries(id)?;
                let order = map_order(&self.stack, &self.heap, &entries);
                IterNode::MapIter { base: Place { reborrow: true, ..p.clone() }, borrow: Box::new(Value::Ref(p, kind)), order, front: 0, kind, part: MapPart::Both }
            }
            Value::Ref(p, kind) => {
                let len = self.len_of(&p)?;
                IterNode::Slice { base: Place { reborrow: true, ..p.clone() }, borrow: Box::new(Value::Ref(p, kind)), front: 0, back: len, kind }
            }
            v => return compile_err(format!("`{}` is not an iterator (E0277)", v.type_name())),
        })
    }

    pub(super) fn iter_src_from_value(&mut self, v: Value) -> R<(IterSrc, bool)> {
        Ok((IterSrc::Lazy(Box::new(self.node_from(v)?)), false))
    }

    /// Pulls the next item through a pipeline — the heart of laziness.
    pub(super) fn iter_next(&mut self, node: &mut IterNode, line: usize) -> R<Option<Value>> {
        self.check_step_budget()?;
        Ok(match node {
            IterNode::Range { front, back } => {
                if front < back {
                    *front += 1;
                    Some(Value::Int(*front - 1))
                } else {
                    None
                }
            }
            IterNode::Slice { base, front, back, kind, .. } => {
                if front < back {
                    let mut p = base.clone();
                    push_proj(&mut p, Proj::Index(*front));
                    p.reborrow = true;
                    *front += 1;
                    Some(Value::Ref(p, *kind))
                } else {
                    None
                }
            }
            IterNode::Owned { items, front, back, .. } => {
                if front < back {
                    *front += 1;
                    Some(items[*front - 1].clone())
                } else {
                    None
                }
            }
            IterNode::Chars { chars, front, back, .. } => {
                if front < back {
                    *front += 1;
                    Some(Value::Char(chars[*front - 1]))
                } else {
                    None
                }
            }
            IterNode::Strs { base, text, ranges, front, back, .. } => {
                if front < back {
                    *front += 1;
                    Some(str_item(base, text, ranges[*front - 1]))
                } else {
                    None
                }
            }
            IterNode::Map(inner, f) => match self.iter_next(inner, line)? {
                Some(x) => Some(self.call_value(f, vec![x], line, ClosureKind::FnMut)?),
                None => None,
            },
            IterNode::Filter(inner, f) => loop {
                let Some(x) = self.iter_next(inner, line)? else { break None };
                let keep = self.call_with_ref(f, &x, line)?;
                if self.truthy(keep)? {
                    break Some(x);
                }
                self.drop_value(&x)?;
            },
            IterNode::FilterMap(inner, f) => loop {
                let Some(x) = self.iter_next(inner, line)? else { break None };
                match self.call_value(f, vec![x], line, ClosureKind::FnMut)? {
                    Value::Enum { variant, fields, .. } if variant == "Some" => break fields.into_iter().next().map(|(_, v)| v),
                    Value::Enum { variant, .. } if variant == "None" => continue,
                    v => return compile_err(format!("`filter_map`'s closure must return an `Option`, not `{}` (E0308)", v.type_name())),
                }
            },
            IterNode::TakeWhile(inner, f, done) => {
                if *done {
                    return Ok(None);
                }
                let Some(x) = self.iter_next(inner, line)? else { return Ok(None) };
                let keep = self.call_with_ref(f, &x, line)?;
                if self.truthy(keep)? {
                    Some(x)
                } else {
                    *done = true;
                    self.drop_value(&x)?;
                    None
                }
            }
            IterNode::SkipWhile(inner, f, started) => loop {
                let Some(x) = self.iter_next(inner, line)? else { break None };
                if !*started {
                    let skip = self.call_with_ref(f, &x, line)?;
                    if self.truthy(skip)? {
                        self.drop_value(&x)?;
                        continue;
                    }
                    *started = true;
                }
                break Some(x);
            },
            IterNode::Enumerate(inner, n) => match self.iter_next(inner, line)? {
                Some(x) => {
                    *n += 1;
                    Some(Value::Tuple(vec![Value::Int(*n as i64 - 1), x]))
                }
                None => None,
            },
            IterNode::Take(inner, n) => {
                if *n == 0 {
                    None
                } else {
                    *n -= 1;
                    self.iter_next(inner, line)?
                }
            }
            IterNode::Skip(inner, n) => {
                while *n > 0 {
                    *n -= 1;
                    match self.iter_next(inner, line)? {
                        Some(x) => self.drop_value(&x)?,
                        None => return Ok(None),
                    }
                }
                self.iter_next(inner, line)?
            }
            IterNode::StepBy(inner, step, first) => {
                if *first {
                    *first = false;
                } else {
                    for _ in 1..*step {
                        match self.iter_next(inner, line)? {
                            Some(x) => self.drop_value(&x)?,
                            None => return Ok(None),
                        }
                    }
                }
                self.iter_next(inner, line)?
            }
            IterNode::Zip(a, b) => {
                let Some(x) = self.iter_next(a, line)? else { return Ok(None) };
                match self.iter_next(b, line)? {
                    Some(y) => Some(Value::Tuple(vec![x, y])),
                    None => {
                        self.drop_value(&x)?;
                        None
                    }
                }
            }
            IterNode::Chain(a, b) => match self.iter_next(a, line)? {
                Some(x) => Some(x),
                None => self.iter_next(b, line)?,
            },
            IterNode::Cloned(inner) => match self.iter_next(inner, line)? {
                Some(x) => {
                    let target = self.deref_val(x.clone())?;
                    let c = self.deep_clone(&target)?;
                    self.drop_value(&x)?;
                    Some(c)
                }
                None => None,
            },
            IterNode::Copied(inner) => match self.iter_next(inner, line)? {
                Some(x) => {
                    let target = self.deref_val(x.clone())?;
                    if !target.is_copy() {
                        return compile_err(format!("`.copied()` needs `Copy` items, but `{}` isn't — use `.cloned()` (E0277)", target.type_name()));
                    }
                    self.drop_value(&x)?;
                    Some(target)
                }
                None => None,
            },
            IterNode::Rev(inner) => self.iter_next_back(inner, line)?,
            IterNode::MapIter { base, order, front, kind, part, .. } => {
                let Some(&i) = order.get(*front) else { return Ok(None) };
                *front += 1;
                let r = |proj: Proj, k: RefKind| {
                    let mut p = base.clone();
                    p.path.push(proj);
                    Value::Ref(p, k)
                };
                Some(match part {
                    MapPart::Keys => r(Proj::MapKey(i), RefKind::Shared),
                    MapPart::Values => r(Proj::MapVal(i), *kind),
                    MapPart::Both => Value::Tuple(vec![r(Proj::MapKey(i), RefKind::Shared), r(Proj::MapVal(i), *kind)]),
                })
            }
            IterNode::MapInto { items, front, .. } => {
                let x = items.get(*front).cloned();
                if x.is_some() {
                    *front += 1;
                }
                x
            }
        })
    }

    fn iter_next_back(&mut self, node: &mut IterNode, line: usize) -> R<Option<Value>> {
        self.check_step_budget()?;
        Ok(match node {
            IterNode::Range { front, back } => {
                if *back == i64::MAX {
                    return compile_err("an unbounded range (`a..`) can't be reversed (E0277)");
                }
                if front < back {
                    *back -= 1;
                    Some(Value::Int(*back))
                } else {
                    None
                }
            }
            IterNode::Slice { base, front, back, kind, .. } => {
                if front < back {
                    *back -= 1;
                    let mut p = base.clone();
                    push_proj(&mut p, Proj::Index(*back));
                    p.reborrow = true;
                    Some(Value::Ref(p, *kind))
                } else {
                    None
                }
            }
            IterNode::Owned { items, front, back, .. } => {
                if front < back {
                    *back -= 1;
                    Some(items[*back].clone())
                } else {
                    None
                }
            }
            IterNode::Chars { chars, front, back, .. } => {
                if front < back {
                    *back -= 1;
                    Some(Value::Char(chars[*back]))
                } else {
                    None
                }
            }
            IterNode::Strs { base, text, ranges, front, back, .. } => {
                if front < back {
                    *back -= 1;
                    Some(str_item(base, text, ranges[*back]))
                } else {
                    None
                }
            }
            IterNode::Map(inner, f) => match self.iter_next_back(inner, line)? {
                Some(x) => Some(self.call_value(f, vec![x], line, ClosureKind::FnMut)?),
                None => None,
            },
            IterNode::Filter(inner, f) => loop {
                let Some(x) = self.iter_next_back(inner, line)? else { break None };
                let keep = self.call_with_ref(f, &x, line)?;
                if self.truthy(keep)? {
                    break Some(x);
                }
                self.drop_value(&x)?;
            },
            IterNode::Enumerate(inner, n) => {
                let remaining = size_hint(inner).0;
                self.iter_next_back(inner, line)?.map(|x| Value::Tuple(vec![Value::Int((*n + remaining - 1) as i64), x]))
            }
            IterNode::Cloned(inner) => match self.iter_next_back(inner, line)? {
                Some(x) => {
                    let target = self.deref_val(x.clone())?;
                    let c = self.deep_clone(&target)?;
                    self.drop_value(&x)?;
                    Some(c)
                }
                None => None,
            },
            IterNode::Copied(inner) => match self.iter_next_back(inner, line)? {
                Some(x) => {
                    let target = self.deref_val(x.clone())?;
                    self.drop_value(&x)?;
                    Some(target)
                }
                None => None,
            },
            IterNode::Chain(a, b) => match self.iter_next_back(b, line)? {
                Some(x) => Some(x),
                None => self.iter_next_back(a, line)?,
            },
            IterNode::Rev(inner) => self.iter_next(inner, line)?,
            IterNode::MapIter { .. } | IterNode::MapInto { .. } => {
                return compile_err("a HashMap's iterator can't be reversed — it has no order to reverse (not a DoubleEndedIterator) (E0277)");
            }
            _ => {
                // Not double-ended in a way we model directly: run it
                // forward to the end, then hand items out from the back.
                self.materialize(node, line)?;
                return self.iter_next_back(node, line);
            }
        })
    }

    fn materialize(&mut self, node: &mut IterNode, line: usize) -> R<()> {
        let buffers = node.buffers();
        let mut borrows = Vec::new();
        refs_in(&Value::Iter(Box::new(node.clone())), &mut borrows);
        let mut items = Vec::new();
        while let Some(x) = self.iter_next(node, line)? {
            items.push(x);
        }
        let n = items.len();
        let borrow = borrows.first().map(|(p, k)| Box::new(Value::Ref(p.clone(), *k)));
        *node = IterNode::Owned { items, front: 0, back: n, buffer: buffers.first().copied(), borrow };
        Ok(())
    }

    /// Methods on iterators, iterator sources, and the closure-taking
    /// methods of `Option`/`Vec`. `None` = not one of these.
    pub(super) fn iter_method(
        &mut self,
        method: &str,
        recv: &PlaceRef,
        got: &Got,
        arg_exprs: &syn::punctuated::Punctuated<Expr, syn::Token![,]>,
        turbofish: Option<String>,
        line: usize,
    ) -> R<Option<Value>> {
        let mut args = Vec::new();
        let eval_args = |this: &mut Self, args: &mut Vec<Value>| -> R<()> {
            for a in arg_exprs {
                args.push(this.eval_arg(a)?);
            }
            Ok(())
        };
        match (got, method) {
            (Got::Val(Value::Iter(_)), _) => {
                eval_args(self, &mut args)?;
                self.iterator_method(method, recv, args, turbofish, line).map(Some)
            }
            // ── sources
            (Got::Val(Value::Vec(_) | Value::Array(_)) | Got::Slice(_), "iter" | "iter_mut") => {
                let mutable = method == "iter_mut";
                let len = self.len_of(&recv.place)?;
                let r = self.borrow_place(recv.clone(), mutable)?;
                let Value::Ref(base, kind) = r.clone() else { unreachable!() };
                let node = IterNode::Slice { base: Place { reborrow: true, ..base }, borrow: Box::new(r), front: 0, back: len, kind };
                Ok(Some(Value::Iter(Box::new(node))))
            }
            (Got::Val(Value::Vec(_) | Value::Array(_)) | Got::Slice(_), "into_iter") => {
                if recv.via.is_some() || matches!(got, Got::Slice(_)) {
                    // `(&v).into_iter()` — same as `.iter()`.
                    let len = self.len_of(&recv.place)?;
                    let r = self.borrow_place(recv.clone(), false)?;
                    let Value::Ref(base, kind) = r.clone() else { unreachable!() };
                    let node = IterNode::Slice { base: Place { reborrow: true, ..base }, borrow: Box::new(r), front: 0, back: len, kind };
                    return Ok(Some(Value::Iter(Box::new(node))));
                }
                let v = self.read_place(recv)?;
                if matches!(v, Value::Vec(_)) {
                    self.pending.push(Event::Note("into_iter() takes ownership of the Vec — its heap buffer now belongs to the iterator".into()));
                }
                Ok(Some(Value::Iter(Box::new(self.node_from(v)?))))
            }
            (Got::Val(Value::Str(_) | Value::StaticStr(_)) | Got::Str(_), "chars" | "bytes" | "split_whitespace" | "lines" | "split") => {
                eval_args(self, &mut args)?;
                let text = match got {
                    Got::Str(s) => s.clone(),
                    Got::Val(v) => self.text_of(v)?.unwrap_or_default(),
                    _ => unreachable!(),
                };
                // A string literal's bytes are 'static — nothing to borrow.
                let is_static = matches!(got, Got::Val(Value::StaticStr(_)));
                let borrow = if is_static { None } else { Some(Box::new(self.borrow_place(recv.clone(), false)?)) };
                let node = match method {
                    "chars" => {
                        let chars: Vec<char> = text.chars().collect();
                        let n = chars.len();
                        IterNode::Chars { chars, front: 0, back: n, borrow }
                    }
                    "bytes" => {
                        let items: Vec<Value> = text.bytes().map(|b| Value::Int(b as i64)).collect();
                        let n = items.len();
                        IterNode::Owned { items, front: 0, back: n, buffer: None, borrow }
                    }
                    _ => {
                        let ranges = match method {
                            "split_whitespace" => word_ranges(&text),
                            "lines" => line_ranges(&text),
                            _ => {
                                let pat = match args.first() {
                                    Some(Value::Char(c)) => c.to_string(),
                                    Some(a) => self.text_of(a)?.unwrap_or_default(),
                                    None => return compile_err("`split` needs a pattern (E0061)"),
                                };
                                split_ranges(&text, &pat)
                            }
                        };
                        let n = ranges.len();
                        let base = if is_static { None } else { Some(Place { reborrow: true, ..recv.place.clone() }) };
                        IterNode::Strs { base, text, ranges, front: 0, back: n, borrow }
                    }
                };
                for a in &args {
                    self.drop_value(a)?;
                }
                Ok(Some(Value::Iter(Box::new(node))))
            }
            (Got::Val(Value::Str(_) | Value::StaticStr(_)) | Got::Str(_), "as_bytes") => {
                // A `&[u8]` view of the same bytes: borrows the string,
                // allocates nothing.
                let mut p = recv.clone();
                match p.place.path.last().cloned() {
                    Some(Proj::Range(a, b)) => {
                        p.place.path.pop();
                        p.place.path.push(Proj::Bytes);
                        p.place.path.push(Proj::Range(a, b));
                    }
                    _ => p.place.path.push(Proj::Bytes),
                }
                Ok(Some(self.borrow_place(p, false)?))
            }
            // ── Option + closures
            (Got::Val(Value::Enum { ty, .. }), "map" | "and_then" | "unwrap_or_else" | "is_some_and" | "filter") if ty == "Option" => {
                eval_args(self, &mut args)?;
                let mut f = args.into_iter().next().unwrap_or(Value::Unit);
                let Value::Enum { variant, fields, .. } = self.read_place(recv)? else { unreachable!() };
                let inner = fields.into_iter().next().map(|(_, v)| v);
                let out = match (method, inner) {
                    ("map", Some(x)) => option_some(self.call_value(&mut f, vec![x], line, ClosureKind::FnOnce)?),
                    ("and_then", Some(x)) => self.call_value(&mut f, vec![x], line, ClosureKind::FnOnce)?,
                    ("unwrap_or_else", Some(x)) => x,
                    ("unwrap_or_else", None) => self.call_value(&mut f, vec![], line, ClosureKind::FnOnce)?,
                    ("is_some_and", Some(x)) => {
                        let r = self.call_value(&mut f, vec![x], line, ClosureKind::FnOnce)?;
                        Value::Bool(self.truthy(r)?)
                    }
                    ("is_some_and", None) => Value::Bool(false),
                    ("filter", Some(x)) => {
                        let keep = self.call_with_ref(&mut f, &x, line)?;
                        if self.truthy(keep)? {
                            option_some(x)
                        } else {
                            self.drop_value(&x)?;
                            option_none()
                        }
                    }
                    _ => option_none(),
                };
                self.drop_value(&f)?;
                let _ = variant;
                Ok(Some(out))
            }
            // ── Vec + closures
            (Got::Val(Value::Vec(id)), "retain" | "sort" | "sort_unstable" | "sort_by_key" | "reverse" | "dedup") => {
                let id = *id;
                eval_args(self, &mut args)?;
                self.check_write(recv, false)?;
                let HeapObject::Vec { items, .. } = self.heap.get(id)? else { unreachable!() };
                let mut items = items.clone();
                match method {
                    "reverse" => items.reverse(),
                    "retain" => {
                        let mut f = args.into_iter().next().unwrap_or(Value::Unit);
                        let mut kept = Vec::new();
                        for x in items {
                            let k = self.call_with_ref(&mut f, &x, line)?;
                            if self.truthy(k)? {
                                kept.push(x);
                            } else {
                                self.drop_value(&x)?;
                            }
                        }
                        self.drop_value(&f)?;
                        items = kept;
                    }
                    "dedup" => {
                        let mut kept: Vec<Value> = Vec::new();
                        for x in items {
                            let same = match kept.last() {
                                Some(l) => self.compare(&self.deref_val(l.clone())?, &self.deref_val(x.clone())?)? == Some(std::cmp::Ordering::Equal),
                                None => false,
                            };
                            if same {
                                self.drop_value(&x)?;
                            } else {
                                kept.push(x);
                            }
                        }
                        items = kept;
                    }
                    _ => {
                        let mut f = args.into_iter().next();
                        items = self.sort_items(items, &mut f, line)?;
                        if let Some(f) = &f {
                            self.drop_value(f)?;
                        }
                    }
                }
                if let HeapObject::Vec { items: slot, .. } = self.heap.get_mut(id)? {
                    *slot = items;
                }
                self.pending.push(Event::Mutate { name: recv.place.describe() });
                Ok(Some(Value::Unit))
            }
            (Got::Val(Value::Vec(_) | Value::Array(_)) | Got::Slice(_), "join" | "concat") => {
                eval_args(self, &mut args)?;
                let sep = match args.first() {
                    Some(a) => self.text_of(a)?.unwrap_or_default(),
                    None => String::new(),
                };
                let mut parts = Vec::new();
                for x in self.items_of(got)? {
                    match self.text_of(&x)? {
                        Some(t) => parts.push(t),
                        None => return compile_err(format!("`join` needs string items, not `{}` (E0599)", x.type_name())),
                    }
                }
                // `join` computes the exact final length up front.
                Ok(Some(self.alloc_str(parts.join(&sep))))
            }
            _ => Ok(None),
        }
    }

    /// `sort`/`sort_by_key`, matching std's call pattern: slices of up to
    /// 20 elements are insertion-sorted (`insertion_sort_shift_left`), and
    /// `sort_by_key` calls the key function *on every comparison* (twice
    /// each — it doesn't cache keys), which a counting closure can observe.
    fn sort_items(&mut self, mut items: Vec<Value>, key_fn: &mut Option<Value>, line: usize) -> R<Vec<Value>> {
        let key = |this: &mut Self, f: &mut Option<Value>, x: &Value| -> R<Value> {
            match f {
                Some(f) => {
                    let k = this.call_with_ref(f, x, line)?;
                    this.deref_val(k)
                }
                None => this.deref_val(x.clone()),
            }
        };
        let less = |this: &mut Self, a: &Value, b: &Value| -> R<bool> {
            match this.compare(a, b)? {
                Some(o) => Ok(o == std::cmp::Ordering::Less),
                None => compile_err("these values can't be ordered (no `Ord`) (E0277)"),
            }
        };
        if items.len() <= 20 {
            for i in 1..items.len() {
                let mut j = i;
                while j > 0 {
                    let kt = key(self, key_fn, &items[j])?;
                    let kp = key(self, key_fn, &items[j - 1])?;
                    if !less(self, &kt, &kp)? {
                        break;
                    }
                    items.swap(j, j - 1);
                    j -= 1;
                }
            }
            return Ok(items);
        }
        // Larger slices: same result (stable), key computed once each.
        let mut keyed = Vec::new();
        for x in items {
            let k = key(self, key_fn, &x)?;
            keyed.push((k, x));
        }
        let mut err = false;
        keyed.sort_by(|(a, _), (b, _)| match self.compare(a, b) {
            Ok(Some(o)) => o,
            _ => {
                err = true;
                std::cmp::Ordering::Equal
            }
        });
        if err {
            return compile_err("these values can't be ordered (no `Ord`) (E0277)");
        }
        Ok(keyed.into_iter().map(|(_, x)| x).collect())
    }

    fn take_iter(&mut self, recv: &PlaceRef) -> R<IterNode> {
        match self.read_place(recv)? {
            Value::Iter(n) => Ok(*n),
            _ => unreachable!(),
        }
    }

    fn iterator_method(&mut self, method: &str, recv: &PlaceRef, args: Vec<Value>, turbofish: Option<String>, line: usize) -> R<Value> {
        if method == "next" || method == "next_back" {
            // `&mut self`: needs `let mut it`.
            self.check_write(recv, false)?;
            let Got::Val(Value::Iter(mut node)) = self.got(&recv.place)? else { unreachable!() };
            let item = if method == "next" { self.iter_next(&mut node, line)? } else { self.iter_next_back(&mut node, line)? };
            *resolve_mut(&mut self.stack, &mut self.heap, &recv.place)? = Value::Iter(node);
            return Ok(item.map_or_else(option_none, option_some));
        }
        if method == "len" {
            let Got::Val(Value::Iter(node)) = self.got(&recv.place)? else { unreachable!() };
            return Ok(Value::Int(size_hint(&node).0 as i64));
        }
        let adaptor = |f: fn(Box<IterNode>, Box<Value>) -> IterNode, this: &mut Self, args: Vec<Value>| -> R<Value> {
            let node = this.take_iter(recv)?;
            let c = args.into_iter().next().unwrap_or(Value::Unit);
            Ok(Value::Iter(Box::new(f(Box::new(node), Box::new(c)))))
        };
        match method {
            "map" => return adaptor(IterNode::Map, self, args),
            "filter" => return adaptor(IterNode::Filter, self, args),
            "filter_map" => return adaptor(IterNode::FilterMap, self, args),
            "take_while" => return adaptor(|i, f| IterNode::TakeWhile(i, f, false), self, args),
            "skip_while" => return adaptor(|i, f| IterNode::SkipWhile(i, f, false), self, args),
            "enumerate" | "take" | "skip" | "step_by" | "cloned" | "copied" | "rev" | "zip" | "chain" | "into_iter" | "iter" => {
                let n = match method {
                    "take" | "skip" | "step_by" => self.arg_usize(&args, 0)?,
                    _ => 0,
                };
                if method == "step_by" && n == 0 {
                    return panic_err("assertion failed: step != 0");
                }
                let node = Box::new(self.take_iter(recv)?);
                let other = match method {
                    "zip" | "chain" => Some(Box::new(self.node_from(args.into_iter().next().unwrap_or(Value::Unit))?)),
                    _ => None,
                };
                return Ok(Value::Iter(Box::new(match method {
                    "enumerate" => IterNode::Enumerate(node, 0),
                    "take" => IterNode::Take(node, n),
                    "skip" => IterNode::Skip(node, n),
                    "step_by" => IterNode::StepBy(node, n, true),
                    "cloned" => IterNode::Cloned(node),
                    "copied" => IterNode::Copied(node),
                    "rev" => IterNode::Rev(node),
                    "zip" => IterNode::Zip(node, other.unwrap()),
                    "chain" => IterNode::Chain(node, other.unwrap()),
                    _ => *node,
                })));
            }
            _ => {}
        }
        // Consumers: drive the pipeline, then drop what's left of it
        // (ending its borrows, freeing an IntoIter's buffer).
        let mut node = self.take_iter(recv)?;
        let mut args = args;
        let out = match method {
            "collect" => {
                let hint = turbofish.or_else(|| self.collect_hint.clone()).unwrap_or_default();
                if hint.trim_start_matches(":: ").starts_with("HashMap") || hint.trim_start_matches(":: <").starts_with("HashMap") {
                    let m = self.map_new(0);
                    let Value::Map(id) = m else { unreachable!() };
                    self.map_extend(id, &mut node, line)?;
                    m
                } else if hint.contains("String") && !hint.contains("Vec") {
                    self.collect_string(&mut node, line)?
                } else if hint.contains("HashSet") || hint.contains("Result") || hint.contains("Option") {
                    return Err(self.unsupported(format!("collecting into `{hint}`")));
                } else {
                    self.collect_vec(&mut node, line)?
                }
            }
            "sum" | "product" => {
                let mut acc: Option<Value> = None;
                while let Some(x) = self.iter_next(&mut node, line)? {
                    let x = self.take_val(x)?;
                    acc = Some(match acc {
                        None => x,
                        Some(a) => {
                            let op: BinOp = if method == "sum" { syn::parse_str("+").unwrap() } else { syn::parse_str("*").unwrap() };
                            self.binop(&op, a, x)?
                        }
                    });
                }
                acc.unwrap_or(Value::Int(if method == "sum" { 0 } else { 1 }))
            }
            "count" => {
                let mut n = 0;
                while let Some(x) = self.iter_next(&mut node, line)? {
                    self.drop_value(&x)?;
                    n += 1;
                }
                Value::Int(n)
            }
            "last" | "nth" => {
                let target = if method == "nth" { Some(self.arg_usize(&args, 0)?) } else { None };
                let mut out = None;
                let mut i = 0;
                while let Some(x) = self.iter_next(&mut node, line)? {
                    if let Some(old) = out.take() {
                        self.drop_value(&old)?;
                    }
                    if target == Some(i) {
                        out = Some(x);
                        break;
                    }
                    if target.is_none() {
                        out = Some(x);
                    } else {
                        self.drop_value(&x)?;
                    }
                    i += 1;
                }
                out.map_or_else(option_none, option_some)
            }
            "min" | "max" | "min_by_key" | "max_by_key" => {
                let mut key_fn = if method.ends_with("_by_key") { args.pop() } else { None };
                let mut best: Option<(Value, Value)> = None; // (key, item)
                while let Some(x) = self.iter_next(&mut node, line)? {
                    let key = match &mut key_fn {
                        Some(f) => {
                            let k = self.call_with_ref(f, &x, line)?;
                            self.deref_val(k)?
                        }
                        None => self.deref_val(x.clone())?,
                    };
                    let replace = match &best {
                        None => true,
                        Some((bk, _)) => {
                            let o = self.compare(&key, bk)?.ok_or_else(|| Flow::Err(InterpError::CompileError("these values can't be ordered (E0277)".into())))?;
                            // `max` keeps the last of equal maxima, `min` the first.
                            if method.starts_with("max") { o != std::cmp::Ordering::Less } else { o == std::cmp::Ordering::Less }
                        }
                    };
                    if replace {
                        if let Some((_, old)) = best.take() {
                            self.drop_value(&old)?;
                        }
                        best = Some((key, x));
                    } else {
                        self.drop_value(&x)?;
                    }
                }
                if let Some(f) = &key_fn {
                    self.drop_value(f)?;
                }
                best.map(|(_, x)| x).map_or_else(option_none, option_some)
            }
            "fold" => {
                let mut f = args.pop().unwrap_or(Value::Unit);
                let mut acc = args.pop().unwrap_or(Value::Unit);
                while let Some(x) = self.iter_next(&mut node, line)? {
                    acc = self.call_value(&mut f, vec![acc, x], line, ClosureKind::FnMut)?;
                }
                self.drop_value(&f)?;
                acc
            }
            "for_each" => {
                let mut f = args.pop().unwrap_or(Value::Unit);
                while let Some(x) = self.iter_next(&mut node, line)? {
                    let r = self.call_value(&mut f, vec![x], line, ClosureKind::FnMut)?;
                    self.drop_value(&r)?;
                }
                self.drop_value(&f)?;
                Value::Unit
            }
            "any" | "all" | "position" | "find" => {
                let mut f = args.pop().unwrap_or(Value::Unit);
                let mut i = 0;
                let mut out = match method {
                    "any" => Value::Bool(false),
                    "all" => Value::Bool(true),
                    _ => option_none(),
                };
                while let Some(x) = self.iter_next(&mut node, line)? {
                    if method == "find" {
                        let r = self.call_with_ref(&mut f, &x, line)?;
                        if self.truthy(r)? {
                            out = option_some(x);
                            break;
                        }
                        self.drop_value(&x)?;
                        continue;
                    }
                    let r = self.call_value(&mut f, vec![x], line, ClosureKind::FnMut)?;
                    let b = self.truthy(r)?;
                    match method {
                        "any" if b => {
                            out = Value::Bool(true);
                            break;
                        }
                        "all" if !b => {
                            out = Value::Bool(false);
                            break;
                        }
                        "position" if b => {
                            out = option_some(Value::Int(i));
                            break;
                        }
                        _ => {}
                    }
                    i += 1;
                }
                self.drop_value(&f)?;
                out
            }
            other => {
                self.drop_value(&Value::Iter(Box::new(node)))?;
                return Err(self.unsupported(format!("the iterator method `.{other}()`")));
            }
        };
        self.drop_value(&Value::Iter(Box::new(node)))?;
        Ok(out)
    }

    /// `collect::<Vec<_>>()`, sized exactly like std's `SpecFromIter`:
    /// - an exact-length source (`TrustedLen`: ranges, `.iter()`, `map`…)
    ///   allocates exactly once, for all items;
    /// - otherwise (`filter`, `chars`…) the first item allocates
    ///   `max(4, lower_bound + 1)`, then it grows like `push`;
    /// - `vec.into_iter()…collect()` with same-type items reuses the
    ///   original buffer in place (no allocation at all).
    fn collect_vec(&mut self, node: &mut IterNode, line: usize) -> R<Value> {
        if let Some((id, src_ty)) = in_place_source(node) {
            let mut items = Vec::new();
            while let Some(x) = self.iter_next(node, line)? {
                items.push(x);
            }
            if items.first().is_none_or(|x| Some(x.type_name()) == src_ty) {
                detach_buffer(node);
                if let HeapObject::Vec { items: slot, .. } = self.heap.get_mut(id)? {
                    *slot = items;
                }
                self.pending.push(Event::Note(format!("collect() reuses the original Vec's buffer #{id} in place — no new allocation")));
                return Ok(Value::Vec(id));
            }
            let n = items.len();
            return Ok(self.alloc_vec(items, n));
        }
        if trusted_len(node) {
            let cap = size_hint(node).1.unwrap_or(0);
            let v = self.alloc_vec(vec![], cap);
            let Value::Vec(id) = v else { unreachable!() };
            while let Some(x) = self.iter_next(node, line)? {
                if let HeapObject::Vec { items, .. } = self.heap.get_mut(id)? {
                    items.push(x);
                }
            }
            return Ok(v);
        }
        let Some(first) = self.iter_next(node, line)? else { return Ok(self.alloc_vec(vec![], 0)) };
        let cap = 4.max(size_hint(node).0.saturating_add(1));
        let v = self.alloc_vec(vec![first], cap);
        let Value::Vec(id) = v else { unreachable!() };
        while let Some(x) = self.iter_next(node, line)? {
            let (len, cap) = match self.heap.get(id)? {
                HeapObject::Vec { items, cap } => (items.len(), *cap),
                _ => (0, 0),
            };
            if len == cap {
                let extra = size_hint(node).0.saturating_add(1);
                self.reserve(id, len + extra)?;
            }
            if let HeapObject::Vec { items, .. } = self.heap.get_mut(id)? {
                items.push(x);
            }
        }
        Ok(v)
    }

    /// `collect::<String>()` — std's `FromIterator` impls: chars reserve
    /// the iterator's lower bound then push; `&str`s are pushed one by
    /// one; `String`s reuse the *first* string's buffer and append the rest.
    fn collect_string(&mut self, node: &mut IterNode, line: usize) -> R<Value> {
        let lower = size_hint(node).0;
        let Some(first) = self.iter_next(node, line)? else { return Ok(self.alloc_str(String::new())) };
        let first_target = self.deref_val(first.clone())?;
        let id = match &first_target {
            Value::Str(id) if !matches!(first, Value::Ref(..)) => {
                self.pending.push(Event::Note(format!("collect::<String>() reuses the first String's buffer #{id} and appends the rest")));
                *id
            }
            _ => {
                let Value::Str(id) = self.alloc_str(String::new()) else { unreachable!() };
                if matches!(first_target, Value::Char(_)) && lower > 0 {
                    self.reserve(id, lower)?;
                }
                self.push_text(id, &first)?;
                id
            }
        };
        while let Some(x) = self.iter_next(node, line)? {
            self.push_text(id, &x)?;
            if !matches!(x, Value::Ref(..)) {
                self.drop_value(&x)?;
            }
        }
        Ok(Value::Str(id))
    }

    fn push_text(&mut self, id: HeapId, x: &Value) -> R<()> {
        let t = match self.deref_val(x.clone())? {
            Value::Char(c) => c.to_string(),
            v => match self.text_of(&v)? {
                Some(t) => t,
                None => return compile_err(format!("a `String` can't be collected from `{}` items (E0277)", v.type_name())),
            },
        };
        self.str_append(id, &t)
    }
}

fn str_item(base: &Option<Place>, text: &str, (a, b): (usize, usize)) -> Value {
    match base {
        Some(p) => {
            let mut p = p.clone();
            push_proj(&mut p, Proj::Range(a, b));
            p.reborrow = true;
            Value::Ref(p, RefKind::Shared)
        }
        None => Value::StaticStr(text[a..b].to_string()),
    }
}

fn word_ranges(t: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in t.char_indices() {
        match (c.is_whitespace(), start) {
            (false, None) => start = Some(i),
            (true, Some(s)) => {
                out.push((s, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push((s, t.len()));
    }
    out
}

fn line_ranges(t: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut pos = 0;
    for l in t.split_inclusive('\n') {
        let mut end = pos + l.len();
        if l.ends_with('\n') {
            end -= 1;
            if t[pos..end].ends_with('\r') {
                end -= 1;
            }
        }
        out.push((pos, end));
        pos += l.len();
    }
    out
}

fn split_ranges(t: &str, pat: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut pos = 0;
    for part in t.split(pat) {
        out.push((pos, pos + part.len()));
        pos += part.len() + pat.len();
    }
    out
}

/// `Iterator::size_hint`, per std's implementations.
pub(super) fn size_hint(n: &IterNode) -> (usize, Option<usize>) {
    let sat = |x: i64| x.max(0) as usize;
    match n {
        IterNode::Range { front, back } => {
            if *back == i64::MAX { (usize::MAX, None) } else { (sat(back - front), Some(sat(back - front))) }
        }
        IterNode::Slice { front, back, .. } | IterNode::Owned { front, back, .. } => (back - front, Some(back - front)),
        IterNode::Chars { chars, front, back, .. } => {
            let bytes: usize = chars[*front..*back].iter().map(|c| c.len_utf8()).sum();
            (bytes.div_ceil(4), Some(bytes))
        }
        IterNode::Strs { front, back, .. } => (0, Some(back - front)),
        IterNode::MapIter { order, front, .. } => (order.len() - front, Some(order.len() - front)),
        IterNode::MapInto { items, front, .. } => (items.len() - front, Some(items.len() - front)),
        IterNode::Map(i, _) | IterNode::Enumerate(i, _) | IterNode::Cloned(i) | IterNode::Copied(i) | IterNode::Rev(i) => size_hint(i),
        IterNode::Filter(i, _) | IterNode::FilterMap(i, _) => (0, size_hint(i).1),
        IterNode::TakeWhile(i, _, done) => (0, if *done { Some(0) } else { size_hint(i).1 }),
        IterNode::SkipWhile(i, _, started) => {
            if *started { size_hint(i) } else { (0, size_hint(i).1) }
        }
        IterNode::Take(i, n) => {
            let (lo, up) = size_hint(i);
            (lo.min(*n), Some(up.map_or(*n, |u| u.min(*n))))
        }
        IterNode::Skip(i, n) => {
            let (lo, up) = size_hint(i);
            (lo.saturating_sub(*n), up.map(|u| u.saturating_sub(*n)))
        }
        IterNode::StepBy(i, step, first) => {
            let f = |n: usize| if *first { if n == 0 { 0 } else { 1 + (n - 1) / step } } else { n / step };
            let (lo, up) = size_hint(i);
            (f(lo), up.map(f))
        }
        IterNode::Zip(a, b) => {
            let ((la, ua), (lb, ub)) = (size_hint(a), size_hint(b));
            let up = match (ua, ub) {
                (Some(x), Some(y)) => Some(x.min(y)),
                (x, y) => x.or(y),
            };
            (la.min(lb), up)
        }
        IterNode::Chain(a, b) => {
            let ((la, ua), (lb, ub)) = (size_hint(a), size_hint(b));
            (la.saturating_add(lb), ua.zip(ub).and_then(|(x, y)| x.checked_add(y)))
        }
    }
}

/// Whether std treats this pipeline as `TrustedLen` (exact size known up
/// front), which lets `collect` allocate exactly once.
fn trusted_len(n: &IterNode) -> bool {
    match n {
        IterNode::Range { back, .. } => *back != i64::MAX,
        IterNode::Slice { .. } | IterNode::Owned { .. } => true,
        IterNode::Map(i, _) | IterNode::Enumerate(i, _) | IterNode::Cloned(i) | IterNode::Copied(i) | IterNode::Rev(i) | IterNode::Take(i, _) => trusted_len(i),
        IterNode::Zip(a, b) | IterNode::Chain(a, b) => trusted_len(a) && trusted_len(b),
        _ => false,
    }
}

/// `vec.into_iter()` followed only by adaptors std can collect in place
/// (`InPlaceIterable`): the source buffer and its element type.
fn in_place_source(n: &IterNode) -> Option<(HeapId, Option<String>)> {
    match n {
        IterNode::Owned { buffer: Some(id), items, .. } => Some((*id, items.first().map(Value::type_name))),
        IterNode::Map(i, _) | IterNode::Filter(i, _) | IterNode::FilterMap(i, _) | IterNode::Take(i, _) | IterNode::Skip(i, _)
        | IterNode::TakeWhile(i, ..) | IterNode::SkipWhile(i, ..) => in_place_source(i),
        _ => None,
    }
}

/// The buffer is being reused by `collect`, so the exhausted iterator must
/// not free it.
fn detach_buffer(n: &mut IterNode) {
    match n {
        IterNode::Owned { buffer, .. } => *buffer = None,
        IterNode::Map(i, _) | IterNode::Filter(i, _) | IterNode::FilterMap(i, _) | IterNode::Take(i, _) | IterNode::Skip(i, _)
        | IterNode::TakeWhile(i, ..) | IterNode::SkipWhile(i, ..) => detach_buffer(i),
        _ => {}
    }
}
