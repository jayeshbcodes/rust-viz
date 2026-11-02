//! Keeps GOLDEN_RULES.md honest: every ```rust block in it is tagged `ok`
//! (must compile and print the same under rustc and rust-viz) or with an
//! error code like `E0382` (both must reject it with that code).

use std::fs;
use std::process::Command;

use rust_viz::interp::Interpreter;

struct Example {
    line: usize,
    tag: String,
    code: String,
}

fn examples() -> Vec<Example> {
    let doc = fs::read_to_string("GOLDEN_RULES.md").expect("read GOLDEN_RULES.md");
    let mut out = Vec::new();
    let mut cur: Option<Example> = None;
    for (i, line) in doc.lines().enumerate() {
        match (&mut cur, line.strip_prefix("```")) {
            (None, Some(info)) if info.starts_with("rust ") => {
                cur = Some(Example { line: i + 1, tag: info["rust ".len()..].trim().to_string(), code: String::new() });
            }
            (Some(_), Some("")) => out.push(cur.take().unwrap()),
            (Some(ex), _) => {
                ex.code.push_str(line);
                ex.code.push('\n');
            }
            _ => {}
        }
    }
    out
}

/// rustc's verdict: Ok(stdout) or Err(stderr).
fn rustc(code: &str, n: usize) -> Result<String, String> {
    let dir = std::env::temp_dir().join(format!("rust-viz-rules-{}-{n}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let (src, bin) = (dir.join("ex.rs"), dir.join("ex_bin"));
    fs::write(&src, code).unwrap();
    let compile = Command::new("rustc").arg(&src).args(["--edition", "2024", "-A", "warnings", "-o"]).arg(&bin).output().expect("invoke rustc");
    let result = if compile.status.success() {
        let run = Command::new(&bin).output().expect("run example");
        Ok(String::from_utf8_lossy(&run.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&compile.stderr).into_owned())
    };
    let _ = fs::remove_dir_all(&dir);
    result
}

#[test]
fn every_golden_rule_example_behaves_as_tagged() {
    let examples = examples();
    assert!(examples.len() >= 14, "found only {} examples", examples.len());
    let mut failures = Vec::new();
    for (n, ex) in examples.iter().enumerate() {
        let (interp, viz) = Interpreter::run_program(&ex.code);
        let real = rustc(&ex.code, n);
        let problem = match (ex.tag.as_str(), &real, &viz) {
            ("ok", Ok(out), Ok(())) if *out == interp.output => None,
            ("ok", Ok(out), Ok(())) => Some(format!("output differs: rustc {out:?}, rust-viz {:?}", interp.output)),
            ("ok", Err(e), _) => Some(format!("rustc rejected it:\n{e}")),
            ("ok", _, Err(e)) => Some(format!("rust-viz rejected it: {e}")),
            (code, Err(e), Err(v)) if e.contains(code) && v.to_string().contains(code) => None,
            (code, Err(e), _) if !e.contains(code) => Some(format!("rustc didn't report {code}:\n{e}")),
            (code, Ok(_), _) => Some(format!("rustc accepted it, expected {code}")),
            (code, _, Ok(())) => Some(format!("rust-viz accepted it, expected {code}")),
            (code, _, Err(v)) => Some(format!("rust-viz said `{v}`, expected {code}")),
        };
        if let Some(p) = problem {
            failures.push(format!("GOLDEN_RULES.md:{} ({}): {p}", ex.line, ex.tag));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
