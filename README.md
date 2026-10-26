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
3–6 of [the Rust Book](https://doc.rust-lang.org/book/) plus `Box`:

functions, structs/enums/`impl`/methods, trait default methods,
`match`/`if let`/`while let`, `if`/`loop`/`while`/`for` (over ranges,
`&v`, `&mut v`, `.iter()`, `.enumerate()`, `.rev()`, `.chars()`),
`String`/`Vec`/arrays/tuples/slices/`Option`, `println!`/`format!` with
format specs, and `assert!`s.

It does **not** support closures, iterator chains (`.map(..).collect()`),
`HashMap`, `Rc`/`RefCell`, `impl Display`, `?`, modules, or external
crates yet. Anything unsupported stops with a clear "rust-viz doesn't
support X yet (line N)" message instead of guessing. Because the
interpreter is dynamically typed, it doesn't type-check: all integers
behave like `i64`.

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
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.
