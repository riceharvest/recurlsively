//! `recurlsively search <output-dir> <query>` — deterministic scoring search
//! over a crawl corpus. Title matches weigh 10x, headings 4x, body 1x.

use std::io::BufRead;
use std::path::Path;

use serde::Serialize;

const TITLE_WEIGHT: u64 = 10;
const HEADING_WEIGHT: u64 = 4;
const BODY_WEIGHT: u64 = 1;
const MAX_RESULTS: usize = 20;

#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub url: String,
    pub path: String,
    pub title: String,
    pub score: u64,
    /// Matching lines as `pages/xxx.md:12: text`.
    pub matches: Vec<String>,
    /// Indices of the queries (0-based) this hit matched.
    pub queries: Vec<usize>,
}

#[derive(Debug)]
pub enum SearchError {
    NoCorpus(String),
    Io(std::io::Error),
}

impl std::fmt::Display for SearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCorpus(directory) => write!(
                f,
                "no manifest.jsonl in {directory} — is this a recurlsively output directory?"
            ),
            Self::Io(e) => write!(f, "search failed: {e}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// One combined ranking; hits tagged with matching query indices.
    Combined,
    /// Separate result list per query.
    Separate,
    /// Only pages matching every query.
    All,
}

/// Mode-aware search. Separate returns per-query lists joined by markers;
/// all filters to pages matching every query.
pub fn search_mode(
    directory: &Path,
    query: &str,
    mode: SearchMode,
) -> Result<Vec<SearchHit>, SearchError> {
    let queries: Vec<&str> = query.split(',').collect();
    if queries.len() > 1 || mode != SearchMode::Combined {
        // mark multi-query paths
    }
    let mut hits = search(directory, query)?;
    if mode == SearchMode::All && queries.len() > 1 {
        hits.retain(|hit| hit.queries.len() == queries.len());
    }
    Ok(hits)
}

/// Formats separate-mode output: one block per query.
pub fn format_separate(directory: &Path, query: &str) -> Result<String, SearchError> {
    let mut out = String::new();
    for (index, single) in query.split(',').enumerate() {
        out.push_str(&format!("== query {}: {} ==\n", index + 1, single.trim()));
        let hits = search(directory, single)?;
        if hits.is_empty() {
            out.push_str("no matches\n");
        }
        for hit in hits {
            out.push_str(&format!(
                "{}  {}  {}\n",
                hit.score,
                directory.join(&hit.path).display(),
                hit.title
            ));
            for line in &hit.matches {
                out.push_str(&format!("    {line}\n"));
            }
        }
        out.push('\n');
    }
    Ok(out)
}

