//! The crawl run loop: frontier leasing, fetching, extraction, output.

use serde::Serialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::config::Config;
use crate::extract::{self, ExtractLimits};
use crate::fetch::Fetcher;
use crate::output::{ErrorRecord, ManifestRecord, OutputRoot};
use crate::robots::{RobotsCache, RobotsOutcome};
use crate::state::StateStore;
use crate::url_policy::UrlPolicy;

#[derive(Debug, Default, Serialize)]
pub struct CrawlReport {
    pub pages_written: u64,
    pub pages_failed: u64,
    pub pages_skipped: u64,
    pub pages_pending: u64,
    pub truncated: bool,
}

#[derive(Debug)]
pub enum CrawlError {
    State(crate::state::StateError),
    Output(crate::output::OutputError),
    InvalidStartUrl(String),
}

impl std::fmt::Display for CrawlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::State(e) => write!(f, "state error: {e}"),
            Self::Output(e) => write!(f, "output error: {e}"),
            Self::InvalidStartUrl(u) => write!(f, "invalid start url: {u}"),
        }
    }
}

impl std::error::Error for CrawlError {}

impl From<crate::state::StateError> for CrawlError {
    fn from(e: crate::state::StateError) -> Self {
        Self::State(e)
    }
}

impl From<crate::output::OutputError> for CrawlError {
    fn from(e: crate::output::OutputError) -> Self {
        Self::Output(e)
    }
}

fn extraction_limits(config: &Config) -> ExtractLimits {
    ExtractLimits {
        max_body_bytes: config.max_body_size.min(usize::MAX as u64) as usize,
        max_output_bytes: 2 * 1024 * 1024,
        max_links: 10_000,
    }
}

