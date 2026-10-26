// Rust Book ch. 10 / std::fmt — implementing Display (and Debug) by hand.
use std::fmt;
use std::fmt::Write;

struct Point {
    x: i32,
    y: i32,
}

impl fmt::Display for Point {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "({}, {})", self.x, self.y)
    }
}

struct Line {
    from: Point,
    to: Point,
    label: String,
}

impl fmt::Display for Line {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.label)?;
        write!(f, "{} -> {}", self.from, self.to)?;
        Ok(())
    }
}

enum Shape {
    Circle(u32),
    Square(u32),
}

impl fmt::Display for Shape {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Shape::Circle(r) => write!(f, "circle r={r}"),
            Shape::Square(s) => {
                f.write_str("square ")?;
                write!(f, "side={}", s)
            }
        }
    }
}

struct Secret(String);

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Secret(<{} chars>)", self.0.len())
    }
}

fn main() {
    let p = Point { x: 1, y: -2 };
    println!("p = {p}");
    println!("[{:>12}]", p);
    let line = Line { from: Point { x: 0, y: 0 }, to: p, label: String::from("diag") };
    println!("{}", line);
    let text = line.to_string();
    println!("{} chars: {text}", text.len());
    println!("{} / {}", Shape::Circle(3), Shape::Square(4));
    println!("{:?}", Secret(String::from("hunter2")));

    let mut report = String::new();
    writeln!(report, "line one {}", 1).unwrap();
    write!(report, "line two").unwrap();
    println!("{report}");
}
