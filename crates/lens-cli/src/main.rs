use clap::Parser;
use lens_cli::cli::Cli;

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match lens_cli::ops::dispatch(cli.command) {
        // Exit-code convention: 0 = clean, 1 = findings, 2 = error.
        Ok(outcome) => {
            lens_cli::flush_stdout();
            match outcome {
                lens_cli::ops::Outcome::Clean => std::process::ExitCode::SUCCESS,
                lens_cli::ops::Outcome::Findings => std::process::ExitCode::from(1),
            }
        }
        Err(e) => {
            // Flush buffered stdout before the error line so both streams
            // carry their full output.
            lens_cli::flush_stdout();
            // Human-readable Display variant (Debug dumps raw io::Error structs).
            eprintln!("error: {e}");
            std::process::ExitCode::from(2)
        }
    }
}
