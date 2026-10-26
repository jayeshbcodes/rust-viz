# Fidelity ledger

`rust-viz`'s whole point is teaching an accurate mental model of Rust's
memory/ownership rules — so a `Value` variant that's *wrong* about where
data actually lives (stack vs. heap), whether it's `Copy`, or when it
drops, is worse than a missing feature. A missing feature just errors;
a wrong model teaches something false.

Golden tests (`tests/golden.rs`, comparing our `println!` output against
real `rustc`) catch wrong **values**. They cannot catch wrong **memory
model** — a compiled Rust program never prints "I allocated this on the
heap," so the `Array`-heap-allocation bug (below) sailed through all 4
golden tests passing cleanly. This file is the second kind of check: a
running record of what's been verified true-to-Rust, and what's a known,
deliberate divergence.

**Going forward, two rules:**
1. **Keep this ledger updated** — every time a `Value` variant is added or
   a divergence is found/fixed, add or update its row here.
2. **Verify the real-Rust fact *before* writing the `Value` variant, not
   after.** Check `size_of`/the Reference/a trait-bound compile check
   first; don't ship a plausible-looking representation and wait for a
   golden-test mismatch or a sharp question to catch it.

Sources: the [Rust Reference's Type Layout
chapter](https://doc.rust-lang.org/reference/type-layout.html) for
language-level facts (arrays, primitives, tuples); `std::mem::size_of`
checks against real `rustc` for standard-library types (`Vec`, `String`),
whose exact field layout is a library implementation detail the language
Reference doesn't specify.

| `Value` variant | Real Rust fact | Our model | Status |
|---|---|---|---|
| `Int(i64)` | Primitive scalar, fixed size (e.g. `i32`=4 bytes), stored inline, `Copy` | Stored inline in `Value`, `is_copy() == true` | ✅ Correct |
| `Bool(bool)` | 1 byte, inline, `Copy` | Inline, `Copy` | ✅ Correct |
| `StaticStr(String)` | A `&'static str` literal lives in the binary's read-only data — **not the heap**, no `Drop` needed | Content stored directly in the `Value`, no `HeapId`, `Copy` | ✅ Correct |
| `Str(HeapId)` (owned `String`) | `String` wraps `Vec<u8>` — a 3-word (ptr/len/cap) stack value whose buffer is genuinely heap-allocated. Confirmed via `size_of::<Vec<i32>>() == 24` on this platform | Heap-allocated via `HeapObject::Str`, not `Copy` | ✅ Correct |
| `Vec(HeapId)` | Same 3-word stack value + heap buffer as `String` | Heap-allocated via `HeapObject::Vec`, not `Copy` | ✅ Correct |
| `Ref(Place, RefKind::Shared)` (`&T`) | Pointer-sized, `Copy` (confirmed `size_of::<&i32>() == 8` on this platform) | `Copy` via `is_copy()` | ✅ Correct |
| `Ref(Place, RefKind::Mutable)` (`&mut T`) | Pointer-sized, **not** `Copy` (duplicating it would alias two live `&mut`s) | `is_copy() == false` | ✅ Correct |
| `Array(Vec<Value>)` (`[T; N]`) | **Stored inline/contiguous, not behind a pointer** — size is exactly `size_of::<T>() * N`, confirmed both via the Reference and empirically (`size_of::<[i32;4]>()==16`, `[i32;8]==32`, `[i32;100]==400`, scaling linearly with N). `Copy` iff every element is `Copy`. | Fixed: stores elements directly in the `Value` (no `HeapId`); `is_copy()` checks all elements recursively | ✅ Correct (fixed 2026-10-25). Drop now recurses into elements, so `[String; N]` frees each `String` (fixed 2026-10-25). |
| Slices `&s[a..b]`, `&v[a..b]`, `s.as_str()` | A slice is a fat pointer: address + length into existing data, no allocation of its own; a slice of a `&'static str` is still `&'static str` | `Ref` whose `Place` ends in `Proj::Range(a, b)` — borrows the owner like any `&`, no `HeapAlloc`. The old unused `StrSlice` variant was removed in favor of this. Slicing a `StaticStr` yields a narrower `StaticStr` (borrows nothing). Out-of-range/non-char-boundary panics like real Rust | ✅ Correct (implemented 2026-10-25) |
| `Float(f64)`, `Char(char)` | Inline scalars, `Copy` | Inline, `Copy`; `Display`/`Debug` use std's own formatting, so `1.0` prints `1` and `{:?}` prints `1.0` exactly like rustc | ✅ Correct |
| `Tuple(Vec<Value>)` | Inline, `Copy` iff all elements are | Inline, `is_copy` checks elements | ✅ Correct |
| `Struct { .. }` | Fields stored inline (`size_of::<Rect{u32,u32}>() == 8`, confirmed). **Not `Copy` unless `#[derive(Copy)]`**, even if every field is `Copy` | Inline; `copy` flag set only from `#[derive(Copy)]` | ✅ Correct |
| `Enum { .. }` (incl. `Option`) | Inline tag + payload; user enums `Copy` only via derive; `Option<T>` is `Copy` iff `T` is | Same | ✅ Correct |
| `Box(HeapId)` | One heap allocation holding the value; moving the `Box` moves only the pointer | `HeapObject::Box`, `HeapAlloc` on `Box::new`, freed on drop | ✅ Correct |
| `Vec`/`String` capacity | `Vec::new()`/`String::new()` don't allocate (cap 0). Growth = `max(2·cap, needed, MIN_NON_ZERO_CAP)`, min 4 for `Vec<T>` (elem ≤ 1024 bytes), 8 for `String`. `vec![..]`/`String::from`/`.clone()` allocate exactly `len`. `clear()` keeps capacity. **Verified against rustc's `.capacity()`** (`samples/capacity.rs` golden test) | `HeapObject::{Vec,Str}::cap` with exactly that rule; `HeapRealloc` event on growth | ✅ Correct |
| `Closure { captures, modes, kind }` | A closure is an anonymous struct of its captures, stored inline. Each variable is captured by `&`, `&mut`, or by value — the least the body needs (a by-value use of a `Copy` value still only captures `&`, unless `move`). Mutating a capture ⇒ `FnMut`; moving a captured value out ⇒ `FnOnce`. A closure is `Copy` iff all captures are (verified: copying a closure that captures `&i32` compiles and both copies work) | Captures inferred syntactically from the body, then borrowed/moved at creation (real `Borrow`/`Move` events). Inside a call, captured names alias the real storage: the outer variable (by-ref) or the closure's own field (by-value, so `move` FnMut state persists between calls). Calling an `FnMut` needs `let mut`; calling an `FnOnce` moves it | ✅ Correct (2026-10-26). Gaps: Rust 2021 *disjoint field* capture (`p.x` only) isn't modeled — the whole variable is captured, and a field read counts as a borrow |
| `Iter(Box<IterNode>)` | Iterators are lazy, inline structs (`Map<Iter<'_, T>, F>`); `.iter()` holds a borrow of the collection, `into_iter()` owns the Vec's buffer; adaptors do nothing until a consumer pulls | Same: a pipeline of nodes, driven one item at a time, so closure calls appear in true execution order. Dropping an iterator ends its borrow / frees its buffer | ✅ Correct |
| `collect()` capacity | `TrustedLen` sources (`map` over `.iter()`, ranges) allocate exactly once; others start at `max(4, lower_bound+1)` then grow like `push`; `vec.into_iter()…collect()` with same-type items **reuses the source buffer in place**; `collect::<String>()` reserves `chars`' lower bound `(bytes+3)/4`. **All verified against rustc** (`samples/iter_capacity.rs`, plus a pointer-equality check for the in-place case) | Same rules. In-place reuse is decided by comparing the first output item's type name with the source element's — an approximation of std's size/align check | ✅ Correct (approximation noted) |
| `sort_by_key` call count | std calls the key fn on *every comparison* (no caching); slices ≤ 20 use insertion sort — the Book's 3-rectangle example reports 6 calls | Same algorithm for ≤ 20 elements; larger slices compute each key once (call count diverges, order doesn't) | ✅ Correct for ≤ 20 |
| `FnItem(String)` | A function used as a value is a zero-sized `Copy` value | Same | ✅ Correct |
| `s.as_bytes()` | A `&[u8]` view of the string's own bytes: borrows the `String`, allocates nothing | `Ref` with a `Proj::Bytes` projection; indexing yields `u8`s; `s.bytes()` iterates them | ✅ Correct |

