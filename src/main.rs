use std::process::ExitCode;

use recurlsively::cli::{ParseOutcome, help_text, parse_from};

fn main() -> ExitCode {
    match parse_from(std::env::args()) {
        Ok(ParseOutcome::Help(text)) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Ok(ParseOutcome::Version(text)) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Ok(ParseOutcome::Run(cli)) => {
            println!(
                "validated {} (crawl engine not implemented in this scaffold)",
                cli.start_url
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            eprintln!();
            eprintln!("{}", help_text());
            ExitCode::from(2)
        }
    }
}
