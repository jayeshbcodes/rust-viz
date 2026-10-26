// Rust Book ch. 6 — enums, Option, match, if let.
#[derive(Debug)]
enum Message {
    Quit,
    Move { x: i32, y: i32 },
    Write(String),
    ChangeColor(i32, i32, i32),
}

impl Message {
    fn describe(&self) -> String {
        match self {
            Message::Quit => String::from("quit"),
            Message::Move { x, y } => format!("move to {x},{y}"),
            Message::Write(text) => format!("write {text}"),
            Message::ChangeColor(r, g, b) if *r > 200 => format!("reddish {r} {g} {b}"),
            Message::ChangeColor(..) => String::from("some color"),
        }
    }
}

fn plus_one(x: Option<i32>) -> Option<i32> {
    match x {
        None => None,
        Some(i) => Some(i + 1),
    }
}

fn main() {
    let msgs = vec![
        Message::Write(String::from("hi")),
        Message::Move { x: 1, y: 2 },
        Message::ChangeColor(255, 0, 0),
        Message::Quit,
    ];
    for m in &msgs {
        println!("{}", m.describe());
    }
    println!("{:?}", msgs[1]);

    let five = Some(5);
    println!("{:?} {:?}", plus_one(five), plus_one(None));

    let mut stack = vec![1, 2, 3];
    while let Some(top) = stack.pop() {
        println!("popped {top}");
    }
    if let Some(x) = five {
        println!("got {x}");
    }

    let owned = Message::Write(String::from("moved out"));
    if let Message::Write(text) = owned {
        println!("{text}");
    }

    let grade = 85;
    let letter = match grade {
        90..=100 => 'A',
        80..=89 => 'B',
        _ => 'F',
    };
    println!("{letter}");
}
