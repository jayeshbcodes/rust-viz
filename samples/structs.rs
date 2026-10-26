// Rust Book ch. 5 — structs, methods, and associated functions.
#[derive(Debug)]
struct Rect {
    width: u32,
    length: u32,
}

impl Rect {
    fn square(size: u32) -> Self {
        Self { width: size, length: size }
    }

    fn area(&self) -> u32 {
        self.width * self.length
    }

    fn scale(&self, scale: u32) -> Self {
        Self {
            width: self.width * scale,
            length: self.length * scale,
        }
    }

    fn grow(&mut self, by: u32) {
        self.width += by;
    }
}

#[derive(Debug, Clone)]
struct User {
    name: String,
    email: String,
    active: bool,
}

fn main() {
    let mut rect = Rect { width: 100, length: 50 };
    println!("Rect: {rect:?}");
    println!("Area: {}", rect.area());
    let rect1 = rect.scale(2);
    println!("Rect: {rect1:#?}");
    rect.grow(5);
    println!("{} {}", rect.width, Rect::square(3).area());

    let user1 = User { name: String::from("ana"), email: String::from("a@x.io"), active: true };
    let user2 = User { email: String::from("b@x.io"), ..user1 };
    // user1.name moved into user2, but user1.email is still usable.
    println!("{} {} {}", user1.email, user2.name, user2.active);
}
