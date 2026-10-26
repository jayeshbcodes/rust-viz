// Inline modules: paths (`crate::`, `self::`, `super::`), `use`, `pub`,
// and types defined inside a module.
mod ops {
    pub const ZERO: f64 = 0.0;

    pub fn add(a: f64, b: f64) -> f64 {
        a + b
    }

    pub fn div(a: f64, b: f64) -> Result<f64, String> {
        if is_zero(b) {
            return Err(String::from("division by zero"));
        }
        Ok(a / b)
    }

    fn is_zero(x: f64) -> bool {
        x == ZERO
    }

    pub mod stats {
        pub fn mean(xs: &[f64]) -> f64 {
            let mut total = 0.0;
            for x in xs {
                total = super::add(total, *x);
            }
            total / xs.len() as f64
        }
    }
}

mod parser {
    use crate::ops;

    #[derive(Debug)]
    pub enum Token {
        Num(f64),
        Op(char),
    }

    pub struct Expr {
        pub left: f64,
        pub op: char,
        pub right: f64,
    }

    impl Expr {
        pub fn new(left: f64, op: char, right: f64) -> Self {
            Expr { left, op, right }
        }

        pub fn evaluate(&self) -> Result<f64, String> {
            match self.op {
                '+' => Ok(ops::add(self.left, self.right)),
                '/' => ops::div(self.left, self.right),
                op => Err(format!("unknown operator {op}")),
            }
        }
    }

    pub fn parse(tokens: &[Token]) -> Expr {
        match (&tokens[0], &tokens[1], &tokens[2]) {
            (Token::Num(l), Token::Op(o), Token::Num(r)) => self::Expr::new(*l, *o, *r),
            other => panic!("can't parse {other:?}"),
        }
    }
}

use ops::stats::mean;
use parser::Expr;

fn main() {
    use parser::Token::{Num, Op};
    let tokens = vec![Num(3.0), Op('+'), Num(4.0)];
    let e: Expr = parser::parse(&tokens);
    println!("{tokens:?} → {} {} {} = {:?}", e.left, e.op, e.right, e.evaluate());
    let bad = crate::parser::parse(&[Num(1.0), Op('/'), Num(0.0)]);
    println!("{:?}", bad.evaluate());
    let manual = parser::Expr::new(10.0, '/', 4.0);
    println!("{:?}", manual.evaluate());
    let scores = vec![2.0, 4.0, 9.0];
    println!("mean = {}", mean(&scores));
    println!("zero = {}", ops::ZERO);
}
