# Contributing

Thanks for helping improve `recurlsively`.

## Development

The project targets stable Rust on Linux, macOS, and Windows. Before opening a change, run:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

## Test-first changes

Use strict RED-GREEN-REFACTOR TDD for production behavior:

1. Write a focused failing test.
2. Run it and confirm the failure is for the intended missing behavior.
3. Implement the smallest change that makes it pass.
4. Run the focused test, then the full checks above.
5. Refactor only while the suite remains green.

Do not add fake implementations or declare planned modules until they have real behavior and tests.

## Pull requests

Keep changes focused, document user-visible contract changes in `docs/spec.md`, and explain security or determinism implications. Do not commit credentials, private paths, generated binaries, or unrelated artifacts. The repository is dual-licensed under MIT OR Apache-2.0; contributions are accepted under the same terms.
