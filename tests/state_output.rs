use std::fs;
use std::path::Path;

use recurlsively::output::{ErrorRecord, ManifestRecord, OutputError, OutputRoot};
use recurlsively::state::{PageState, StateStore};
use tempfile::tempdir;

fn manifest(page_id: i64, path: &str, body: &[u8]) -> ManifestRecord {
    ManifestRecord::for_content(
        page_id,
        "https://example.test/page",
        0,
        Some("https://example.test/page"),
        Some(200),
        path,
        "",
        body,
    )
}

#[test]
fn state_admission_deduplicates_and_keeps_lowest_depth() {
    let directory = tempdir().unwrap();
    let state = StateStore::open(directory.path().join("state.sqlite")).unwrap();

    let first = state
        .admit_url("https://example.test/page", 4, None, Some("seed"))
        .unwrap();
    let second = state
        .admit_url(
            "https://example.test/page",
            2,
            Some("https://example.test/parent"),
            Some("link"),
        )
        .unwrap();
    let third = state
        .admit_url("https://example.test/page", 7, None, None)
        .unwrap();

    assert_eq!(first.page_id, second.page_id);
    assert_eq!(second.page_id, third.page_id);
    assert!(first.inserted);
    assert!(!second.inserted);
    assert_eq!(state.page_count().unwrap(), 1);
    let page = state.page(first.page_id).unwrap().unwrap();
    assert_eq!(page.depth, 2);
    assert_eq!(
        page.parent_url.as_deref(),
        Some("https://example.test/parent")
    );
    assert_eq!(page.state, PageState::Queued);
}

#[test]
fn state_leases_in_depth_and_canonical_order_and_requeues_after_expiry() {
    let directory = tempdir().unwrap();
    let state = StateStore::open(directory.path().join("state.sqlite")).unwrap();
    state
        .admit_url("https://example.test/z", 1, None, None)
        .unwrap();
    state
        .admit_url("https://example.test/a", 1, None, None)
        .unwrap();
    state
        .admit_url("https://example.test/root", 0, None, None)
        .unwrap();

    let leases = state.lease_batch(100, 2, 50).unwrap();
    assert_eq!(
        leases
            .iter()
            .map(|lease| lease.canonical_url.as_str())
            .collect::<Vec<_>>(),
        ["https://example.test/root", "https://example.test/a"]
    );
    assert!(leases.iter().all(|lease| !lease.lease_token.is_empty()));
    assert_eq!(state.counts().unwrap().leased, 2);

    assert_eq!(state.recover_expired_leases(150).unwrap(), 2);
    assert_eq!(state.counts().unwrap().queued, 3);
    let again = state.lease_batch(151, 1, 50).unwrap();
    assert_eq!(again[0].canonical_url, "https://example.test/root");
}

#[test]
fn state_retry_and_terminal_transitions_require_current_lease() {
    let directory = tempdir().unwrap();
    let state = StateStore::open(directory.path().join("state.sqlite")).unwrap();
    let page_id = state
        .admit_url("https://example.test/retry", 0, None, None)
        .unwrap()
        .page_id;
    let lease = state.lease_batch(10, 1, 100).unwrap().remove(0);
    state
        .record_attempt(page_id, &lease.lease_token, 11)
        .unwrap();
    state
        .schedule_retry(
            page_id,
            &lease.lease_token,
            12,
            1000,
            "temporary network failure",
        )
        .unwrap();
    let page = state.page(page_id).unwrap().unwrap();
    assert_eq!(page.state, PageState::Delayed);
    assert_eq!(page.attempts, 1);
    assert_eq!(page.next_eligible_at, 1000);

    let lease = state.lease_batch(1000, 1, 100).unwrap().remove(0);
    state
        .mark_terminal_error(page_id, &lease.lease_token, 1001, "permanent")
        .unwrap();
    assert_eq!(
        state.page(page_id).unwrap().unwrap().state,
        PageState::TerminalError
    );
    assert!(
        state
            .schedule_retry(page_id, &lease.lease_token, 1002, 2000, "wrong")
            .is_err()
    );
}

