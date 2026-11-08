# Rust golden rules

The rules that keep coming up when you learn Rust, each with the smallest
program that shows it. Every example here is checked by `tests/golden_rules.rs`:
blocks marked `ok` must compile and print the same under `rustc` and rust-viz,
and blocks marked with an error code must be rejected by both with that code.
Run any of them through rust-viz to watch it happen.

## Ownership and moves

### 1. Every value has one owner; `=` moves it

Assigning a `String` (or passing it to a function) moves ownership. The old
variable can't be used afterwards.

```rust E0382
fn main() {
    let s1 = String::from("hi");
    let s2 = s1;
    println!("{s1} {s2}");
}
```

*Java:* both variables would point at the same object. Rust has exactly one
owner, so the heap buffer is freed exactly once.

### 2. `Copy` is for types that own no heap memory

`i32`, `f64`, `bool`, `char` and references are fully described by their
stack bytes, so `=` duplicates them instead of moving.

```rust ok
fn main() {
    let a = 5;
    let b = a;
    println!("{a} {b}");
}
```

A `String` is a stack header (ptr, len, cap) pointing at a heap buffer;
copying just the header would give two owners of one buffer. So it moves.

### 3. Formatting macros borrow; they never move

`println!` and `format!` take `&` of each argument for you.

```rust ok
fn main() {
    let s1 = String::from("Hello");
    let s2 = format!("{s1}, world");
    println!("{s1} | {s2}");
}
```

`format!` allocates a brand-new String; `s1`'s buffer is only read.

## Borrowing

### 4. Many `&` readers, or one `&mut` writer, never both

```rust E0499
fn main() {
    let mut s = String::from("hi");
    let r1 = &mut s;
    let r2 = &mut s;
    println!("{r1} {r2}");
}
```

```rust E0502
fn main() {
    let mut v = vec![1, 2, 3];
    let first = &v[0];
    v.push(4);
    println!("{first}");
}
```

`push` may reallocate the buffer `first` points into, which is why rustc
refuses it.

### 5. While a borrow is in use, the owner can't be moved, changed or dropped

```rust E0505
fn main() {
    let s = String::from("hi");
    let r = &s;
    let t = s;
    println!("{r} {t}");
}
```

"In use" means *used later*: a borrow ends at its last use (non-lexical
lifetimes). Same lines, reordered, compile fine:

```rust ok
fn main() {
    let s = String::from("hi");
    let r = &s;
    println!("{r}");
    let t = s;
    println!("{t}");
}
```

### 6. No dangling references, ever (in safe Rust)

A reference can't outlive what it points at.

```rust E0106
fn dangle() -> &String {
    let s = String::from("hi");
    &s
}
fn main() {
    let r = dangle();
    println!("{r}");
}
```

Return the owned value instead (`-> String`), and ownership moves to the
caller. *Java* avoids dangling pointers with a garbage collector; Rust proves
it at compile time instead.

### 7. A struct can't borrow itself, but it can borrow from outside

Moving a struct copies its bytes to a new place, so a reference into itself
would dangle; the compiler won't allow one. A borrow of *someone else's*
data is fine: the lifetime `'a` ties the struct to the owner.

```rust ok
struct View<'a> {
    bytes: &'a [u8],
}
fn main() {
    let owner = String::from("hello");
    let view = View { bytes: owner.as_bytes() };
    println!("{}", view.bytes.len());
}
```

## Methods

### 8. The receiver says what a method does to its value

| Receiver | Method gets | Caller needs |
|---|---|---|
| `&self` | a read-only borrow | any `s` |
| `&mut self` | an exclusive, mutable borrow | `let mut s` |
| `self` | ownership | `s` is gone afterwards |

Forgetting `mut` in `&mut self`:

```rust E0596
struct Counter {
    n: Vec<i32>,
}
impl Counter {
    fn add(&self, x: i32) {
        self.n.push(x);
    }
}
fn main() {
    let mut c = Counter { n: Vec::new() };
    c.add(1);
}
```

Calling a `&mut self` method needs a `mut` variable, too:

