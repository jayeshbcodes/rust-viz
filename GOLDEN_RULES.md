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

A borrow used only inside a call is even shorter: it ends when the call
returns a plain value. `v.contains(&1)` borrows `v`, returns a `bool` that
points at nothing, and the borrow is over before the `if` body runs, so
mutating inside is fine. Braces decide when owned values are *dropped*,
not how long borrows last.

```rust ok
fn main() {
    let mut v = vec![1, 2];
    if v.contains(&1) {
        v.push(3);
    }
    println!("{:?}", v);
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

### 20. A move copies the header, never the heap data

Moving a `Vec` or `String` copies its 24-byte stack header (ptr, len, cap);
the elements stay exactly where they are on the heap and just get a new
owner. That's why moving a million-element Vec costs the same as moving an
empty one. `std::mem::take(&mut v)` uses this: it's
`mem::replace(&mut v, Default::default())`, which moves the old value out
(header only) and leaves a valid empty one behind, so you can take
ownership of something you only have `&mut` access to.

```rust ok
fn main() {
    let mut v = vec![1, 2, 3];
    let old = std::mem::take(&mut v);
    println!("{:?} {:?} {}", old, v, v.capacity());
}
```

In rust-viz the buffer keeps its `#` number: `old` owns the same heap
object `v` had, and `v` is left with no heap buffer at all (cap 0).

### 21. Unsized types (`str`, `[T]`, `dyn Trait`) only live behind a pointer

A stack slot needs a size known at compile time. `str`, `[T]` and
`dyn Trait` have none (`"hi"` is 2 bytes, `"hello"` 5), so you can never
hold one directly, only through a pointer that carries the missing size:
`&str` and `&[T]` are *fat pointers* (ptr + len, 16 bytes), `&dyn Trait`
is ptr + vtable. Every generic `T` silently requires `T: Sized`, so
generic code like `mem::replace` can't take a bare `str` either;
`T: ?Sized` opts out, for code that only uses `T` behind a reference.

```rust E0277
fn main() {
    let s: &str = "hi";
    let owned = *s;
    println!("{}", owned.len());
}
```

Use the `&str` itself, or make an owned `String` with `s.to_string()`.
*Java* never meets this: every object is on the heap behind a reference,
and all references are the same size.

### 22. Put trait bounds on impls, not structs; a method exists only where its bounds hold

`impl<K: Hash + Eq, V> MyMap<K, V>` declares `K` and `V`, requires
`K: Hash + Eq`, and adds methods to `MyMap<K, V>`. Keep the struct itself
unbounded: a bound on the struct must be repeated on every impl and every
function that mentions the type, even ones that never use it. Instead,
split methods into impl blocks by what they need. Calling a method whose
impl bounds your types don't meet is a compile error (E0599 "the method
exists ... but its trait bounds were not satisfied").

```rust ok
use std::fmt::Display;
struct Pair<T> {
    a: T,
    b: T,
}
impl<T> Pair<T> {
    fn new(a: T, b: T) -> Self {
        Pair { a, b }
    }
}
impl<T: Display> Pair<T> {
    fn show(&self) {
        println!("({}, {})", self.a, self.b);
    }
}
fn main() {
    let p = Pair::new(1, 2);
    p.show();
    let names = Pair::new("x", "y");
    names.show();
}
```

A `Pair<Secret>` (no `Display`) can still be built with `new`, but has no
`show`. The real `HashMap` does this: `len` needs nothing, `insert` needs
`K: Eq + Hash`. The compiler picks the one impl defining the method, checks
its bounds against the concrete types, and generates a copy per type
(monomorphization). *Java* puts bounds on the class
(`class MyMap<K extends Comparable<K>, V>`) and erases generics at runtime,
so it can't have methods that exist only for some type arguments.
rust-viz doesn't check impl bounds before a call; rustc does.

### 23. A signature is a contract: `&` only reads, `&mut` may change, `T` takes it, and bounds list what's allowed