/// Runs one crawl to completion. Returns the final report.
pub async fn run(config: &Config, start_url: &str) -> Result<CrawlReport, CrawlError> {
    if config.fresh {
        let _ = std::fs::remove_dir_all(&config.output);
    }
    let output = OutputRoot::setup(&config.output)?;
    let state = StateStore::open(output.state_path())?;

    let query_mode = match config.query_mode {
        crate::config::QueryMode::Drop => crate::url_policy::QueryMode::Drop,
        crate::config::QueryMode::Preserve => crate::url_policy::QueryMode::Preserve,
    };
    let policy = UrlPolicy::with_options(
        start_url,
        query_mode,
        config.include_subdomains,
        config.allow_private_network,
    )
    .map_err(|e| CrawlError::InvalidStartUrl(e.to_string()))?;

    let fingerprint = fingerprint_of(start_url, config);
    if let Some(previous) = state.config_fingerprint().map_err(CrawlError::State)? {
        if previous != fingerprint {
            return Err(CrawlError::State(
                crate::state::StateError::ConfigMismatch {
                    expected: fingerprint,
                    actual: previous,
                },
            ));
        }
    } else {
        state
            .set_config_fingerprint(&fingerprint)
            .map_err(CrawlError::State)?;
    }

    let start_admission = state.admit(start_url, 0, None, None)?;
    let _ = start_admission;

    let fetcher = Arc::new(Fetcher::new(
        &format!("recurlsively/{}", env!("CARGO_PKG_VERSION")),
        config.timeout,
        config.allow_private_network,
    ));
    // Sitemap seeding (auto or on): adds in-scope URLs at depth 1.
    if matches!(
        config.sitemap,
        crate::config::SitemapMode::Auto | crate::config::SitemapMode::On
    ) {
        let origin = format!(
            "{}://{}:{}",
            policy.origin().scheme(),
            policy.origin().host(),
            policy.origin().port()
        );
        for candidate in crate::sitemap::discover_sitemaps(&fetcher, &origin).await {
            if let Ok(urls) = crate::sitemap::load_sitemap(&fetcher, &candidate, &origin).await {
                for url in urls {
                    if policy.contains(&policy.canonicalize(&url).map_err(|e| {
                        CrawlError::InvalidStartUrl(format!("sitemap url invalid: {e}"))
                    })?) {
                        let _ = state.admit(&url, 1, None, Some("sitemap"));
                    }
                }
                break;
            }
        }
    }

    let stop = Arc::new(AtomicBool::new(false));
    let sigint_stop = Arc::clone(&stop);
    std::thread::spawn(move || {
        if ctrlc_wait() {
            sigint_stop.store(true, Ordering::SeqCst);
        }
    });

    let written = AtomicU64::new(0);
    let failed = AtomicU64::new(0);
    let skipped = AtomicU64::new(0);

    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let _ = state.recover_expired_leases(crate::robots::unix_ms() as i64);
        let leases = state
            .lease_batch(
                crate::robots::unix_ms() as i64,
                config.concurrency,
                (config.timeout.as_millis() as i64) * 4 + 60_000,
            )
            .map_err(CrawlError::State)?;
        if leases.is_empty() {
            // Frontier is drained only when nothing is queued AND nothing is
            // waiting on a retry timer. Delayed pages must be awaited, not
            // abandoned — otherwise a 429 with Retry-After never gets its
            // retry within the same run.
            let counts = state.counts().map_err(CrawlError::State)?;
            if counts.queued == 0 && counts.delayed > 0 {
                let earliest = state
                    .earliest_next_eligible_at()
                    .map_err(CrawlError::State)?;
                let now = crate::robots::unix_ms() as i64;
                let wait = (earliest.saturating_sub(now)).clamp(0, 5_000) as u64;
                tokio::time::sleep(std::time::Duration::from_millis(wait.max(50))).await;
                continue;
            }
            break; // frontier drained
        }
        let mut handles = Vec::with_capacity(leases.len());
        for lease in leases {
            let fetcher = Arc::clone(&fetcher);
            let stop = Arc::clone(&stop);
            handles.push(tokio::spawn(process_lease(
                config.clone(),
                policy.clone(),
                fetcher,
                stop,
                lease,
            )));
        }
        for handle in handles {
            let outcome = handle.await.unwrap_or(Outcome::Failed);
            match outcome {
                Outcome::Written => {
                    written.fetch_add(1, Ordering::SeqCst);
                }
                Outcome::Skipped => {
                    skipped.fetch_add(1, Ordering::SeqCst);
                }
                Outcome::Failed => {
                    failed.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
        if state.page_count().map_err(CrawlError::State)? as usize >= config.max_pages {
            break;
        }
    }

    // Rebuild the URL -> file index so agents can map pages without parsing JSONL.
    write_page_index(&output, &state).map_err(CrawlError::Output)?;
    write_llms_files(&output).map_err(CrawlError::Output)?;

    let counts = state.counts().map_err(CrawlError::State)?;
    let report = CrawlReport {
        pages_written: written.load(Ordering::SeqCst),
        pages_failed: failed.load(Ordering::SeqCst),
        pages_skipped: skipped.load(Ordering::SeqCst),
        pages_pending: counts.queued + counts.delayed + counts.leased,
        truncated: state.page_count().map_err(CrawlError::State)? as usize >= config.max_pages,
    };
    Ok(report)
}

enum Outcome {
    Written,
    Skipped,
    Failed,
}

async fn process_lease(
    config: Config,
    policy: UrlPolicy,
    fetcher: Arc<Fetcher>,
    stop: Arc<AtomicBool>,
    lease: crate::state::Lease,
) -> Outcome {
    let state = match StateStore::open(lease_output_path(&config).join("state.sqlite")) {
        Ok(s) => s,
        Err(_) => return Outcome::Failed,
    };
    if stop.load(Ordering::SeqCst) {
        return Outcome::Failed;
    }
    let now = crate::robots::unix_ms() as i64;
    let _ = state.record_attempt(lease.page_id, &lease.lease_token, now);

    let Ok(parsed) = reqwest::Url::parse(&lease.canonical_url) else {
        return finish_error(&config, &state, &lease, "invalid_url", "URL unparseable").await;
    };
    let path = parsed.path().to_owned();
    let origin_string = format!(
        "{}://{}:{}",
        policy.origin().scheme(),
        policy.origin().host(),
        policy.origin().port()
    );
    let robots_outcome = RobotsCache::new()
        .check(&fetcher, &origin_string, &path, config.ignore_robots)
        .await;
    match robots_outcome {
        RobotsOutcome::Denied => {
            let _ = state.mark_skipped(lease.page_id, "robots_denied");
            return Outcome::Skipped;
        }
        RobotsOutcome::FailClosed(message) => {
            return finish_error(&config, &state, &lease, "robots_fetch_failed", &message).await;
        }
        RobotsOutcome::NoRules | RobotsOutcome::Allowed(_) => {}
    }

    match fetcher
        .get(&lease.canonical_url, config.max_body_size)
        .await
    {
        Ok(fetched) => {
            let html = match String::from_utf8(fetched.body) {
                Ok(text) => text,
                Err(_) => {
                    return finish_error(
                        &config,
                        &state,
                        &lease,
                        "not_html",
                        "response was not valid UTF-8",
                    )
                    .await;
                }
            };
            let extracted =
                extract::extract_html(&html, &fetched.final_url, extraction_limits(&config));
            let extracted = match extracted {
                Ok(page) => page,
                Err(e) => {
                    return finish_error(&config, &state, &lease, "extract_failed", &e.to_string())
                        .await;
                }
            };
            let document = build_document(&lease.canonical_url, &fetched.final_url, &extracted);
            let relative = crate::url_policy::output_path(&lease.canonical_url);
            let record = ManifestRecord::for_content(
                lease.page_id,
                lease.canonical_url.clone(),
                lease.depth,
                Some(&fetched.final_url),
                Some(fetched.status),
                relative.to_string_lossy().into_owned(),
                extracted.description.clone(),
                document.as_bytes(),
            );
            let output = match OutputRoot::setup(lease_output_path(&config)) {
                Ok(o) => o,
                Err(_) => return Outcome::Failed,
            };
            if output.commit_page(&record, document.as_bytes()).is_err() {
                return Outcome::Failed;
            }
            let now = crate::robots::unix_ms() as i64;
            let _ = state.mark_written(
                lease.page_id,
                &relative.to_string_lossy(),
                &record.digest,
                document.len() as u64,
                now,
            );
            // admit discovered links
            for link in &extracted.links {
                if lease.depth + 1 > config.max_depth {
                    break; // deeper links would all exceed the depth budget
                }
                if let Ok(canonical_link) = policy.canonicalize(link) {
                    if policy.contains(&canonical_link) {
                        let _ = state.admit(
                            canonical_link.as_str(),
                            lease.depth + 1,
                            Some(&lease.canonical_url),
                            Some("link"),
                        );
                    }
                }
            }
            Outcome::Written
        }
        Err(fetch_error) => {
            let retryable = matches!(
                fetch_error,
                crate::fetch::FetchError::Timeout
                    | crate::fetch::FetchError::Network(_)
                    | crate::fetch::FetchError::Status {
                        retryable: true,
                        ..
                    }
            );
            if retryable && lease.attempts < config.retries {
                let now = crate::robots::unix_ms() as i64;
                let backoff = 500u64.saturating_mul(1 << lease.attempts.min(6));
                let _ = state.schedule_retry(
                    lease.page_id,
                    &lease.lease_token,
                    now,
                    now + backoff as i64,
                    &fetch_error.to_string(),
                );
                Outcome::Failed
            } else {
                finish_error(
                    &config,
                    &state,
                    &lease,
                    "fetch_failed",
                    &fetch_error.to_string(),
                )
                .await
            }
        }
    }
}

async fn finish_error(
    config: &Config,
    state: &StateStore,
    lease: &crate::state::Lease,
    kind: &str,
    message: &str,
) -> Outcome {
    let now = crate::robots::unix_ms() as i64;
    let _ = state.mark_terminal_error(lease.page_id, &lease.lease_token, now, message);
    let output = match OutputRoot::setup(lease_output_path(config)) {
        Ok(o) => o,
        Err(_) => return Outcome::Failed,
    };
    let _ = output.append_error(&ErrorRecord {
        page_id: lease.page_id,
        canonical_url: lease.canonical_url.clone(),
        depth: lease.depth,
        attempts: lease.attempts + 1,
        error_kind: kind.to_owned(),
        error: message.to_owned(),
    });
    Outcome::Failed
}

/// Deterministic, version-tolerant config fingerprint without serializing
/// the whole Config (keeps StateStore's Serialize bound simple).
fn fingerprint_of(start_url: &str, config: &Config) -> String {
    use sha2::{Digest, Sha256};
    let mut material = String::with_capacity(256);
    material.push_str("v1|");
    material.push_str(start_url);
    material.push('|');
    material.push_str(&config.output.display().to_string());
    for value in [
        config.max_depth.to_string(),
        config.max_pages.to_string(),
        config.concurrency.to_string(),
        config.per_host_concurrency.to_string(),
        config.retries.to_string(),
        config.max_body_size.to_string(),
        config.max_total_bytes.to_string(),
        format!("{:?}", config.delay),
        format!("{:?}", config.timeout),
        u8::from(matches!(config.query_mode, crate::config::QueryMode::Drop)).to_string(),
        u8::from(config.ignore_robots).to_string(),
        u8::from(config.include_subdomains).to_string(),
        u8::from(config.allow_private_network).to_string(),
        u8::from(config.fresh).to_string(),
        u8::from(config.sitemap == crate::config::SitemapMode::Auto).to_string(),
        u8::from(config.redirect_policy == crate::config::RedirectPolicy::SameOrigin).to_string(),
    ] {
        material.push_str(&value);
        material.push('|');
    }
    let digest = Sha256::digest(material.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Writes `index.md`: one line per written page, `url -> pages/xxx.md`,
/// sorted by URL. Regenerated after every run (including resume no-ops).
/// Writes `llms.txt` (per-page summary index) and `llms-full.txt`
/// (whole corpus concatenated). llms-full is capped at 10k pages.
fn write_llms_files(output: &OutputRoot) -> Result<(), crate::output::OutputError> {
    let mut records = output.read_manifest()?;
    if records.is_empty() {
        return Ok(());
    }
    records.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then(a.canonical_url.cmp(&b.canonical_url))
    });

    const SUMMARY_MAX: usize = 150;
    let truncate = |value: &str| -> String {
        if value.len() <= SUMMARY_MAX {
            value.to_owned()
        } else {
            let cut = &value[..SUMMARY_MAX];
            match cut.rfind(' ') {
                Some(p) if p > SUMMARY_MAX / 2 => format!("{}\u{2026}", &cut[..p]),
                _ => format!("{cut}\u{2026}"),
            }
        }
    };

    // llms.txt
    let mut index = String::with_capacity(records.len() * 160);
    index.push_str("# recurlsively crawl corpus\n\n");
    for record in &records {
        let description = truncate(&record.description);
        if description.is_empty() {
            index.push_str(&format!(
                "- [{}]({})\n",
                record.canonical_url, record.output_path
            ));
        } else {
            index.push_str(&format!(
                "- [{}]({}): {}\n",
                record.canonical_url, record.output_path, description
            ));
        }
    }
    let index_tmp = output.root().join(".llms.txt.tmp");
    std::fs::write(&index_tmp, index.as_bytes())?;
    std::fs::rename(&index_tmp, output.root().join("llms.txt"))?;

    // llms-full.txt (capped)
    if records.len() > 10_000 {
        return Ok(());
    }
    let mut full = String::with_capacity(records.iter().map(|r| r.bytes as usize + 64).sum());
    for record in &records {
        let path = output.page_path(std::path::Path::new(&record.output_path))?;
        match std::fs::read_to_string(&path) {
            Ok(body) => {
                full.push_str(&format!(
                    "# {}\nurl: {}\n\n{}\n\n---\n\n",
                    record.canonical_url, record.canonical_url, body
                ));
            }
            Err(_) => continue, // file vanished mid-run; skip rather than fail the run
        }
    }
    let full_tmp = output.root().join(".llms-full.txt.tmp");
    std::fs::write(&full_tmp, full.as_bytes())?;
    std::fs::rename(&full_tmp, output.root().join("llms-full.txt"))?;
    Ok(())
}

fn write_page_index(
    output: &OutputRoot,
    _state: &StateStore,
) -> Result<(), crate::output::OutputError> {
    let records = output.read_manifest()?;
    if records.is_empty() {
        return Ok(());
    }
    let mut lines: Vec<(String, &str, u32, &str)> = records
        .iter()
        .map(|record| {
            (
                record.canonical_url.clone(),
                record.output_path.as_str(),
                record.depth,
                record.description.as_str(),
            )
        })
        .collect();
    lines.sort();
    let mut document = String::with_capacity(records.len() * 128);
    document.push_str("# Crawl index\n\n");
    document.push_str("One line per page: `[url](file)` (depth) — summary.\n\n");
    for (url, path, depth, description) in lines {
        if description.is_empty() {
            document.push_str(&format!("- [{url}]({path}) ({depth})\n"));
        } else {
            document.push_str(&format!("- [{url}]({path}) ({depth}) — {description}\n"));
        }
    }
    // index.md lives at the corpus root; pages are relative to it.
    let destination = output.root().join("index.md");
    let temp = output.root().join(".index.md.tmp");
    std::fs::write(&temp, document.as_bytes())?;
    std::fs::rename(&temp, &destination)?;
    Ok(())
}

fn lease_output_path(config: &Config) -> std::path::PathBuf {
    config.output.clone()
}

/// Renders the final Markdown file: front matter, body, links appendix.
fn build_document(
    canonical_url: &str,
    final_url: &str,
    extracted: &extract::ExtractedPage,
) -> String {
    let mut document = String::with_capacity(extracted.markdown.len() + 256);
    document.push_str("---\n");
    document.push_str(&format!("url: {canonical_url}\n"));
    document.push_str(&format!("final_url: {final_url}\n"));
    let title = extracted.title.replace('\\', "\\\\").replace('"', "\\\"");
    document.push_str(&format!("title: \"{title}\"\n"));
    document.push_str("---\n\n");
    document.push_str(&extracted.markdown);
    if !document.ends_with('\n') {
        document.push('\n');
    }
    document
}

/// Blocks until SIGINT; returns true when the signal fired.
#[cfg(unix)]
fn ctrlc_wait() -> bool {
    use signal_hook::consts::SIGINT;
    let mut signals = match signal_hook::iterator::Signals::new([SIGINT]) {
        Ok(s) => s,
        Err(_) => return false,
    };
    signals.forever().next().is_some()
}

#[cfg(not(unix))]
fn ctrlc_wait() -> bool {
    false
}
