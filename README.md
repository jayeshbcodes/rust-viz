# rust-viz

**Run a real Rust program and watch what ownership actually does:** every
heap allocation, reallocation and free; every move, borrow and drop; and
when each borrow ends. You step through it line by line in your terminal.

rust-viz is a learning tool. It parses real `.rs` source (with
[`syn`](https://crates.io/crates/syn)) and runs it through its own
interpreter, which models the stack, the heap and the borrow checker
explicitly. That's what lets it *show* you things a compiled binary never
reveals: that `Vec::new()` hasn't allocated yet, that `s1 + &s2` reuses
`s1`'s buffer, or that `r1`'s borrow ended two lines before `&mut s`.

```
── step 3 · line 4: takes_ownership(s);
   • `s` moved — ownership left `s`, which can't be used anymore
   • call `takes_ownership` → new stack frame pushed
   [main]
     s = String "hello"  [ptr → #0, len 5, cap 5]  (moved)
   [takes_ownership]
     some_string = String "hello"  [ptr → #0, len 5, cap 5]
   heap #0: String buffer "hello"  len 5 cap 5
── step 5 · line 19: }
   • heap: freed #0
   • `some_string` goes out of scope → dropped
   • `takes_ownership` returns → its stack frame is popped
```

## Usage

```sh
cargo run -- samples/borrowing.rs          # step through a program (n / p / q)
cargo run -- --trace samples/borrowing.rs  # the same trace as plain text
cargo run                                  # live REPL: type Rust, watch memory update
cargo run -- --json samples/borrowing.rs   # machine-readable trace (for editors)
```

The stepper shows the source with the current line highlighted, the stack
(function frames, with block scopes nested inside), the heap (each
allocation and its current owner), a plain-English "what happened" pane,
and the program's output.

## What it models

- **Ownership:** moves vs. `Copy`, moves into and out of functions,
  partial moves out of struct fields, `drop()`, and reverse-order drops at
  scope exit, including drop glue (a `Vec<String>` frees each `String`,
  then its buffer).
- **Borrowing:** `&`/`&mut`, shared-XOR-mutable, non-lexical lifetimes (a
  borrow ends after the reference's last use), implicit reborrows, slices
  as fat pointers, and dangling-reference detection.
- **The heap:** `Vec`/`String` capacity growth and reallocation exactly
  as std does it (checked against `rustc`), no allocation for empty
  collections, `Box`, and temporaries freed at the end of their statement.
- **Errors:** programs `rustc` would reject (E0382, E0499, E0502,
  E0505, E0507, …) stop with a rustc-style explanation at the exact step,
  and runtime panics (index out of bounds, `unwrap` on `None`, overflow)
  stop where they'd panic.

## Scope: what it can run

rust-viz runs a **deliberately limited subset of Rust**, roughly chapters
3–6, 8 and 13 of [the Rust Book](https://doc.rust-lang.org/book/) plus `Box`:

functions, structs/enums/`impl`/methods, trait default methods,
`match`/`if let`/`while let`, `if`/`loop`/`while`/`for` (over ranges,
`&v`, `&mut v`, `.iter()`, `.enumerate()`, `.rev()`, `.chars()`),
`String`/`Vec`/arrays/tuples/slices/`Option`, `println!`/`format!` with
format specs, hand-written `impl Display`/`impl Debug` with `write!`, the
`?` operator, `assert!`s, closures (capture by `&`/`&mut`/`move`, `Fn`/`FnMut`/
`FnOnce`), and lazy iterator chains (`iter`/`into_iter`/`chars`/
`split_whitespace` → `map`/`filter`/`zip`/`enumerate`/`take`/`skip`/`rev`/… →
`collect`/`sum`/`fold`/`find`/`any`/…), and `HashMap` (`insert`/`get`/
`entry().or_insert()`/`remove`/iteration/`map[&k]`, with its real table
growth), and inline modules (`mod x { ... }` with `pub`, `use`,
`crate::`/`self::`/`super::` paths, and E0603 privacy errors).

It does **not** support `HashSet`/`BTreeMap`, `Rc`/`RefCell`, user-defined
iterators (`impl Iterator`), modules in other files (`mod x;`), slice
patterns (`[a, b, ..]`), `str::parse`, or external crates yet. Anything unsupported stops with a clear "rust-viz doesn't
support X yet (line N)" message instead of guessing. Because the
interpreter is dynamically typed, it doesn't type-check: all integers
behave like `i64`.

## Emacs: inline annotations

`emacs/rust-viz.el` shows what each line did to memory right in your
buffer:

```
    let s1 = String::from("hello");     ⟵ alloc #0 · s1 = String "hello"  [ptr → #0, len 5, cap 5]
    let len = calculate_length(&s1);    ⟵ &s1 · → calculate_length() · s = &s1 → "hello" · len = 5
    let r1 = &s;                        ⟵ &s · r1 = &s → "hello, world"
    println!("{r1} and {r2}");          ⟵ r1's borrow ends (last use) · r2's borrow ends (last use)
    let r3 = &mut s;                    ⟵ &mut s · r3 = &mut s → "hello, world"
    r3.push_str("!");                   ⟵ realloc #1 cap 12→24 · s = String "hello, world!"  [ptr → #1, len 13, cap 24]
}                                       ⟵ free #1 · drop s · drop len · free #0 · drop s1 · ← main returns
```

Setup: install the binary (`cargo install --path .`), then

```elisp
(add-to-list 'load-path "/path/to/rust-viz/emacs")
(require 'rust-viz)
(add-hook 'rust-mode-hook #'rust-viz-mode)     ; or rust-ts-mode-hook
```

| Key | Command | What it does |
|---|---|---|
| `C-c C-v r` | `rust-viz-run` | Annotate every executed line with its latest effect (loop lines show `×N`), and open a window below with the stack and heap right after the line at point ran — it follows the cursor. Drag its border to resize; set `rust-viz-detail-window-position` to `right` to split sideways instead. With `rust-viz-mode` on, saving re-annotates too (quietly — it refreshes the window only if it's already open). |
| `C-c C-v s` | `rust-viz-step` | Step through execution: `n`/`SPC` next, `p`/`DEL` back, `g`/`G` first/last, `q` quit. The current line is highlighted, annotations replay up to that step, and the window below shows the full stack, heap, events and output so far. |
| `C-c C-v c` | `rust-viz-clear` | Remove annotations. |

The buffer's text is piped in, so unsaved edits and bare snippets (no
`fn main`) work. If the program has an error, it's shown in red on the
line where it stopped. The commands work without `rust-viz-mode` too.
Any editor can build the same integration on `rust-viz --json <file|->`;
the format is documented at the top of `src/json.rs`.

## Accuracy

The whole point is to teach a *correct* mental model, so accuracy is
tested two ways:

- `tests/golden.rs` runs each program in `samples/` both through real
  `rustc` and through rust-viz, and requires identical output. Capacity
  growth is checked this way too, by printing `.capacity()`. Each run must
  also end with zero live heap allocations, so a wrong drop model fails
  the test.
- [`NOTES.md`](NOTES.md) is a fidelity ledger: for each kind of value, the
  real-Rust fact (from the Rust Reference or a `size_of` check), how
  rust-viz models it, and every known divergence.

```sh
cargo test
emacs --batch -L emacs -l emacs/rust-viz-test.el -f ert-run-tests-batch-and-exit
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.
