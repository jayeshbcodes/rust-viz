use rust_viz::interp::Interpreter;
use rust_viz::{trace, tui};

const USAGE: &str = "usage:
  rust-viz                 live REPL: type statements, watch memory update
  rust-viz <file.rs>       run a program, then step through it (n/p)
  rust-viz --trace <file>  print the step-by-step trace as text";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.as_slice() {
        [] => tui::run_live(Interpreter::new()),
        [flag, path] if flag == "--trace" => {
            let (interp, result) = Interpreter::run_program(&read(path));
            print!("{}", trace::render(&interp.steps));
            if let Err(e) = result {
                eprintln!("{e}");
                std::process::exit(1);
            }
            Ok(())
        }
        [path] if !path.starts_with('-') => {
            let source = read(path);
            let (interp, result) = Interpreter::run_program(&source);
            tui::run(&interp.steps, &source, path, result.err().map(|e| e.to_string()))
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("tui error: {e}");
        std::process::exit(1);
    }
}

fn read(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("can't read {path}: {e}");
        std::process::exit(1);
    })
}