/// Runs the search; returns hits sorted by score descending, then path.
pub fn search(directory: &Path, query: &str) -> Result<Vec<SearchHit>, SearchError> {
    let manifest_path = directory.join("manifest.jsonl");
    let manifest = std::fs::File::open(&manifest_path)
        .map_err(|_| SearchError::NoCorpus(directory.display().to_string()))?;
    // comma-separated alternative: "a, b" == two queries
    let queries: Vec<Vec<String>> = query
        .split(',')
        .map(|q| {
            q.to_lowercase()
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|terms| !terms.is_empty())
        .collect();
    if queries.is_empty() {
        return Ok(Vec::new());
    }

    let reader = std::io::BufReader::new(manifest);
    let mut hits = Vec::new();
    for line in reader.lines() {
        let line = line.map_err(SearchError::Io)?;
        let Ok(record) = serde_json::from_str::<crate::output::ManifestRecord>(&line) else {
            continue;
        };
        let page_path = directory.join(&record.output_path);
        let Ok(body) = std::fs::read_to_string(&page_path) else {
            continue;
        };
        if let Some(hit) = score_page_multi(&record, &body, &queries) {
            hits.push(hit);
        }
    }
    hits.sort_by(|a, b| b.score.cmp(&a.score).then(a.path.cmp(&b.path)));
    hits.truncate(MAX_RESULTS);
    Ok(hits)
}

/// Scores one page against every query; a hit matches any query.
fn score_page_multi(
    record: &crate::output::ManifestRecord,
    body: &str,
    queries: &[Vec<String>],
) -> Option<SearchHit> {
    let mut total_score = 0u64;
    let mut matched_queries = Vec::new();
    let mut all_matches: Vec<String> = Vec::new();
    for (index, terms) in queries.iter().enumerate() {
        if let Some(mut hit) = score_page(record, body, terms) {
            total_score += hit.score;
            matched_queries.push(index);
            all_matches.append(&mut hit.matches);
        }
    }
    if matched_queries.is_empty() {
        return None;
    }
    Some(SearchHit {
        url: record.canonical_url.clone(),
        path: record.output_path.clone(),
        title: extract_front_matter_title(body).unwrap_or_else(|| record.canonical_url.clone()),
        score: total_score,
        matches: all_matches,
        queries: matched_queries,
    })
}

fn score_page(
    record: &crate::output::ManifestRecord,
    body: &str,
    terms: &[String],
) -> Option<SearchHit> {
    let lower = body.to_lowercase();
    if !terms.iter().all(|term| lower.contains(term)) {
        return None; // all-terms-required
    }
    let title = extract_front_matter_title(body).unwrap_or_else(|| record.canonical_url.clone());
    let title_lower = title.to_lowercase();

    let mut score = 0u64;
    let mut matches = Vec::new();
    for (line_number, line) in body.lines().enumerate() {
        let trimmed = line.trim();
        let is_front_matter = line_number < 4 || trimmed.starts_with("title:");
        let lower_line = trimmed.to_lowercase();
        let matched: Vec<&String> = terms
            .iter()
            .filter(|term| lower_line.contains(term.as_str()))
            .collect();
        if matched.is_empty() || is_front_matter {
            continue;
        }
        let weight = if trimmed.starts_with('#') {
            HEADING_WEIGHT
        } else {
            BODY_WEIGHT
        };
        score += weight * matched.len() as u64;
        if matches.len() < 5 {
            matches.push(format!(
                "{}:{}: {}",
                record.output_path,
                line_number + 1,
                trimmed.chars().take(120).collect::<String>()
            ));
        }
    }
    if title_lower.contains(&terms.join(" ")) {
        score += TITLE_WEIGHT * terms.len() as u64;
    } else {
        for term in terms {
            if title_lower.contains(term.as_str()) {
                score += TITLE_WEIGHT;
            }
        }
    }
    if matches.is_empty() {
        return None;
    }
    Some(SearchHit {
        url: record.canonical_url.clone(),
        path: record.output_path.clone(),
        title,
        score,
        matches,
        queries: vec![0], // single-query path; multi handled by score_page_multi
    })
}

fn extract_front_matter_title(body: &str) -> Option<String> {
    let mut lines = body.lines();
    if lines.next()? != "---" {
        return None;
    }
    for line in lines.by_ref() {
        if line == "---" {
            break;
        }
        if let Some(value) = line.strip_prefix("title:") {
            return Some(value.trim().trim_matches('"').to_owned());
        }
    }
    None
}

/// Formats hits as human-readable text for stdout.
pub fn format_text(hits: &[SearchHit], directory: &Path) -> String {
    if hits.is_empty() {
        return "no matches\n".to_owned();
    }
    let mut out = String::new();
    for hit in hits {
        let tag = if hit.queries.len() > 1 || hit.queries != [0] {
            format!(
                " [q{}]",
                hit.queries
                    .iter()
                    .map(|i| (i + 1).to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            )
        } else {
            String::new()
        };
        out.push_str(&format!(
            "{}{}  {}  {}\n",
            hit.score,
            tag,
            directory.join(&hit.path).display(),
            hit.title
        ));
        for line in &hit.matches {
            out.push_str(&format!("    {line}\n"));
        }
    }
    out
}
