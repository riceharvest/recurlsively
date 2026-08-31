use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use crate::config::{
    Config, ConfigError, ProgressMode, QueryMode, RedirectPolicy, ReportFormat, SitemapMode,
};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cli {
    pub start_url: String,
    pub config: Config,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseOutcome {
    Help(String),
    Version(String),
    Run(Cli),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliError(String);

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}
impl std::error::Error for CliError {}
impl From<ConfigError> for CliError {
    fn from(error: ConfigError) -> Self {
        Self(error.to_string())
    }
}

pub fn help_text() -> &'static str {
    "Usage: recurlsively [crawl] [OPTIONS] <START_URL>

A secure-by-default deterministic Markdown snapshotter for AI agents.

Arguments:
  <START_URL>  HTTP(S) URL to snapshot

Options:
  -o, --output <PATH>                 Output directory [default: ./recurlsively-out]
      --max-depth <N>                 Maximum link depth; 0 visits only the start [default: 3]
      --max-pages <N>                 Maximum pages [default: 1000]
      --concurrency <N>               Global concurrency [default: 8]
      --per-host-concurrency <N>      Per-host concurrency [default: 2]
      --delay <DURATION>              Delay between requests, e.g. 250ms [default: 250ms]
      --timeout <DURATION>            Request timeout, e.g. 30s [default: 30s]
      --retries <N>                   Retries after a failed request [default: 2]
      --max-body-size <BYTES>         Per-response limit, e.g. 10MiB [default: 10MiB]
      --max-total-bytes <BYTES>       Crawl budget, e.g. 500MiB [default: 500MiB]
      --query-mode <MODE>             drop or preserve [default: drop]
      --redirect-policy <POLICY>      same-origin [default: same-origin]
      --sitemap <MODE>                auto, on, or off [default: auto]
      --ignore-robots                 Ignore robots.txt (explicit opt-in)
      --include-subdomains            Include subdomains (explicit opt-in)
      --allow-private-network         Allow localhost/LAN targets (unsafe opt-in)
      --fresh                         Do not reuse prior output state
      --report <FORMAT>               text or json [default: text]
      --progress <MODE>               auto, text, json, or none [default: auto]
  -h, --help                          Print help
  -V, --version                       Print version

MVP scope: HTTP(S) only; no JavaScript, browser automation, authentication, or assets."
}

pub fn parse_from<I, S>(args: I) -> Result<ParseOutcome, CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut arguments = args.into_iter().map(Into::into);
    let _program = arguments.next();
    let mut config = Config::default();
    let mut start_url = None;
    let mut command_seen = false;
    let mut positional_only = false;

    while let Some(argument) = arguments.next() {
        if !positional_only && matches!(argument.as_str(), "--help" | "-h") {
            return Ok(ParseOutcome::Help(help_text().to_owned()));
        }
        if !positional_only && matches!(argument.as_str(), "--version" | "-V") {
            return Ok(ParseOutcome::Version(format!("recurlsively {VERSION}")));
        }
        if !positional_only && argument == "--" {
            positional_only = true;
            continue;
        }
        if !positional_only && argument == "crawl" && !command_seen && start_url.is_none() {
            command_seen = true;
            continue;
        }
        if !positional_only && argument.starts_with('-') {
            parse_option(&argument, &mut arguments, &mut config)?;
        } else if start_url.replace(argument).is_some() {
            return Err(CliError("only one START_URL may be provided".to_owned()));
        }
    }

    let start_url = start_url.ok_or_else(|| CliError("missing START_URL".to_owned()))?;
    validate_start_url(&start_url, config.allow_private_network)?;
    config.validate()?;
    Ok(ParseOutcome::Run(Cli { start_url, config }))
}