#[test]
fn state_config_fingerprint_survives_reopen_and_detects_mismatch() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let state = StateStore::open(&path).unwrap();
    assert!(state.ensure_config_fingerprint("abc").unwrap().is_none());
    drop(state);
    let state = StateStore::open(&path).unwrap();
    assert_eq!(state.config_fingerprint().unwrap().as_deref(), Some("abc"));
    let mismatch = state.ensure_config_fingerprint("def").unwrap_err();
    let message = mismatch.to_string();
    assert!(
        message.contains("different configuration") && message.contains("--fresh"),
        "mismatch error should explain the fix, got: {message}"
    );
}

#[test]
fn state_schema_is_versioned_and_counts_are_durable() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let state = StateStore::open(&path).unwrap();
    assert_eq!(
        state.schema_version().unwrap(),
        recurlsively::state::SCHEMA_VERSION
    );
    let page_id = state
        .admit_url("https://example.test/skip", 0, None, None)
        .unwrap()
        .page_id;
    state.mark_skipped(page_id, "robots").unwrap();
    assert_eq!(state.counts().unwrap().skipped, 1);
    drop(state);
    let state = StateStore::open(path).unwrap();
    assert_eq!(state.counts().unwrap().skipped, 1);
}

#[test]
fn output_root_rejects_symlink_and_path_escape() {
    let directory = tempdir().unwrap();
    let real = directory.path().join("real");
    let root = OutputRoot::setup(&real).unwrap();
    let body = b"# Hello\n";
    let good = manifest(1, "pages/1.md", body);
    root.commit_page(&good, body).unwrap();
    let escaped = manifest(2, "../outside.md", body);
    assert!(matches!(
        root.commit_page(&escaped, body),
        Err(OutputError::PathEscape(_))
    ));

    // Symlink rejection is exercised on Unix; on Windows the same
    // guarantee is enforced by the setup path check without symlink
    // creation support in std.
    #[cfg(unix)]
    {
        let link = directory.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(matches!(
            OutputRoot::setup(&link),
            Err(OutputError::SymlinkRoot(_))
        ));
    }
}

#[test]
fn output_jsonl_is_parseable_and_recovers_truncated_tail() {
    let directory = tempdir().unwrap();
    let root = OutputRoot::setup(directory.path().join("out")).unwrap();
    let body = b"page";
    let record = manifest(1, "pages/1.md", body);
    root.append_manifest(&record).unwrap();
    root.append_error(&ErrorRecord {
        page_id: 2,
        canonical_url: "https://example.test/bad".into(),
        depth: 1,
        attempts: 2,
        error_kind: "http".into(),
        error: "bad gateway".into(),
    })
    .unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(root.errors_path())
        .unwrap()
        .write_all(b"{\"page_id\":")
        .unwrap();
    root.recover_jsonl().unwrap();
    assert_eq!(root.read_manifest().unwrap(), vec![record]);
    assert_eq!(root.read_errors().unwrap().len(), 1);
    let errors = fs::read_to_string(root.errors_path()).unwrap();
    assert!(errors.ends_with('\n'));
    for line in errors.lines() {
        serde_json::from_str::<ErrorRecord>(line).unwrap();
    }
}

#[test]
fn output_page_digest_is_verified_and_duplicate_commit_is_idempotent() {
    let directory = tempdir().unwrap();
    let root = OutputRoot::setup(directory.path().join("out")).unwrap();
    let body = b"stable bytes";
    let record = manifest(42, "pages/42.md", body);
    let first = root.commit_page(&record, body).unwrap();
    assert!(!first.duplicate);
    let second = root.commit_page(&record, body).unwrap();
    assert!(second.duplicate);
    assert_eq!(root.read_manifest().unwrap().len(), 1);
    assert_eq!(
        fs::read(root.page_path(Path::new("pages/42.md")).unwrap()).unwrap(),
        body
    );

    let mut wrong = record.clone();
    wrong.digest = "00".repeat(32);
    assert!(matches!(
        root.commit_page(&wrong, body),
        Err(OutputError::DigestMismatch { .. })
    ));
}

use std::io::Write;
