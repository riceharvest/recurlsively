# recurlsively

`recurlsively` is a full-Rust, local, zero-runtime, secure-by-default deterministic Markdown snapshotter for AI agents.

One command recursively crawls an entire domain and writes one grep-friendly
Markdown file per page, so an agent can run it once and then scan the corpus
for relevant information — no browser, no runtime, no API keys.

## Install

> Available from the first tagged release (v0.1.0). See
> [docs/install.md](docs/install.md) for manual and per-OS instructions.

macOS / Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/riceharvest/recurlsively/main/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/riceharvest/recurlsively/main/install.ps1 | iex
```

## Usage

```text
recurlsively [crawl] <START_URL> [OPTIONS]
```

Crawl a domain and scan the output:

```bash
recurlsively https://docs.example.com -o ./docs-snap
grep -ril "rate limits" docs-snap/pages/
```

Key options (see `--help` for all):

| Flag | Default | Meaning |
|---|---|---|
| `-o, --output` | `./recurlsively-out` | output directory |
| `--max-depth` | `3` | link depth from the start URL |
| `--max-pages` | `1000` | hard page budget |
| `--concurrency` | `8` | parallel fetches |
| `--delay` | `250ms` | per-host politeness delay |
| `--max-body-size` | `10MiB` | hard per-page body limit |
| `--query-mode` | `drop` | drop query strings (dedupes tracking URLs) |
| `--sitemap` | `auto` | seed the frontier from sitemap.xml |
| `--include-subdomains` | off | allow `sub.<start-host>` |
| `--ignore-robots` | off | bypass robots.txt (your responsibility) |
| `--allow-private-network` | off | allow localhost/private IPs (trusted targets only) |

## Output layout

```
recurlsively-out/
├── pages/<sha256>.md   # one Markdown file per page, YAML front matter
├── manifest.jsonl      # one JSON line per written page (url, depth, digest, path)
├── errors.jsonl        # one JSON line per terminal failure with a reason
└── state.sqlite        # durable frontier; re-running the same command resumes
```

Every Markdown file carries front matter with `url`, `final_url`, and
`title`, and ends with exactly one newline. Treat the content as untrusted
input: it came from the network.

## Security model

- Exact-origin crawling by default; subdomains and cross-origin redirects
  are opt-in or rejected.
- Private, loopback, link-local, and special-use IP ranges are refused
  unless `--allow-private-network` is passed explicitly.
- Redirects are followed manually, hop by hop, with the same-origin policy
  re-checked on every hop (max 10).
- Bodies are streamed with hard size limits; retries honor `Retry-After`.
- robots.txt is fetched and honored by default; 5xx/network failure fails
  closed.
- No JavaScript rendering, cookies, or authentication. Static HTML/XHTML
  only.

## Design commitments

- Deterministic Markdown snapshots with explicit, documented policies.
- Secure defaults: exact origin, bounded work, bounded budgets, private-network blocking.
- Local execution with no runtime service dependency.
- Durable scheduling and output state backed by SQLite; interrupted crawls resume.
- Cross-platform support for Linux, macOS, and Windows.
- Dual licensing under MIT OR Apache-2.0.

See [the normative specification](docs/spec.md), [the architecture](docs/architecture.md),
and [install docs](docs/install.md).

## License

Licensed under either of:

- [MIT](LICENSE-MIT)
- [Apache License, Version 2.0](LICENSE-APACHE)

at your option.