```rust E0596
fn main() {
    let s = String::from("hi");
    s.push_str("!");
    println!("{s}");
}
```

`mut self` is not `&mut self`: without the `&`, the method takes the
value by ownership (it's the `self` row, plus permission to change its own
copy), so the caller's variable is moved and gone after the call. The error
points at the *caller*, not the signature:

```rust E0382
struct Counter {
    n: i32,
}
impl Counter {
    fn bump(mut self) {
        self.n += 1;
    }
}
fn main() {
    let c = Counter { n: 0 };
    c.bump();
    println!("{}", c.n);
}
```

*Java:* any method can change `this`. In Rust the signature alone tells you.

### 9. Methods that change in place return `()`

`push_str`, `push`, `clear` change their receiver and return nothing.

```rust E0277
fn main() {
    let mut s1 = String::from("Hello");
    let s2 = s1.push_str(" Rust");
    println!("{s1} {s2}");
}
```

*Java's* `StringBuilder.append` returns the builder; Rust's `push_str` is
like a `void` method.

## Patterns

### 10. Struct patterns match by field name; tuple patterns by position

```rust ok
enum Message {
    Resize { width: u64, height: u64 },
    Color(u8, u8, u8),
}
fn main() {
    let msgs = [Message::Resize { height: 30, width: 10 }, Message::Color(1, 2, 3)];
    for m in &msgs {
        match m {
            Message::Resize { width, height } => println!("{width}x{height}"),
            Message::Color(r, g, b) => println!("{r} {g} {b}"),
        }
    }
}
```

Names after `field:` are new variables, never types: `{ width: w, .. }`
binds `w`; `{ x: u64 }` binds a variable called `u64`.

## Modules

### 11. The parent declares a module; `use` only shortens a path

`mod ops;` means "paste `ops.rs` here as `mod ops { ... }`". No `mod` line,
no module: files are never picked up automatically. Write `mod` once, in the
parent; write `use` wherever you want a shorter name.

```rust ok
mod ops {
    pub fn add(a: i32, b: i32) -> i32 {
        a + b
    }
}
use ops::add;
fn main() {
    println!("{}", add(2, 3));
}
```

*Java:* `mod` ≈ package (declared by the parent, not the file), `use` ≈
`import`.

### 12. Private by default; `pub` opens one door at a time

A private item is visible in its own module and that module's children.
Reaching it from outside needs `pub` on the item *and* on every module
along the path.

```rust E0603
mod ops {
    fn secret() -> i32 {
        42
    }
}
fn main() {
    println!("{}", ops::secret());
}
```

## Traits

### 13. `{}` needs `Display`; you implement it, there's no derive

```rust E0277
struct Point {
    x: i32,
}
fn main() {
    let p = Point { x: 1 };
    println!("{p}");
}
```

```rust ok
use std::fmt;
struct Point {
    x: i32,
}
impl fmt::Display for Point {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "({})", self.x)
    }
}
fn main() {
    let p = Point { x: 1 };
    println!("{p}");
}
```

`impl Trait for Type` ≈ *Java's* `implements`, one block per trait. You may
implement a trait only if you own the trait or the type (the orphan rule).

## Strings

### 14. `String` owns (ptr, len, cap); `&str` borrows (ptr, len)

A `String` grows by reallocating: new capacity is `max(2 × cap, needed, 8)`.
A `&str` can never grow; it's a view into bytes someone else owns.

```rust ok
fn main() {
    let mut s = String::from("hello");
    s.push_str(" world");
    let word: &str = &s[0..5];
    println!("{word} {} {}", s.len(), s.capacity());
}
```

`len()` counts bytes, not characters: `"é".len()` is 2.

### 15. A `char` is 1–4 bytes; continuation bytes look like `10xxxxxx`

UTF-8 marks every byte: `0xxxxxxx` is a whole ASCII character,
`110xxxxx`/`1110xxxx`/`11110xxx` start a 2/3/4-byte character, and
`10xxxxxx` continues one. From any position you can find a character's
start by skipping continuation bytes, and from a start byte you can read
the character's length (its number of leading 1s).

