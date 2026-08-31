# recurlsively

`recurlsively` is a full-Rust, local, zero-runtime, secure-by-default deterministic Markdown snapshotter for AI agents.

One command recursively crawls an entire domain and writes one grep-friendly
Markdown file per page, so an agent can run it once and then scan the corpus
for relevant information — no browser, no runtime, no API keys.

## Install

macOS / Linux:

```sh
curl -fsSL https://raw.githubusercontent.com/riceharvest/recurlsively/main/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://raw.githubusercontent.com/riceharvest/recurlsively/main/install.ps1 | iex
```

Already installed? Self-update from the latest release (v0.1.2+; first run after v0.1.2):

```sh
recurlsively --update
```

See [docs/install.md](docs/install.md) for manual and per-OS instructions.

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
| `--timeout` | `30s` | per-request timeout |
| `--retries` | `2` | retries on 408/425/429/5xx (honors `Retry-After`) |
| `--max-body-size` | `10MiB` | hard per-page body limit |
| `--max-total-bytes` | `500MiB` | crawl-wide download budget |
| `--query-mode` | `drop` | drop query strings (dedupes tracking URLs) |
| `--sitemap` | `auto` | seed the frontier from robots.txt/sitemap.xml |
| `--include-subdomains` | off | allow `sub.<start-host>` |
| `--ignore-robots` | off | bypass robots.txt (your responsibility) |
| `--allow-private-network` | off | allow localhost/private IPs (trusted targets only) |
| `--fresh` | off | wipe prior output state and start over |
| `--include <glob>` | off | only crawl paths matching the glob (repeatable) |
| `--exclude <glob>` | off | skip paths matching the glob (repeatable) |
| `--report` | `text` | final summary: `text` or `json` on stdout |
| `--progress` | `auto` | stderr progress: `auto`, `text`, `json`, `none` |
| `--update` | — | self-update from GitHub Releases (checksum-verified) |

## Output layout

```
recurlsively-out/
├── index.md           # sorted map: [url](pages/xxx.md) (depth) — start here
├── llms.txt           # per-page summary index in llms.txt format
├── llms-full.txt      # whole corpus concatenated (up to 10k pages)
├── graph.jsonl        # link graph: {url, inbound, outbound} per page
├── pages/<sha256>.md  # one Markdown file per page, YAML front matter
├── manifest.jsonl     # one JSON line per written page (url, depth, digest, path)
├── errors.jsonl       # one JSON line per terminal failure with a reason
└── state.sqlite       # durable frontier; re-running the same command resumes
```

### Multiple start URLs

```bash
# one corpus per site (default)
recurlsively crawl https://docs.a.com https://docs.b.com -o ./out
#   -> out/docs.a.com/..., out/docs.b.com/...

# one merged corpus, deduped across starts
recurlsively crawl https://docs.a.com https://docs.b.com -o ./out --merge
```

All flags apply to every start URL.

### Relevance crawl

```bash
# only save pages matching the query; irrelevant pages are traversed but not saved
recurlsively crawl https://docs.example.com -o out --for "rate limits" --max-pages 100
# strict: prune irrelevant pages AND their links
recurlsively crawl https://docs.example.com -o out --for "rate limits" --for-prune
```

### Search the corpus

```bash
recurlsively search ./docs-snap "rate limits"
recurlsively search ./docs-snap "rate limits" --json
# multiple queries: combined ranking tagged [q1,q2], or per-query lists
recurlsively search ./docs-snap "rate limits, authentication"
recurlsively search ./docs-snap "rate limits, authentication" --mode separate
recurlsively search ./docs-snap "rate limits, authentication" --mode all
```

Title matches rank 10x, headings 4x, body 1x. All query terms must appear.
Hit lines include `file:line` so follow-up greps are one copy-paste away.

### Re-crawls are cheap

Pages store `etag`/`last_modified` validators. Re-running a crawl re-downloads
only pages whose content changed — unchanged pages are confirmed with zero
body transfer, and the JSON report shows `changed`/`unchanged` counts.

Every Markdown file carries front matter with `url`, `final_url`, and
`title`, and ends with exactly one newline. Treat the content as untrusted
input: it came from the network.

Exit codes: `0` complete (including a resume no-op), `1` partial/truncated,
`2` usage error, `3` fatal startup failure.

## Security model

- Exact-origin crawling by default; subdomains and cross-origin redirects
  are opt-in or rejected.
- Private, loopback, link-local, and special-use IP ranges are refused at
  DNS resolution time (anti-rebinding) unless `--allow-private-network` is
  passed explicitly.
- Redirects are followed manually, hop by hop, with the same-origin policy
  re-checked on every hop (max 10); HTTPS→HTTP downgrades rejected.
- Bodies are streamed with hard size limits; retries honor `Retry-After`.
- robots.txt is fetched and honored by default; 5xx/network failure fails
  closed. `Sitemap:` directives seed the frontier when `--sitemap auto`.
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