What a function can do to its inputs is written in its signature and
enforced by the compiler, not by convention: through a `&T` it cannot
change the value, `&mut T` may change it (and the caller had to write
`&mut` too), an owned `T` is moved in. Trait bounds list the only
operations it may use on a generic input: `fn f<Q: Hash>(key: &Q)` can
hash `key` and nothing else. So the signature alone tells you what a call
can do. (Not tracked: printing, files, globals, and types built for
changes behind `&`, like `RefCell`.)

```rust E0596
fn total(v: &Vec<i32>) -> i32 {
    v.push(0);
    v.iter().sum()
}
fn main() {
    let v = vec![1, 2, 3];
    println!("{}", total(&v));
}
```

*Java's* `final List<T> list` only stops reassigning the variable;
`list.add(..)` still works, so read-only access comes from conventions
(`Collections.unmodifiableList`, immutable classes) rather than the type.

### 24. `==` on references compares the values, never the addresses

`a == b` is `PartialEq::eq(&a, &b)`, and for references std's impl follows
them and compares what they point to. So two `&String`s holding the same
text are equal even though they point at different buffers, and a
`&String` equals a `&str` literal in the binary. To ask "do these point
at the very same place?", say so: `std::ptr::eq(a, b)`.

```rust ok
fn main() {
    let a = String::from("apple");
    let b = String::from("apple");
    let (ra, rb) = (&a, &b);
    println!("{} {} {}", ra == rb, ra == "apple", std::ptr::eq(ra, rb));
}
```

`Eq` is `PartialEq` plus a promise that every value equals itself; `f64`
is only `PartialEq` (`NaN != NaN`), which is why it can't be a `HashMap`
key. *Java* is the other way round: `==` on objects compares addresses and
`.equals()` compares contents, a classic bug that Rust's `==` can't make.

### 25. `Borrow` points from the owner to its view: `K: Borrow<Q>` is `String → str`

`impl Borrow<str> for String` reads "a String can be *viewed as* a str".
The trait sits on the owner and names the view. A view can only drop
information (a `String` is ptr+len+cap, a `str` is just the bytes), so the
reverse, `str → String`, isn't a view at all: it would need a new
allocation. That's why a map's lookup converts its *stored* keys,
`k.borrow() == key` (`&String → &str`, then `&str == &str`), and never the
caller's key, which already arrives as `&str`. The trait also promises the
view hashes and compares exactly like the owner, which is what lets
`HashMap<String, _>` take `get("apple")` without allocating.

```rust ok
use std::borrow::Borrow;
use std::collections::HashMap;
fn main() {
    let owned = String::from("apple");
    let view: &str = owned.borrow();
    let mut m: HashMap<String, i32> = HashMap::new();
    m.insert(owned.clone(), 3);
    println!("{view} {:?}", m.get("apple"));
}
```

Memory hook: owner → view, never view → owner; `.borrow()` is called on
what the map *owns*.

### 26. **Monomorphization**: a generic function becomes one copy per type you use

The compiler stamps out a separate, specialized copy of generic code for
every concrete type it's called with. `bucket_of::<str>` and
`bucket_of::<i32>` are two functions in the binary, each calling its
type's `hash` *directly*, chosen at compile time (static dispatch). Generic
code is therefore exactly as fast as hand-written per-type code. The costs
are a bigger binary and longer compile times. It's also why bounds are
checked per concrete type, and why a method can exist only for some type
arguments (rule 22).

```rust ok
use std::fmt::Display;
fn describe<T: Display>(x: T) -> String {
    format!("<{x}>")
}
fn main() {
    println!("{} {} {}", describe(42), describe("hi"), describe(1.5));
}
```

Three copies of `describe` exist after compiling: for `i32`, `&str`, `f64`.
*Java* does the opposite, **type erasure**: one compiled copy where `T`
becomes `Object`, and `x.toString()` is picked at runtime by a virtual
call (runtime polymorphism). Rust has that too, opt-in, with `dyn Trait`
(Rust Book ch. 17): one copy plus a vtable lookup.
