use std::{env, process::ExitCode};

fn main() -> ExitCode {
    match shepherd_core::run_cli(env::args().skip(1)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("shepherd: {error}");
            ExitCode::FAILURE
        }
    }
}
