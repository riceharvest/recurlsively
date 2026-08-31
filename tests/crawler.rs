//! Crawl engine integration tests: fetcher, robots, sitemap, and the run loop.
//!
//! These tests spin a minimal HTTP server on localhost. Private-network
//! blocking is bypassed explicitly with `allow_private_network`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use recurlsively::config::{Config, QueryMode, ReportFormat, SitemapMode};
use recurlsively::crawler;

/// A deterministic single-threaded HTTP fixture server.
struct Fixture {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<String>>>,
}

type Routes = HashMap<String, (u16, Vec<(&'static str, String)>)>;

impl Fixture {
    fn start(routes: Routes) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture");
        let addr = listener.local_addr().expect("addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let request_log = Arc::clone(&requests);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let mut reader = BufReader::new(stream);
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).is_err() {
                    continue;
                }
                let mut header = String::new();
                loop {
                    header.clear();
                    if reader.read_line(&mut header).is_err() || header == "\r\n" {
                        break;
                    }
                }
                let path = request_line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .to_owned();
                request_log.lock().expect("log").push(path.clone());
                let Some((status, headers)) = routes.get(&path) else {
                    let body = "not found\n";
                    let response = format!(
                        "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = reader.get_mut().write_all(response.as_bytes());
                    continue;
                };
                let body = headers
                    .iter()
                    .find(|(name, _)| *name == "body")
                    .map(|(_, value)| value.clone())
                    .unwrap_or_default();
                let mut response = format!("HTTP/1.1 {status} X\r\n");
                for (name, value) in headers {
                    if *name != "body" {
                        response.push_str(&format!("{name}: {value}\r\n"));
                    }
                }
                response.push_str(&format!(
                    "Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                ));
                response.push_str(&body);
                let _ = reader.get_mut().write_all(response.as_bytes());
            }
        });
        Self { addr, requests }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    fn requested(&self, path: &str) -> bool {
        self.requests.lock().expect("log").iter().any(|p| p == path)
    }
}

fn page(title: &str, links: &[&str]) -> String {
    let mut body = format!(
        "<!doctype html><html><head><title>{title}</title></head><body><main><h1>{title}</h1>"
    );
    for link in links {
        body.push_str(&format!(r#"<a href="{link}">next</a>"#));
    }
    body.push_str("<p>content</p></main></body></html>");
    body
}

fn test_config(output: PathBuf, start: &str) -> (Config, String) {
    let config = Config {
        output,
        max_depth: 1,
        max_pages: 50,
        concurrency: 4,
        per_host_concurrency: 2,
        delay: Duration::from_millis(0),
        timeout: Duration::from_secs(10),
        retries: 1,
        max_body_size: 1024 * 1024,
        max_total_bytes: 64 * 1024 * 1024,
        query_mode: QueryMode::Drop,
        sitemap: SitemapMode::Off,
        report: ReportFormat::Json,
        allow_private_network: true,
        ..Config::default()
    };
    (config, start.to_owned())
}

#[tokio::test]
async fn crawl_engine_captures_linked_pages_and_records_dead_links() {
    let mut routes = HashMap::new();
    routes.insert(
        "/".to_owned(),
        (
            200,
            vec![(
                "body",
                page(
                    "home",
                    &["/a", "/missing", "https://external.example.org/x"],
                ),
            )],
        ),
    );
    routes.insert(
        "/a".to_owned(),
        (200, vec![("body", page("alpha", &["/"]))]),
    );
    let fixture = Fixture::start(routes);
    let output = std::env::temp_dir().join(format!(
        "recurlsively-test-{}-{}",
        std::process::id(),
        fixture.addr.port()
    ));
    let (config, start) = test_config(output.clone(), &fixture.url("/"));

    let report = crawler::run(&config, &start).await.expect("crawl succeeds");
    assert_eq!(report.pages_written, 2, "home + /a written");
    assert!(report.pages_failed >= 1, "dead link recorded");
    assert!(output.join("manifest.jsonl").exists());
    assert!(output.join("errors.jsonl").exists());
    assert!(
        output
            .join("pages")
            .join(format!("{}.md", hash_of(&fixture.url("/"))))
            .exists()
    );
    assert!(!fixture.requested("/x"), "external origin never fetched");
    let _ = std::fs::remove_dir_all(&output);
}

#[tokio::test]
async fn robots_disallow_is_respected() {
    let mut routes = HashMap::new();
    routes.insert(
        "/robots.txt".to_owned(),
        (
            200,
            vec![("body", "User-agent: *\nDisallow: /private/\n".to_owned())],
        ),
    );
    routes.insert(
        "/".to_owned(),
        (200, vec![("body", page("home", &["/private/secret"]))]),
    );
    let fixture = Fixture::start(routes);
    let output = std::env::temp_dir().join(format!(
        "recurlsively-robots-{}-{}",
        std::process::id(),
        fixture.addr.port()
    ));
    let (mut config, start) = test_config(output.clone(), &fixture.url("/"));
    config.sitemap = SitemapMode::Off;
    let report = crawler::run(&config, &start).await.expect("crawl succeeds");
    assert_eq!(report.pages_written, 1);
    assert!(report.pages_skipped >= 1, "robots-denied link skipped");
    assert!(!fixture.requested("/private/secret"));
    let _ = std::fs::remove_dir_all(&output);
}

#[tokio::test]
async fn body_limit_is_enforced_as_terminal_error() {
    let mut routes = HashMap::new();
    routes.insert("/".to_owned(), (200, vec![("body", "x".repeat(100_000))]));
    let fixture = Fixture::start(routes);
    let output = std::env::temp_dir().join(format!(
        "recurlsively-body-{}-{}",
        std::process::id(),
        fixture.addr.port()
    ));
    let (mut config, start) = test_config(output, &fixture.url("/"));
    config.max_body_size = 1024;
    let report = crawler::run(&config, &start)
        .await
        .expect("crawl completes");
    assert_eq!(report.pages_written, 0);
    assert_eq!(report.pages_failed, 1);
}

#[tokio::test]
async fn same_origin_redirect_is_followed_and_deduplicated() {
    let mut routes = HashMap::new();
    routes.insert(
        "/start".to_owned(),
        (302, vec![("Location", "/final".to_owned())]),
    );
    routes.insert(
        "/final".to_owned(),
        (200, vec![("body", page("final", &[]))]),
    );
    let fixture = Fixture::start(routes);
    let output = std::env::temp_dir().join(format!(
        "recurlsively-redirect-{}-{}",
        std::process::id(),
        fixture.addr.port()
    ));
    let (config, start) = test_config(output, &fixture.url("/start"));
    let report = crawler::run(&config, &start)
        .await
        .expect("crawl completes");
    assert_eq!(report.pages_written, 1, "only /final written");
}

fn hash_of(url: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(url.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[tokio::test]
async fn resume_of_completed_crawl_is_not_an_error() {
    let mut routes = HashMap::new();
    routes.insert("/".to_owned(), (200, vec![("body", page("solo", &[]))]));
    let fixture = Fixture::start(routes);
    let output = std::env::temp_dir().join(format!(
        "recurlsively-resume-{}-{}",
        std::process::id(),
        fixture.addr.port()
    ));
    let (config, start) = test_config(output.clone(), &fixture.url("/"));

    let first = crawler::run(&config, &start).await.expect("first run");
    assert_eq!(first.pages_written, 1);

    // Second run: everything already written — must not count as failure.
    let second = crawler::run(&config, &start).await.expect("resume run");
    assert_eq!(second.pages_written, 0);
    assert_eq!(second.pages_failed, 0);
    assert_eq!(second.pages_pending, 0, "frontier must be drained");
    let _ = std::fs::remove_dir_all(&output);
}

#[tokio::test]
async fn crawl_writes_index_md_mapping_urls_to_files() {
    let mut routes = HashMap::new();
    routes.insert("/".to_owned(), (200, vec![("body", page("home", &["/a"]))]));
    routes.insert("/a".to_owned(), (200, vec![("body", page("alpha", &[]))]));
    let fixture = Fixture::start(routes);
    let output = std::env::temp_dir().join(format!(
        "recurlsively-index-{}-{}",
        std::process::id(),
        fixture.addr.port()
    ));
    let (config, start) = test_config(output.clone(), &fixture.url("/"));
    let _report = crawler::run(&config, &start).await.expect("crawl succeeds");

    let index = std::fs::read_to_string(output.join("index.md")).expect("index.md exists");
    assert!(
        index.contains(fixture.url("/").as_str()),
        "start url listed"
    );
    assert!(index.contains(".md"), "file paths listed");
    let _ = std::fs::remove_dir_all(&output);
}