## Borrow checker model

- **NLL (whole-file mode)**: after each statement, a reference binding
  whose name appears nowhere in the code still to run (rest of its block,
  enclosing loop bodies, `{name}` format captures) has its borrow ended —
  `BorrowExpired` event. Verified on the Rust Book's `r1`/`r2` then `r3 =
  &mut s` example (`samples/borrowing.rs`). Approximation: name-based, so a
  *shadowed* later `r` keeps an earlier `r`'s borrow alive (conservative —
  can only produce a false error, never miss a real one).
- **NLL (live REPL)**: the future is unknown, so a borrow ends lazily when
  something conflicting needs it; using the old reference afterwards is
  then reported as the compile error it would be.
- Partial moves (`let n = u.name;`) are tracked per field path: the rest of
  the struct stays usable and is still dropped.
- Shared reborrows through a reference (`&s[..]` inside `fn(s: &String)`)
  add a borrow count on the original owner, so a returned `&str` keeps the
  `String` borrowed — `s.clear()` while `word` is live errors (E0502).
  Known gap: a `&mut` reborrow returned from a function does *not* keep its
  owner borrowed after the callee's parameter is gone.
- Disjoint field borrows (`&mut p.x` and `&mut p.y` together) are rejected
  here but allowed by rustc — borrows are tracked per binding, not per field.

## Open items

- Integers are all `i64` at runtime: no `i32`/`u8` overflow panics, and no
  type checking of mismatched integer widths.
- ~~`impl Display for T` isn't supported~~ — done 2026-10-25: `{}`/`{:?}`
  on a type with a hand-written `impl Display`/`impl Debug` calls its
  `fmt` as a real call (own frame, `self` borrowing the actual variable);
  `write!`/`writeln!`/`f.write_str` append to the `Formatter`; `write!`
  into a `String` works; `?` works on `Result`/`Option`. Width/fill on
  such types are ignored, **verified against rustc** (`{:>12}` on a
  `write!`-based impl prints unpadded — std only pads via `f.pad`). Known
  gap: a manual `impl Debug` is only used at the top level — nested inside
  a derived `Debug` (e.g. `Vec<Secret>`), the derived-style rendering is
  shown instead. `f.pad`, `f.debug_struct(..)` and `{:.2}`-style flags
  read by the impl (`f.precision()`) aren't modeled.
- ~~Closures and iterator chains unsupported~~ — done 2026-10-26 (see the
  ledger rows above). Not yet: `peekable`, `flat_map`/`flatten`, `windows`/
  `chunks`, `HashMap` collection, `impl Iterator for MyType` (user
  iterators); a closure isn't `Debug`/`Clone`-checked beyond its captures.
- `format!`'s initial capacity isn't modeled exactly (we allocate `len`;
  real `format!` uses an estimate) — don't trust its `.capacity()`.
- No per-binding type tracking: reassigning a variable to a value of a
  completely different type (e.g. `k: &&&&String` then `k = String::from(...)`)
  is accepted silently, where real Rust would reject it at compile time as
  a type mismatch. Found 2026-10-25, not yet fixed.
- ~~Stack/heap panes don't scroll~~ — fixed 2026-10-25: every pane scrolls
  to keep the binding/heap object touched by the current step in view.
