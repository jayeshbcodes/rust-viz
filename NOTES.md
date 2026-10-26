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
- `impl Display for T` (`write!(f, ...)`) isn't supported yet, so `{}` on a
  user type is rejected even when the program implements `Display`.
- Closures and iterator adaptor chains (`.iter().map(..).collect()`) are
  unsupported; `for` handles ranges, `&v`/`&mut v`/`v`, `.iter()`,
  `.iter_mut()`, `.enumerate()`, `.rev()`, `.chars()` directly.
- `format!`'s initial capacity isn't modeled exactly (we allocate `len`;
  real `format!` uses an estimate) — don't trust its `.capacity()`.
- No per-binding type tracking: reassigning a variable to a value of a
  completely different type (e.g. `k: &&&&String` then `k = String::from(...)`)
  is accepted silently, where real Rust would reject it at compile time as
  a type mismatch. Found 2026-10-25, not yet fixed.
- ~~Stack/heap panes don't scroll~~ — fixed 2026-10-25: every pane scrolls
  to keep the binding/heap object touched by the current step in view.
