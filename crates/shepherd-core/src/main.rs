use std::{env, process::ExitCode};

fn main() -> ExitCode {
    let is_json = env::args().any(|a| a == "--json");
    match shepherd_core::run_cli(env::args().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if !is_json {
                eprintln!("shepherd: {error}");
            }
            ExitCode::FAILURE
        }
    }
}
