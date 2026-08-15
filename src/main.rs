use std::process::ExitCode;

fn main() -> ExitCode {
    match keep::cli::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("keep: {error}");
            ExitCode::FAILURE
        }
    }
}
