# Contributing

Use English for code comments, documentation, issues, and pull requests. Rust is the implementation language. Keep future agent integrations behind the executable plugin contract rather than adding provider-specific logic to MCP tools.

## Test-driven development

For each behavior change:

1. Write a test that expresses the observable requirement or reproduces the bug.
2. Run the focused test and confirm that it fails for the intended reason.
3. Implement the smallest coherent fix.
4. Run the focused test, then the relevant suite.
5. Refactor while the tests remain green.

Record meaningful red/green evidence in the pull request, especially for regressions. Do not substitute mocks for the subprocess or protocol boundary when that boundary is what changed. Tests must not depend on private credentials, paid providers, global client configuration, or another developer's workspace.

```sh
cargo test --locked
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
```

CI runs these checks on Linux and macOS. Live provider tests require an explicit local invocation; see [testing](docs/testing.md). Never add transcripts containing private project data or account credentials to a pull request.

Explain the problem, resulting behavior, and validation in pull requests. Document plugin or client compatibility changes and any remaining platform limitations. Publishing crates or releases is a separate maintainer action.