```rust ok
fn main() {
    let s = "hé!";
    let bytes = s.as_bytes();
    let continuation = bytes.iter().filter(|b| **b & 0b1100_0000 == 0b1000_0000).count();
    println!("{} bytes, {} chars, {} continuation byte", s.len(), s.chars().count(), continuation);
}
```

That's why `s.len()` counts bytes, why you can't index a `String` by
position (`s[1]` doesn't compile), and why slicing in the middle of a
character (`&"hé"[0..2]`) panics. *Java* strings are UTF-16: an emoji takes
two `char`s (a surrogate pair), the same idea with 2-byte units.

### 16. Shrinking a Vec keeps its buffer; rebuilding it allocates a new one

`truncate`, `clear` and `pop` only lower the length: same heap buffer, same
capacity, nothing copied. Assigning a fresh copy (`v = v[..i].to_vec()`)
allocates a new buffer, copies the kept elements, and frees the old one.

```rust ok
fn main() {
    let mut v = vec![1, 2, 3, 4];
    v.truncate(2);
    println!("{:?} cap {}", v, v.capacity());
    v = v[..1].to_vec();
    println!("{:?} cap {}", v, v.capacity());
}
```

In rust-viz the `truncate` line shows no heap event; the `to_vec` line shows
an allocation and a free. To hand memory back on purpose, use
`shrink_to_fit()`.

### 17. A small fixed-size stack buffer beats a temporary heap value

`c.to_string()` allocates a heap `String` just to hold 1–4 bytes, then
frees it. When you know the maximum size up front, a stack array does the
same job with no heap at all: `[u8; 4]` fits any `char` in UTF-8.

```rust ok
fn main() {
    let mut out: Vec<u8> = Vec::new();
    for c in ['h', 'é', '😀'] {
        let mut buf = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }
    println!("{:?}", out);
}
```

In rust-viz the only heap object is `out`'s buffer; the `to_string()`
version shows an allocate + free for every character. `encode_utf8`
returns a `&mut str` that borrows `buf`, so it can't outlive it (rule 6).

### 18. An owned argument that isn't stored is dropped when the function returns

Passing a `String` by value moves it in; if the function doesn't keep it
(store it, return it), it's freed at the function's end. `HashMap::insert`
with a key that's already there is the classic case: the map keeps its
*old* key and updates the value, so the new key you allocated is thrown
away.

```rust ok
use std::collections::HashMap;
fn main() {
    let mut m = HashMap::new();
    m.insert(String::from("apple"), 3);
    let old = m.insert(String::from("apple"), 7);
    println!("{old:?} {:?}", m.get("apple"));
}
```

In rust-viz the second `String::from("apple")` is allocated, moved into
`insert`, and freed right there. Nothing in the output shows it. To update
without allocating, borrow with a `&str`: `*m.get_mut("apple").unwrap() += 1`.
*Java* behaves the same (`put` keeps the old key), but the discarded key
just becomes garbage for the GC to collect later.

### 19. A string literal lives in the binary; `String::from` copies it to the heap

`"apple"` is a `&'static str`: its bytes are baked into the compiled
program's read-only memory, so using it allocates nothing. Every
`String::from("apple")` allocates a *new* heap buffer and copies those bytes
in; two calls give two independent Strings (each owns its buffer and could
change it). So APIs that only need to *look* take `&str`, and only ones that
*keep* the text take `String`.

```rust ok
use std::collections::HashMap;
fn main() {
    let mut m: HashMap<String, i32> = HashMap::new();
    m.insert(String::from("apple"), 1);
    *m.get_mut("apple").unwrap() += 1;
    println!("{:?} {}", m.get("apple"), m.contains_key("pear"));
}
```

Here only `insert` allocates; `get_mut`, `get` and `contains_key` borrow
the literals straight from the binary. *Java* keeps literals in its string
pool much the same way, and `new String("apple")` copies like
`String::from`, but in Rust the cost is visible in the type.