fn parse_option<I>(argument: &str, arguments: &mut I, config: &mut Config) -> Result<(), CliError>
where
    I: Iterator<Item = String>,
{
    let (name, inline) = argument
        .split_once('=')
        .map_or((argument, None), |(name, value)| (name, Some(value)));
    match name {
        "-o" | "--output" => config.output = PathBuf::from(value(name, inline, arguments)?),
        "--max-depth" => config.max_depth = parse_number(name, value(name, inline, arguments)?)?,
        "--max-pages" => config.max_pages = parse_number(name, value(name, inline, arguments)?)?,
        "--concurrency" => {
            config.concurrency = parse_number(name, value(name, inline, arguments)?)?
        }
        "--per-host-concurrency" => {
            config.per_host_concurrency = parse_number(name, value(name, inline, arguments)?)?;
        }
        "--delay" => config.delay = parse_duration(name, value(name, inline, arguments)?)?,
        "--timeout" => config.timeout = parse_duration(name, value(name, inline, arguments)?)?,
        "--retries" => config.retries = parse_number(name, value(name, inline, arguments)?)?,
        "--max-body-size" => {
            config.max_body_size = parse_bytes(name, value(name, inline, arguments)?)?;
        }
        "--max-total-bytes" => {
            config.max_total_bytes = parse_bytes(name, value(name, inline, arguments)?)?;
        }
        "--query-mode" => config.query_mode = parse_query_mode(value(name, inline, arguments)?)?,
        "--redirect-policy" => {
            config.redirect_policy = parse_redirect_policy(value(name, inline, arguments)?)?;
        }
        "--sitemap" => config.sitemap = parse_sitemap(value(name, inline, arguments)?)?,
        "--report" => config.report = parse_report(value(name, inline, arguments)?)?,
        "--progress" => config.progress = parse_progress(value(name, inline, arguments)?)?,
        "--ignore-robots" => set_boolean(name, inline, &mut config.ignore_robots)?,
        "--include-subdomains" => set_boolean(name, inline, &mut config.include_subdomains)?,
        "--allow-private-network" => set_boolean(name, inline, &mut config.allow_private_network)?,
        "--fresh" => set_boolean(name, inline, &mut config.fresh)?,
        _ => return Err(CliError(format!("unknown option `{argument}`"))),
    }
    Ok(())
}

fn value<I>(name: &str, inline: Option<&str>, arguments: &mut I) -> Result<String, CliError>
where
    I: Iterator<Item = String>,
{
    inline
        .map(str::to_owned)
        .or_else(|| arguments.next())
        .ok_or_else(|| CliError(format!("option `{name}` requires a value")))
}

fn set_boolean(name: &str, inline: Option<&str>, destination: &mut bool) -> Result<(), CliError> {
    if inline.is_some() {
        return Err(CliError(format!("option `{name}` does not take a value")));
    }
    *destination = true;
    Ok(())
}

fn parse_number<T>(name: &str, value: String) -> Result<T, CliError>
where
    T: std::str::FromStr,
{
    value
        .parse()
        .map_err(|_| CliError(format!("invalid value `{value}` for `{name}`")))
}

fn parse_duration(name: &str, value: String) -> Result<Duration, CliError> {
    let (number, multiplier) = if let Some(number) = value.strip_suffix("ms") {
        (number, 1_000_000)
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1_000_000_000)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60 * 1_000_000_000)
    } else {
        return Err(CliError(format!("invalid duration `{value}` for `{name}`")));
    };
    let number: u64 = number
        .parse()
        .map_err(|_| CliError(format!("invalid duration for `{name}`")))?;
    let nanos = number
        .checked_mul(multiplier)
        .ok_or_else(|| CliError(format!("duration is too large for `{name}`")))?;
    Ok(Duration::from_nanos(nanos))
}

