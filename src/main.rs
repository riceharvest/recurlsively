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
        Ok(ParseOutcome::Update) => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            match runtime.block_on(recurlsively::update::run_update()) {
                Ok(message) => {
                    println!("{message}");
                    ExitCode::SUCCESS
                }
                Err(recurlsively::update::UpdateError::UpToDate(message)) => {
                    println!("{message}");
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("recurlsively update: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Ok(ParseOutcome::Search {
            directory,
            query,
            json,
        }) => match recurlsively::search::search(&directory, &query) {
            Ok(hits) => {
                if json {
                    println!(
                        "{}",
                        serde_json::to_string(&hits).unwrap_or_else(|_| "[]".to_owned())
                    );
                } else {
                    print!("{}", recurlsively::search::format_text(&hits, &directory));
                }
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("recurlsively: {error}");
                ExitCode::from(2)
            }
        },
        Ok(ParseOutcome::Run(cli)) => {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            match runtime.block_on(crawler::run(&cli.config, &cli.start_urls)) {
                Ok(reports) => {
                    let sum = |field: fn(&recurlsively::crawler::CrawlReport) -> u64| -> u64 {
                        reports.iter().map(|r| field(&r.report)).sum()
                    };
                    let truncated = reports.iter().any(|r| r.report.truncated);
                    let failed = sum(|r| r.pages_failed);
                    let pending = sum(|r| r.pages_pending);
                    let written = sum(|r| r.pages_written);
                    match cli.config.report {
                        ReportFormat::Json => {
                            let mut entries = Vec::new();
                            for r in &reports {
                                entries.push(serde_json::json!({
                                    "url": r.url,
                                    "pages_written": r.report.pages_written,
                                    "pages_failed": r.report.pages_failed,
                                    "pages_skipped": r.report.pages_skipped,
                                    "pages_pending": r.report.pages_pending,
                                    "changed": r.report.changed,
                                    "unchanged": r.report.unchanged,
                                    "truncated": r.report.truncated,
                                }));
                            }
                            println!(
                                "{}",
                                serde_json::json!({
                                    "urls": entries,
                                    "totals": {
                                        "pages_written": written,
                                        "pages_failed": failed,
                                        "pages_pending": pending,
                                        "truncated": truncated,
                                    }
                                })
                            );
                        }
                        ReportFormat::Text => {
                            for r in &reports {
                                println!(
                                    "{}: written {} failed {} skipped {} pending {} truncated {}",
                                    r.url,
                                    r.report.pages_written,
                                    r.report.pages_failed,
                                    r.report.pages_skipped,
                                    r.report.pages_pending,
                                    r.report.truncated
                                );
                            }
                        }
                    }
                    if truncated || pending > 0 || (written == 0 && failed > 0) {
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
