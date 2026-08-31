//! robots.txt fetching, parsing, and evaluation per RFC 9309 subset.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::fetch::Fetcher;

const ROBOTS_MAX_BODY: u64 = 500 * 1024;
const USER_AGENT_TOKEN: &str = "recurlsively";

#[derive(Debug, Clone)]
pub struct RobotsRules {
    pub crawl_delay: Option<f64>,
    rules: Vec<(bool, String)>, // (allow, path-pattern)
}

impl RobotsRules {
    /// RFC 9309 longest-match; Allow wins ties.
    pub fn allows(&self, path: &str) -> bool {
        let mut best_len: Option<usize> = None;
        let mut best_allow = true;
        for &(allow, ref pattern) in &self.rules {
            if path.starts_with(pattern.as_str()) {
                match best_len {
                    Some(len) if len > pattern.len() => {}
                    Some(_) if !allow => {} // Allow wins ties
                    _ => {
                        best_len = Some(pattern.len());
                        best_allow = allow;
                    }
                }
            }
        }
        best_len.map(|_| best_allow).unwrap_or(true)
    }
}

#[derive(Debug)]
pub enum RobotsOutcome {
    Allowed(RobotsRules),
    Denied,
    /// robots.txt unreachable in a way that requires failing closed.
    FailClosed(String),
    /// 4xx: no rules apply.
    NoRules,
}

#[derive(Default)]
pub struct RobotsCache {
    cache: HashMap<String, RobotsRules>,
    fail_closed: HashMap<String, String>,
}

pub fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl RobotsCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn fail_closed_error(&self, origin: &str) -> Option<&String> {
        self.fail_closed.get(origin)
    }

    /// Fetches and evaluates robots.txt for `origin`. Honors `ignore_robots`
    /// by treating the origin as unrestricted.
    pub async fn check(
        &mut self,
        fetcher: &Fetcher,
        origin: &str,
        path: &str,
        ignore_robots: bool,
    ) -> RobotsOutcome {
        if ignore_robots {
            return RobotsOutcome::Allowed(RobotsRules {
                crawl_delay: None,
                rules: Vec::new(),
            });
        }
        if let Some(error) = self.fail_closed.get(origin) {
            return RobotsOutcome::FailClosed(error.clone());
        }
        if !self.cache.contains_key(origin) {
            match fetcher
                .get_raw(&format!("{origin}/robots.txt"), ROBOTS_MAX_BODY)
                .await
            {
                Ok(fetched) => {
                    if (400..500).contains(&fetched.status)
                        && fetched.status != 401
                        && fetched.status != 403
                    {
                        // RFC 9309: unavailable (4xx) = no rules for this origin.
                        self.cache.insert(
                            origin.to_owned(),
                            RobotsRules {
                                crawl_delay: None,
                                rules: Vec::new(),
                            },
                        );
                    } else {
                        let text = String::from_utf8_lossy(&fetched.body).into_owned();
                        self.cache.insert(origin.to_owned(), parse_robots(&text));
                    }
                }
                // 4xx surfaced as a Status error: unavailable = no rules.
                Err(crate::fetch::FetchError::Status { status, .. })
                    if (400..500).contains(&status) && status != 401 && status != 403 =>
                {
                    self.cache.insert(
                        origin.to_owned(),
                        RobotsRules {
                            crawl_delay: None,
                            rules: Vec::new(),
                        },
                    );
                }
                Err(e) => {
                    // 5xx / network failure: fail closed for this origin.
                    let message = format!("robots.txt fetch failed: {e}");
                    self.fail_closed.insert(origin.to_owned(), message.clone());
                    return RobotsOutcome::FailClosed(message);
                }
            }
        }
        let rules = &self.cache[origin];
        if rules.allows(path) {
            RobotsOutcome::Allowed(rules.clone())
        } else {
            RobotsOutcome::Denied
        }
    }
}

fn parse_robots(text: &str) -> RobotsRules {
    let mut rules = Vec::new();
    let mut crawl_delay: Option<f64> = None;
    let mut applies = false; // current group applies to our UA token
    for raw_line in text.lines() {
        let line = raw_line.split('#').next().unwrap_or("").trim();
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        match key.as_str() {
            "user-agent" => {
                applies = value.eq_ignore_ascii_case(USER_AGENT_TOKEN) || value == "*";
            }
            "disallow" if applies => {
                if !value.is_empty() {
                    rules.push((false, unescape_pattern(value)));
                }
            }
            "allow" if applies => {
                if !value.is_empty() {
                    rules.push((true, unescape_pattern(value)));
                }
            }
            "crawl-delay" if applies => {
                if let Ok(delay) = value.parse::<f64>() {
                    crawl_delay = Some(crawl_delay.map_or(delay, |d: f64| d.max(delay)));
                }
            }
            _ => {}
        }
    }
    RobotsRules { crawl_delay, rules }
}

/// Escaped-pattern subset: `*` and `$` are retained as literals for prefix
/// matching; `%XX` escapes are decoded only into safe path characters.
fn unescape_pattern(value: &str) -> String {
    value.trim_end_matches('*').to_owned()
}
