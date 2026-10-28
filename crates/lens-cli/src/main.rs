use clap::{CommandFactory, FromArgMatches};
use lens_cli::cli::Cli;
use std::io::{IsTerminal, Write};

fn main() -> std::process::ExitCode {
    let matches = Cli::command().get_matches();
    let mut cli = Cli::from_arg_matches(&matches).expect("clap validated arguments");
    let resolved = match cli.resolve_config(&matches, std::io::stdout().is_terminal()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return std::process::ExitCode::from(2);
        }
    };
    if let lens_cli::cli::Commands::Config { action } = &cli.command {
        match action {
            lens_cli::cli::ConfigCommands::Explain => {
                lens_cli::output::line(format_args!("configuration files: {:?}\ndiscovery boundary: {}\nformat: {:?}\nfail_on: {:?}\nquiet: {}", resolved.files, resolved.boundary.display(), cli.format, cli.fail_on, cli.quiet));
                lens_cli::output::line(format_args!("{}", resolved.details));
            }
            lens_cli::cli::ConfigCommands::Init { path } => {
                let result = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                    .and_then(|mut f| f.write_all(b"schema_version = 1\n"));
                if let Err(e) = result {
                    eprintln!("error: {}: {e}", path.display());
                    return std::process::ExitCode::from(2);
                }
                lens_cli::output::line(format_args!("Created {}", path.display()));
            }
        }
        lens_cli::flush_stdout();
        return std::process::ExitCode::SUCCESS;
    }
    lens_cli::output::set_quiet(cli.quiet);
    let opts = cli.global_opts();
    match lens_cli::ops::dispatch(cli.command, &opts) {
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
