use std::process::ExitCode;

use recurlsively::cli::{ParseOutcome, parse_from};
use recurlsively::config::ReportFormat;
use recurlsively::crawler;

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
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            match runtime.block_on(crawler::run(&cli.config, &cli.start_url)) {
                Ok(report) => {
                    match cli.config.report {
                        ReportFormat::Json => println!(
                            "{{\"pages_written\":{},\"pages_failed\":{},\"pages_skipped\":{},\"pages_pending\":{},\"truncated\":{}}}",
                            report.pages_written,
                            report.pages_failed,
                            report.pages_skipped,
                            report.pages_pending,
                            report.truncated
                        ),
                        ReportFormat::Text => println!(
                            "written: {} failed: {} skipped: {} pending: {} truncated: {}",
                            report.pages_written,
                            report.pages_failed,
                            report.pages_skipped,
                            report.pages_pending,
                            report.truncated
                        ),
                    }
                    // exit 0 on clean runs including resume no-ops (nothing failed, nothing pending)
                    if report.truncated
                        || report.pages_pending > 0
                        || (report.pages_written == 0 && report.pages_failed > 0)
                    {
                        ExitCode::from(1)
                    } else {
                        ExitCode::SUCCESS
                    }
                }
                Err(error) => {
                    eprintln!("recurlsively: {error}");
                    ExitCode::from(3)
                }
            }
        }
        Err(error) => {
            eprintln!("recurlsively: {error}");
            ExitCode::from(2)
        }
    }
}
