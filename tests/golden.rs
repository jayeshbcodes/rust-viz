//! Golden/oracle tests: run each sample under `samples/` two ways — compiled
//! and executed for real by `rustc`, and interpreted by `rust_viz` — and
//! assert the observable output (`println!`/`print!`) matches. This is the
//! only thing comparable between the two: move/borrow/drop events aren't
//! something a real compiled binary prints, so this only validates the
//! parts of interpreter behavior that are externally observable.

use std::fs;
use std::process::Command;

use rust_viz::interp::Interpreter;

/// Samples are either whole programs (with `fn main`) or a bare statement
/// block (see the note at the top of `samples/strings.rs`), which gets
/// wrapped in `fn main() { ... }` before handing to `rustc`.
fn run_with_rustc(body: &str, test_name: &str) -> String {
    // Unique per test, not just per process: `cargo test` runs tests in
    // parallel *within one process*, so `process::id()` alone collides
    // between concurrently-running tests sharing this temp directory.
    let dir = std::env::temp_dir().join(format!("rust-viz-golden-{}-{}", std::process::id(), test_name));
    fs::create_dir_all(&dir).expect("create temp dir");
    let src_path = dir.join("sample.rs");
    let bin_path = dir.join("sample_bin");
    let program = if rust_viz::interp::defines_main(body) { body.to_string() } else { format!("fn main() {{\n{body}\n}}") };
    fs::write(&src_path, program).expect("write sample source");

    let compile = Command::new("rustc")
        .arg(&src_path)
        .args(["--edition", "2024", "-A", "warnings"])
        .arg("-o")
        .arg(&bin_path)
        .output()
        .expect("invoke rustc");
    assert!(
        compile.status.success(),
        "rustc failed to compile sample:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&bin_path).output().expect("run compiled sample");
    assert!(
        run.status.success(),
        "compiled sample exited non-zero:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );

    let _ = fs::remove_dir_all(&dir);
    String::from_utf8(run.stdout).expect("sample stdout was not valid utf8")
}

fn run_with_interp(body: &str) -> String {
    let (interp, result) = Interpreter::run_program(body);
    result.unwrap_or_else(|e| panic!("interpreter failed on sample: {e}"));
    // Output can't reveal a wrong memory model, but a leak can: once the
    // program has finished, every allocation must have been freed and the
    // stack fully unwound — same as a real Rust program exiting `main`.
    let last = interp.steps.last().expect("sample produced no steps");
    assert_eq!(last.heap.live_count(), 0, "heap allocations still live at program end");
    assert!(last.stack.frames.is_empty(), "stack not unwound at program end");
    interp.output
}

fn assert_matches_rustc(sample_path: &str, test_name: &str) {
    let body = fs::read_to_string(sample_path).expect("read sample file");
    let expected = run_with_rustc(&body, test_name);
    let actual = run_with_interp(&body);
    assert_eq!(actual, expected, "interpreter output diverged from rustc for {sample_path}");
}

#[test]
fn strings_sample_matches_rustc() {
    assert_matches_rustc("samples/strings.rs", "strings");
}

#[test]
fn vec_basic_sample_matches_rustc() {
    assert_matches_rustc("samples/vec_basic.rs", "vec_basic");
}

#[test]
fn refs_sample_matches_rustc() {
    assert_matches_rustc("samples/refs.rs", "refs");
}

#[test]
fn array_basic_sample_matches_rustc() {
    assert_matches_rustc("samples/array_basic.rs", "array_basic");
}

#[test]
fn ownership_sample_matches_rustc() {
    assert_matches_rustc("samples/ownership.rs", "ownership");
}

#[test]
fn borrowing_sample_matches_rustc() {
    assert_matches_rustc("samples/borrowing.rs", "borrowing");
}

#[test]
fn slices_sample_matches_rustc() {
    assert_matches_rustc("samples/slices.rs", "slices");
}

#[test]
fn structs_sample_matches_rustc() {
    assert_matches_rustc("samples/structs.rs", "structs");
}

#[test]
fn control_flow_sample_matches_rustc() {
    assert_matches_rustc("samples/control_flow.rs", "control_flow");
}

#[test]
fn enums_match_sample_matches_rustc() {
    assert_matches_rustc("samples/enums_match.rs", "enums_match");
}

#[test]
fn capacity_sample_matches_rustc() {
    assert_matches_rustc("samples/capacity.rs", "capacity");
}

#[test]
fn boxes_sample_matches_rustc() {
    assert_matches_rustc("samples/boxes.rs", "boxes");
}

#[test]
fn display_sample_matches_rustc() {
    assert_matches_rustc("samples/display.rs", "display");
}
