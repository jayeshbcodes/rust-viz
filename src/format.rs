//! The pure-text half of `println!`/`format!`: splitting a format string
//! into literal pieces and `{...}` placeholders, and applying a
//! placeholder's spec (`{:>5}`, `{:.2}`, `{:05}`, `{:#?}`, …). Evaluating
//! the arguments themselves lives in the interpreter.

#[derive(Debug, PartialEq)]
pub enum Piece {
    Lit(String),
    /// `arg` is `""` (next positional), digits (explicit position), or a
    /// name (named argument / inline capture).
    Arg { arg: String, spec: Spec },
}

#[derive(Debug, Default, PartialEq)]
pub struct Spec {
    pub fill: Option<char>,
    pub align: Option<char>,
    pub plus: bool,
    pub alt: bool,
    pub zero: bool,
    pub width: Option<usize>,
    pub precision: Option<usize>,
    /// `""` (Display), `"?"` (Debug), or `x`/`X`/`b`/`o`/`e`.
    pub ty: String,
}

impl Spec {
    pub fn is_debug(&self) -> bool {
        self.ty == "?"
    }
}

pub fn parse_format(fmt: &str) -> Result<Vec<Piece>, String> {
    let mut pieces = Vec::new();
    let mut lit = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                lit.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                lit.push('}');
            }
            '}' => return Err("unmatched `}` in format string".into()),
            '{' => {
                let mut inside = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c2) => inside.push(c2),
                        None => return Err("unterminated format placeholder".into()),
                    }
                }
                if !lit.is_empty() {
                    pieces.push(Piece::Lit(std::mem::take(&mut lit)));
                }
                let (arg, spec) = match inside.split_once(':') {
                    Some((a, s)) => (a.trim().to_string(), parse_spec(s)),
                    None => (inside.trim().to_string(), Spec::default()),
                };
                pieces.push(Piece::Arg { arg, spec });
            }
            other => lit.push(other),
        }
    }
    if !lit.is_empty() {
        pieces.push(Piece::Lit(lit));
    }
    Ok(pieces)
}

/// `[[fill]align][+][#][0][width][.precision][type]`
pub fn parse_spec(s: &str) -> Spec {
    let c: Vec<char> = s.chars().collect();
    let mut spec = Spec::default();
    let mut i = 0;
    let is_align = |ch: char| matches!(ch, '<' | '^' | '>');
    if c.len() >= 2 && is_align(c[1]) {
        spec.fill = Some(c[0]);
        spec.align = Some(c[1]);
        i = 2;
    } else if !c.is_empty() && is_align(c[0]) {
        spec.align = Some(c[0]);
        i = 1;
    }
    if c.get(i) == Some(&'+') {
        spec.plus = true;
        i += 1;
    }
    if c.get(i) == Some(&'#') {
        spec.alt = true;
        i += 1;
    }
    if c.get(i) == Some(&'0') {
        spec.zero = true;
        i += 1;
    }
    let digits = |i: &mut usize| -> Option<usize> {
        let start = *i;
        while c.get(*i).is_some_and(|ch| ch.is_ascii_digit()) {
            *i += 1;
        }
        (start != *i).then(|| c[start..*i].iter().collect::<String>().parse().unwrap())
    };
    spec.width = digits(&mut i);
    if c.get(i) == Some(&'.') {
        i += 1;
        spec.precision = digits(&mut i);
    }
    spec.ty = c[i..].iter().collect();
    spec
}

/// Applies sign, zero-padding and width/alignment to an already-rendered
/// value. Numbers default to right-aligned, everything else to left —
/// same defaults as std.
pub fn pad(rendered: String, spec: &Spec, numeric: bool) -> String {
    let mut s = rendered;
    if spec.plus && numeric && !s.starts_with('-') {
        s.insert(0, '+');
    }
    let Some(width) = spec.width else { return s };
    let len = s.chars().count();
    if len >= width {
        return s;
    }
    let missing = width - len;
    if spec.zero && numeric {
        let sign_len = if s.starts_with('-') || s.starts_with('+') { 1 } else { 0 };
        let (sign, digits) = s.split_at(sign_len);
        return format!("{sign}{}{digits}", "0".repeat(missing));
    }
    let fill = spec.fill.unwrap_or(' ').to_string();
    let align = spec.align.unwrap_or(if numeric { '>' } else { '<' });
    match align {
        '>' => format!("{}{s}", fill.repeat(missing)),
        '^' => format!("{}{s}{}", fill.repeat(missing / 2), fill.repeat(missing - missing / 2)),
        _ => format!("{s}{}", fill.repeat(missing)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(spec: &str, s: &str, numeric: bool) -> String {
        pad(s.to_string(), &parse_spec(spec), numeric)
    }

    #[test]
    fn padding_matches_std() {
        assert_eq!(render(">5", "42", true), format!("{:>5}", 42));
        assert_eq!(render("<5", "ab", false), format!("{:<5}", "ab"));
        assert_eq!(render("^5", "c", false), format!("{:^5}", "c"));
        assert_eq!(render("05", "-7", true), format!("{:05}", -7));
        assert_eq!(render("+", "3", true), format!("{:+}", 3));
        assert_eq!(render("*^7", "hi", false), format!("{:*^7}", "hi"));
    }

    #[test]
    fn parses_pieces_and_escapes() {
        let p = parse_format("a {{b}} {x:?} {}").unwrap();
        assert_eq!(p.len(), 4);
        assert!(matches!(&p[1], Piece::Arg { arg, spec } if arg == "x" && spec.is_debug()));
    }
}
