//! Sitemap discovery: `Sitemap:` directives and /sitemap.xml probing.
//!
//! Caps: 5 MiB per document, 50,000 URLs per document, one sitemapindex level.

use crate::fetch::Fetcher;

const SITEMAP_MAX_BODY: u64 = 5 * 1024 * 1024;
const SITEMAP_MAX_URLS: usize = 50_000;

/// Returns candidate sitemap URLs for an origin (robots directives then probes).
pub async fn discover_sitemaps(fetcher: &Fetcher, origin: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    // robots.txt Sitemap: lines were parsed into the robots fetch; probe here.
    let robots = fetcher
        .get(&format!("{origin}/robots.txt"), 500 * 1024)
        .await;
    if let Ok(fetched) = robots {
        let text = String::from_utf8_lossy(&fetched.body);
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if let Some((key, value)) = line.split_once(':') {
                if key.trim().eq_ignore_ascii_case("sitemap") {
                    candidates.push(value.trim().to_owned());
                }
            }
        }
    }
    candidates.push(format!("{origin}/sitemap.xml"));
    candidates
}

/// Fetches one sitemap document and returns the page URLs it lists.
/// For a `<sitemapindex>`, the child sitemaps (one level deep) are resolved.
pub async fn load_sitemap(
    fetcher: &Fetcher,
    sitemap_url: &str,
    origin: &str,
) -> Result<Vec<String>, String> {
    let fetched = fetcher
        .get(sitemap_url, SITEMAP_MAX_BODY)
        .await
        .map_err(|e| format!("sitemap fetch failed: {e}"))?;
    let text = String::from_utf8_lossy(&fetched.body).into_owned();
    let urls = extract_loc_tags(&text);
    if urls.is_empty() && text.contains("<sitemapindex") {
        return Err("sitemapindex without child locations".to_owned());
    }
    if text.contains("<sitemapindex") {
        // one level of sitemapindex: fetch each child, bounded
        let mut out = Vec::new();
        for child in urls.iter().take(32) {
            if !same_origin_url(child, origin) {
                continue;
            }
            if let Ok(child_fetched) = fetcher.get(child, SITEMAP_MAX_BODY).await {
                let child_text = String::from_utf8_lossy(&child_fetched.body);
                for url in extract_loc_tags(&child_text) {
                    if out.len() >= SITEMAP_MAX_URLS {
                        break;
                    }
                    if same_origin_url(&url, origin) {
                        out.push(url);
                    }
                }
            }
        }
        return Ok(out);
    }
    Ok(urls
        .into_iter()
        .filter(|u| same_origin_url(u, origin))
        .take(SITEMAP_MAX_URLS)
        .collect())
}

/// True when `candidate` is an http(s) URL on exactly `origin`
/// (scheme + host + effective port), not merely a string prefix.
fn same_origin_url(candidate: &str, origin: &str) -> bool {
    let Ok(parsed) = url::Url::parse(candidate) else {
        return false;
    };
    let Ok(expected) = url::Url::parse(origin) else {
        return false;
    };
    parsed.origin() == expected.origin()
}

/// Extracts `<loc>` values without a full XML dependency stack.
fn extract_loc_tags(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("<loc>") {
        let after = &rest[start + 5..];
        if let Some(end) = after.find("</loc>") {
            let value = after[..end].trim();
            if !value.is_empty() {
                out.push(value.to_owned());
            }
            rest = &after[end + 6..];
        } else {
            break;
        }
    }
    out
}
