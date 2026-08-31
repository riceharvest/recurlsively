use std::net::IpAddr;
use std::path::Path;

use recurlsively::url_policy::{
    CanonicalUrl, IpSafety, QueryMode, UrlPolicy, UrlPolicyError, canonicalize_url, classify_ip,
    is_safe_ip, output_path, page_id,
};

fn assert_invalid(input: &str) {
    assert!(
        canonicalize_url(input, QueryMode::Drop).is_err(),
        "{input} should be invalid"
    );
}

#[test]
fn canonicalization_normalizes_host_port_dots_fragment_and_slash() {
    let canonical = canonicalize_url(
        "HTTP://EXAMPLE.COM:80/a/../docs/./#section",
        QueryMode::Drop,
    )
    .unwrap();
    assert_eq!(canonical.as_str(), "http://example.com/docs/");

    let root = canonicalize_url("http://example.com", QueryMode::Drop).unwrap();
    assert_eq!(root.as_str(), "http://example.com/");
    let no_slash = canonicalize_url("http://example.com/docs", QueryMode::Drop).unwrap();
    assert_eq!(no_slash.as_str(), "http://example.com/docs");
    let slash = canonicalize_url("http://example.com/docs/", QueryMode::Drop).unwrap();
    assert_eq!(slash.as_str(), "http://example.com/docs/");
}

#[test]
fn canonicalization_uses_idna_and_preserves_or_drops_query_without_normalizing_it() {
    let idna = canonicalize_url("https://BÜCHER.example/", QueryMode::Drop).unwrap();
    assert_eq!(idna.as_str(), "https://xn--bcher-kva.example/");

    let with_query =
        canonicalize_url("https://example.com/p?b=2&a=%2f", QueryMode::Preserve).unwrap();
    assert_eq!(with_query.as_str(), "https://example.com/p?b=2&a=%2f");
    let without_query =
        canonicalize_url("https://example.com/p?b=2&a=%2f", QueryMode::Drop).unwrap();
    assert_eq!(without_query.as_str(), "https://example.com/p");
}

#[test]
fn canonicalization_is_idempotent() {
    let first = canonicalize_url(
        "https://BÜCHER.example:443/a/./b/../c/?z=3#fragment",
        QueryMode::Preserve,
    )
    .unwrap();
    let second = canonicalize_url(first.as_str(), QueryMode::Preserve).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.page_id(), second.page_id());
}

#[test]
fn canonicalization_rejects_unsafe_schemes_userinfo_and_malformed_hosts() {
    assert_invalid("ftp://example.com/file");
    assert_invalid("file:///tmp/file");
    assert_invalid("https://user:password@example.com/");
    assert_invalid("https://user@example.com/");
    assert_invalid("https://@example.com/");
    assert_invalid("https:///missing-host");
    assert_invalid("https://example.com/a b");
}

#[test]
fn origin_scope_is_exact_by_default_and_has_label_boundary_subdomains() {
    let exact = UrlPolicy::new("https://EXAMPLE.com:443/start").unwrap();
    assert!(exact.is_in_scope("https://example.com/other").unwrap());
    assert!(!exact.is_in_scope("https://sub.example.com/other").unwrap());
    assert!(
        !exact
            .is_in_scope("https://example.com.evil.test/other")
            .unwrap()
    );
    assert!(!exact.is_in_scope("http://example.com/other").unwrap());
    assert!(!exact.is_in_scope("https://example.com:444/other").unwrap());
    assert!(!exact.is_in_scope("https://EXAMPLE.NET/other").unwrap());

    let subdomains =
        UrlPolicy::with_options("https://example.com/start", QueryMode::Drop, true, false).unwrap();
    assert!(
        subdomains
            .is_in_scope("https://sub.example.com/other")
            .unwrap()
    );
    assert!(
        subdomains
            .is_in_scope("https://deep.sub.example.com/other")
            .unwrap()
    );
    assert!(
        !subdomains
            .is_in_scope("https://example.com.evil.test/other")
            .unwrap()
    );
    assert!(
        !subdomains
            .is_in_scope("http://sub.example.com/other")
            .unwrap()
    );
    assert!(
        !subdomains
            .is_in_scope("https://sub.example.com:444/other")
            .unwrap()
    );
}

#[test]
fn special_ipv4_ranges_are_unsafe_by_default() {
    let blocked = [
        "0.0.0.0",
        "0.1.2.3",
        "10.1.2.3",
        "172.16.0.1",
        "172.31.255.254",
        "100.64.0.1",
        "100.127.255.254",
        "127.0.0.1",
        "169.254.1.1",
        "192.0.0.1",
        "192.0.2.1",
        "192.88.99.1",
        "192.168.1.1",
        "198.18.0.1",
        "198.51.100.1",
        "203.0.113.1",
        "224.0.0.1",
        "239.255.255.255",
        "240.0.0.1",
        "255.255.255.255",
    ];
    for host in blocked {
        let ip: IpAddr = host.parse().unwrap();
        assert!(!is_safe_ip(ip), "{host} was classified as safe");
        assert!(matches!(classify_ip(ip), IpSafety::Special(_)));
        assert_invalid(&format!("http://{host}/"));
    }
    assert!(is_safe_ip("8.8.8.8".parse().unwrap()));
}

