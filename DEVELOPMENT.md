# Development

## Workspace

The repository root is a virtual workspace with two sibling crates:

- `source-mesh` in `crates/source-mesh` owns argument parsing, help, and process exit handling.
  It installs the `source-mesh` executable.
- [`source-mesh-core`](crates/source-mesh-core/README.md) owns analysis, graph processing, exports, output validation,
  and cycle checks. Its typed API can be used without the CLI or `clap`.

The root `Cargo.toml` contains only workspace configuration, shared package metadata, dependencies, and lints;
it does not define a package.

See the [core development guide](crates/source-mesh-core/DEVELOPMENT.md) for exporter internals and fixture maintenance.

## Tests and checks

Run from the repository root:

```console
cargo test --release --locked --workspace
cargo clippy --release --all-targets --locked --workspace -- -D warnings
```

The test command above skips slow tests that run rust-analyzer. To run all workspace tests in release mode,
including ignored tests and documentation tests, use PowerShell from the repository root:

```powershell
./Test-All.ps1
```

The full run requires Cargo, rust-analyzer, and rust-src and can take several minutes. The script runs tests sequentially
to avoid competing analyzer processes, continues with other test targets after a test failure, and returns Cargo's exit code.
It resolves the workspace from the script location, so it can also be invoked by path from another directory.

## Local CLI

Both crates are default workspace members. Run the CLI from the repository root with `cargo run -p source-mesh -- ...`,
or install it locally with `cargo install --path crates/source-mesh --locked`.
Installation from crates.io remains `cargo install source-mesh --locked`.

## Optional agent tools

With PowerShell (`pwsh`) and Cargo on `PATH`, install the optional agent tools on Linux, macOS, or Windows:

```console
pwsh -File ./Install-AgentTools.ps1
```

The script installs missing `rg`, `fd`, `jaq`, and `ast-grep` through Cargo without replacing commands already on `PATH`.

## Test layout and CI

Unit and component tests live under each crate's `src`; normal runs use synthetic data and a bundled SCIP fixture
without launching rust-analyzer.
[Component tests that run rust-analyzer](crates/source-mesh-core/src/fixtures/README.md) index fixture projects during execution
and require rust-src. Integration tests under `crates/source-mesh/tests` enforce the virtual workspace boundary and run the
compiled application on the whole repository to check
[expected exports and dependency cycles](crates/source-mesh/tests/expected/README.md).

Repository integration tests and their expected outputs remain in the checkout and are not included in the published CLI archive.

CI checks release builds, Clippy, unit and component tests, documentation tests, and repository integration tests
on Linux, macOS, and Windows for every push and pull request.
