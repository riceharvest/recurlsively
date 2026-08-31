use std::path::PathBuf;
use std::time::Duration;

use recurlsively::cli::{ParseOutcome, parse_from};
use recurlsively::config::{ProgressMode, QueryMode, ReportFormat};

#[test]
fn help_is_a_successful_parse_outcome() {
    let outcome = parse_from(["recurlsively", "--help"]).expect("help should parse");
    assert!(matches!(outcome, ParseOutcome::Help(text) if text.contains("Usage:")));
}

#[test]
fn version_is_a_successful_parse_outcome() {
    let outcome = parse_from(["recurlsively", "--version"]).expect("version should parse");
    assert!(matches!(outcome, ParseOutcome::Version(text) if text.contains("recurlsively 0.1.0")));
}

#[test]
fn crawl_subcommand_and_defaults_are_parsed() {
    let outcome = parse_from(["recurlsively", "crawl", "https://example.com/docs"])
        .expect("crawl should parse");
    let ParseOutcome::Run(cli) = outcome else {
        panic!("expected a runnable CLI");
    };

    assert_eq!(cli.start_url, "https://example.com/docs");
    assert_eq!(cli.config.output, PathBuf::from("./recurlsively-out"));
    assert_eq!(cli.config.max_depth, 3);
    assert_eq!(cli.config.max_pages, 1_000);
    assert_eq!(cli.config.concurrency, 8);
    assert_eq!(cli.config.per_host_concurrency, 2);
    assert_eq!(cli.config.delay, Duration::from_millis(250));
    assert_eq!(cli.config.timeout, Duration::from_secs(30));
    assert_eq!(cli.config.retries, 2);
    assert_eq!(cli.config.query_mode, QueryMode::Drop);
    assert_eq!(cli.config.report, ReportFormat::Text);
    assert_eq!(cli.config.progress, ProgressMode::Auto);
}

#[test]
fn crawl_subcommand_is_optional() {
    let outcome =
        parse_from(["recurlsively", "https://example.com"]).expect("implicit crawl should parse");
    assert!(matches!(outcome, ParseOutcome::Run(cli) if cli.start_url == "https://example.com"));
}

#[test]
fn explicit_values_and_boolean_flags_are_parsed() {
    let outcome = parse_from([
        "recurlsively",
        "--output",
        "snapshots",
        "--max-depth",
        "0",
        "--max-pages",
        "12",
        "--concurrency",
        "4",
        "--per-host-concurrency",
        "1",
        "--delay",
        "1s",
        "--timeout",
        "2m",
        "--retries",
        "0",
        "--max-body-size",
        "1MiB",
        "--max-total-bytes",
        "5MiB",
        "--query-mode",
        "preserve",
        "--sitemap",
        "off",
        "--ignore-robots",
        "--include-subdomains",
        "--allow-private-network",
        "--fresh",
        "--report",
        "json",
        "--progress",
        "none",
        "https://example.com",
    ])
    .expect("explicit options should parse");
    let ParseOutcome::Run(cli) = outcome else {
        panic!("expected a runnable CLI");
    };

    assert_eq!(cli.config.output, PathBuf::from("snapshots"));
    assert_eq!(cli.config.max_depth, 0);
    assert_eq!(cli.config.max_pages, 12);
    assert_eq!(cli.config.concurrency, 4);
    assert_eq!(cli.config.per_host_concurrency, 1);
    assert_eq!(cli.config.delay, Duration::from_secs(1));
    assert_eq!(cli.config.timeout, Duration::from_secs(120));
    assert_eq!(cli.config.retries, 0);
    assert_eq!(cli.config.max_body_size, 1_048_576);
    assert_eq!(cli.config.max_total_bytes, 5_242_880);
    assert_eq!(cli.config.query_mode, QueryMode::Preserve);
    assert!(cli.config.ignore_robots);
    assert!(cli.config.include_subdomains);
    assert!(cli.config.allow_private_network);
    assert!(cli.config.fresh);
    assert_eq!(cli.config.report, ReportFormat::Json);
    assert_eq!(cli.config.progress, ProgressMode::None);
}

#[test]
fn invalid_url_is_rejected() {
    let error = parse_from(["recurlsively", "ftp://example.com"]).expect_err("URL must be HTTP(S)");
    assert!(error.to_string().contains("http:// or https://"));
}

#[test]
fn invalid_numeric_and_duration_values_are_rejected() {
    for args in [
        vec!["recurlsively", "--max-pages", "0", "https://example.com"],
        vec!["recurlsively", "--concurrency", "0", "https://example.com"],
        vec!["recurlsively", "--delay", "soon", "https://example.com"],
        vec![
            "recurlsively",
            "--max-body-size",
            "0MiB",
            "https://example.com",
        ],
    ] {
        assert!(
            parse_from(args.clone()).is_err(),
            "expected rejection for {args:?}"
        );
    }
}

#[test]
fn unknown_option_and_missing_url_are_rejected() {
    assert!(parse_from(["recurlsively", "--not-a-real-option", "https://example.com"]).is_err());
    assert!(parse_from(["recurlsively", "crawl"]).is_err());
}

#[test]
fn config_validation_rejects_body_limit_larger_than_total_limit() {
    let config = recurlsively::config::Config {
        max_body_size: 2,
        max_total_bytes: 1,
        ..Default::default()
    };
    let error = config.validate().expect_err("limits must be ordered");
    assert!(error.to_string().contains("max-body-size"));
}