fn parse_bytes(name: &str, value: String) -> Result<u64, CliError> {
    let (number, multiplier) = if let Some(number) = value.strip_suffix("MiB") {
        (number, 1024 * 1024)
    } else if let Some(number) = value.strip_suffix("GiB") {
        (number, 1024 * 1024 * 1024)
    } else if let Some(number) = value.strip_suffix("KiB") {
        (number, 1024)
    } else if let Some(number) = value.strip_suffix('B') {
        (number, 1)
    } else {
        return Err(CliError(format!(
            "invalid byte size `{value}` for `{name}`"
        )));
    };
    let number: u64 = number
        .parse()
        .map_err(|_| CliError(format!("invalid byte size for `{name}`")))?;
    number
        .checked_mul(multiplier)
        .ok_or_else(|| CliError(format!("byte size is too large for `{name}`")))
}

fn parse_query_mode(value: String) -> Result<QueryMode, CliError> {
    match value.as_str() {
        "drop" => Ok(QueryMode::Drop),
        "preserve" => Ok(QueryMode::Preserve),
        _ => Err(CliError(format!(
            "invalid query mode `{value}` (expected drop or preserve)"
        ))),
    }
}

fn parse_redirect_policy(value: String) -> Result<RedirectPolicy, CliError> {
    match value.as_str() {
        "same-origin" => Ok(RedirectPolicy::SameOrigin),
        _ => Err(CliError(format!(
            "invalid redirect policy `{value}` (expected same-origin)"
        ))),
    }
}

fn parse_sitemap(value: String) -> Result<SitemapMode, CliError> {
    match value.as_str() {
        "auto" => Ok(SitemapMode::Auto),
        "on" => Ok(SitemapMode::On),
        "off" => Ok(SitemapMode::Off),
        _ => Err(CliError(format!(
            "invalid sitemap mode `{value}` (expected auto, on, or off)"
        ))),
    }
}

fn parse_report(value: String) -> Result<ReportFormat, CliError> {
    match value.as_str() {
        "text" => Ok(ReportFormat::Text),
        "json" => Ok(ReportFormat::Json),
        _ => Err(CliError(format!(
            "invalid report format `{value}` (expected text or json)"
        ))),
    }
}

fn parse_progress(value: String) -> Result<ProgressMode, CliError> {
    match value.as_str() {
        "auto" => Ok(ProgressMode::Auto),
        "text" => Ok(ProgressMode::Text),
        "json" => Ok(ProgressMode::Json),
        "none" => Ok(ProgressMode::None),
        _ => Err(CliError(format!(
            "invalid progress mode `{value}` (expected auto, text, json, or none)"
        ))),
    }
}

fn validate_start_url(url: &str, allow_private_network: bool) -> Result<(), CliError> {
    let Some((scheme, rest)) = url.split_once("://") else {
        return Err(CliError(
            "START_URL must use http:// or https://".to_owned(),
        ));
    };
    if !matches!(scheme, "http" | "https") {
        return Err(CliError(
            "START_URL must use http:// or https://".to_owned(),
        ));
    }
    if rest.is_empty() || rest.starts_with('/') || rest.chars().any(char::is_whitespace) {
        return Err(CliError("START_URL must include a valid host".to_owned()));
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return Err(CliError(
            "START_URL must include a host without userinfo".to_owned(),
        ));
    }
    if !allow_private_network && is_private_host(authority) {
        return Err(CliError(
            "START_URL targets a private network; pass --allow-private-network only for a trusted local target"
                .to_owned(),
        ));
    }
    Ok(())
}

fn is_private_host(authority: &str) -> bool {
    let host = authority
        .strip_prefix('[')
        .and_then(|value| value.split(']').next())
        .unwrap_or_else(|| authority.split(':').next().unwrap_or(authority));
    if matches!(host, "localhost" | "localhost.localdomain" | "::1") {
        return true;
    }
    let Ok(octets) = host
        .split('.')
        .map(str::parse::<u8>)
        .collect::<Result<Vec<_>, _>>()
    else {
        return false;
    };
    matches!(
        octets.as_slice(),
        [10, _, _, _] | [127, _, _, _] | [172, 16..=31, _, _] | [192, 168, _, _]
    )
}
