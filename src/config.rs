use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryMode {
    Drop,
    Preserve,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectPolicy {
    SameOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SitemapMode {
    Auto,
    On,
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressMode {
    Auto,
    Text,
    Json,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub output: PathBuf,
    pub max_depth: u32,
    pub max_pages: usize,
    pub concurrency: usize,
    pub per_host_concurrency: usize,
    pub delay: Duration,
    pub timeout: Duration,
    pub retries: u32,
    pub max_body_size: u64,
    pub max_total_bytes: u64,
    pub query_mode: QueryMode,
    pub redirect_policy: RedirectPolicy,
    pub sitemap: SitemapMode,
    pub ignore_robots: bool,
    pub include_subdomains: bool,
    pub allow_private_network: bool,
    pub fresh: bool,
    pub report: ReportFormat,
    pub progress: ProgressMode,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            output: PathBuf::from("./recurlsively-out"),
            max_depth: 3,
            max_pages: 1_000,
            concurrency: 8,
            per_host_concurrency: 2,
            delay: Duration::from_millis(250),
            timeout: Duration::from_secs(30),
            retries: 2,
            max_body_size: 10 * 1024 * 1024,
            max_total_bytes: 500 * 1024 * 1024,
            query_mode: QueryMode::Drop,
            redirect_policy: RedirectPolicy::SameOrigin,
            sitemap: SitemapMode::Auto,
            ignore_robots: false,
            include_subdomains: false,
            allow_private_network: false,
            fresh: false,
            report: ReportFormat::Text,
            progress: ProgressMode::Auto,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    field: &'static str,
    message: String,
}

impl ConfigError {
    pub(crate) fn new(field: &'static str, message: impl Into<String>) -> Self {
        Self {
            field,
            message: message.into(),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.output.as_os_str().is_empty() {
            return Err(ConfigError::new("output", "path must not be empty"));
        }
        if self.max_pages == 0 {
            return Err(ConfigError::new("max-pages", "must be greater than zero"));
        }
        if self.concurrency == 0 {
            return Err(ConfigError::new("concurrency", "must be greater than zero"));
        }
        if self.per_host_concurrency == 0 {
            return Err(ConfigError::new(
                "per-host-concurrency",
                "must be greater than zero",
            ));
        }
        if self.per_host_concurrency > self.concurrency {
            return Err(ConfigError::new(
                "per-host-concurrency",
                "must not exceed concurrency",
            ));
        }
        if self.timeout.is_zero() {
            return Err(ConfigError::new("timeout", "must be greater than zero"));
        }
        if self.max_body_size == 0 {
            return Err(ConfigError::new(
                "max-body-size",
                "must be greater than zero",
            ));
        }
        if self.max_total_bytes == 0 {
            return Err(ConfigError::new(
                "max-total-bytes",
                "must be greater than zero",
            ));
        }
        if self.max_body_size > self.max_total_bytes {
            return Err(ConfigError::new(
                "max-body-size",
                "must not exceed max-total-bytes",
            ));
        }
        Ok(())
    }
}