#[test]
fn alternate_numeric_ipv4_spellings_cannot_bypass_ip_safety() {
    for host in ["127.1", "0x7f000001", "0177.0.0.1", "2130706433"] {
        assert_invalid(&format!("http://{host}/"));
    }
}

#[test]
fn special_ipv6_ranges_and_mapped_ipv4_are_unsafe_by_default() {
    let blocked = [
        "::",
        "::1",
        "::ffff:192.168.1.1",
        "::ffff:127.0.0.1",
        "fc00::1",
        "fd12:3456::1",
        "fe80::1",
        "ff02::1",
        "100::1",
        "2001:2::1",
        "2001:db8::1",
        "2001::1",
        "3fff::1",
        "64:ff9b::192.0.2.1",
    ];
    for host in blocked {
        let ip: IpAddr = host.parse().unwrap();
        assert!(!is_safe_ip(ip), "{host} was classified as safe");
        assert!(matches!(classify_ip(ip), IpSafety::Special(_)));
        assert_invalid(&format!("http://[{host}]/"));
    }
    assert!(is_safe_ip("::ffff:8.8.8.8".parse().unwrap()));
    assert!(is_safe_ip("2001:4860:4860::8888".parse().unwrap()));
}

#[test]
fn unsafe_allow_private_bypasses_literal_special_address_rejection() {
    let strict = UrlPolicy::new("http://127.0.0.1:8080/").unwrap_err();
    assert!(matches!(strict, UrlPolicyError::UnsafeAddress { .. }));

    let unsafe_policy =
        UrlPolicy::with_options("http://127.0.0.1:8080/", QueryMode::Drop, false, true).unwrap();
    assert!(
        unsafe_policy
            .canonicalize("http://127.0.0.1:8080/health")
            .is_ok()
    );
    assert!(
        unsafe_policy
            .is_in_scope("http://127.0.0.1:8080/health")
            .unwrap()
    );
}

#[test]
fn encoded_slashes_and_path_traversal_are_rejected_without_over_normalizing_safe_paths() {
    for input in [
        "http://example.com/a%2fb",
        "http://example.com/a%2Fb",
        "http://example.com/a%5Cb",
        "http://example.com/%2e%2e/secret",
        "http://example.com/%2E%2E/secret",
        "http://example.com/a/%2e./secret",
        "http://example.com/a\\b",
    ] {
        assert_invalid(input);
    }
    let normalized = canonicalize_url("http://example.com/a/../safe", QueryMode::Drop).unwrap();
    assert_eq!(normalized.as_str(), "http://example.com/safe");
}

#[test]
fn output_paths_are_portable_and_bounded_for_reserved_names_and_long_segments() {
    let reserved = canonicalize_url("http://example.com/CON", QueryMode::Drop).unwrap();
    let path = reserved.output_path();
    assert_eq!(path.extension().and_then(|ext| ext.to_str()), Some("md"));
    assert_eq!(path.components().count(), 2);
    assert!(!path.is_absolute());
    assert!(!path.to_string_lossy().contains(".."));
    assert!(
        !["CON", "PRN", "AUX", "NUL", "COM1", "LPT1"]
            .iter()
            .any(|name| path.file_stem().and_then(|stem| stem.to_str()) == Some(*name))
    );

    let long_segment = "x".repeat(32_768);
    let long_url = format!("http://example.com/{long_segment}");
    let long = canonicalize_url(&long_url, QueryMode::Drop).unwrap();
    let long_path = long.output_path();
    assert!(long_path.to_string_lossy().len() < 100);
    assert!(long_path.starts_with(Path::new("pages")));
}

#[test]
fn page_ids_and_output_paths_are_deterministic_and_query_aware() {
    let first = canonicalize_url("http://example.com/page?a=1", QueryMode::Preserve).unwrap();
    let same = canonicalize_url("http://example.com/page?a=1", QueryMode::Preserve).unwrap();
    let different = canonicalize_url("http://example.com/page?a=2", QueryMode::Preserve).unwrap();
    let dropped = canonicalize_url("http://example.com/page?a=1", QueryMode::Drop).unwrap();

    assert_eq!(page_id(first.as_str()), first.page_id());
    assert_eq!(output_path(first.as_str()), first.output_path());
    assert_eq!(first.page_id(), same.page_id());
    assert_eq!(first.output_path(), same.output_path());
    assert_ne!(first.page_id(), different.page_id());
    assert_ne!(first.output_path(), different.output_path());
    assert_ne!(first.page_id(), dropped.page_id());
    assert_eq!(page_id(first.as_str()).len(), 64);
    assert!(
        page_id(first.as_str())
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    );
}

#[test]
fn canonical_url_exposes_stable_string_and_origin_for_later_modules() {
    let canonical: CanonicalUrl =
        canonicalize_url("https://Example.com:443/a", QueryMode::Drop).unwrap();
    assert_eq!(canonical.to_string(), "https://example.com/a");
    assert_eq!(canonical.origin().scheme(), "https");
    assert_eq!(canonical.origin().host(), "example.com");
    assert_eq!(canonical.origin().port(), 443);
}
