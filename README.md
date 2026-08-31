# recurlsively

`recurlsively` is a full-Rust, local, zero-runtime, secure-by-default deterministic Markdown snapshotter for AI agents.

## Status

This repository contains the first CLI/configuration slice. The command validates its arguments and configuration, and exposes stable help/version output. The crawl engine, Markdown extraction, durable scheduler, SQLite state, and snapshot output are planned but not implemented yet.

The MVP target is HTTP(S) only: no JavaScript, browser automation, authentication, or asset downloading. The default policy is exact-origin crawling with private and special network targets blocked unless explicitly opted into for a trusted local fixture or local documentation.

## Usage

```text
recurlsively [crawl] <START_URL>
```

Inspect the current interface:

```bash
cargo run -- --help
cargo run -- --version
cargo test
```

A successful invocation currently validates the request and reports that the crawl engine is not implemented. It does not fetch a URL.

## Planned install

> Not released yet. After the first release, the planned one-line install is:
>
> `cargo install recurlsively`

Prebuilt release binaries and checksum-verifying installers are a later release requirement. No binaries are committed to this repository.

## Design commitments

- Deterministic Markdown snapshots with explicit, documented policies.
- Secure defaults: exact origin, bounded work, bounded response/body budgets, and private-network blocking.
- Local execution with no runtime service dependency.
- Durable scheduling and output state backed by SQLite in a later slice.
- Cross-platform support for Linux, macOS, and Windows.
- Dual licensing under MIT OR Apache-2.0.

See [the normative specification](docs/spec.md) and [the architecture](docs/architecture.md). Contributions should preserve those contracts rather than infer behavior from a competitor.

## License

Licensed under either of:

- [MIT](LICENSE-MIT)
- [Apache License, Version 2.0](LICENSE-APACHE)

at your option.
