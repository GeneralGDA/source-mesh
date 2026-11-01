# Development

## Workspace

The root `Cargo.toml` defines a virtual workspace with shared package metadata, dependencies, and lints:

- `crates/source-mesh`: CLI argument parsing, help, and process exit handling.
- `crates/source-mesh-core`: analysis, graph processing, exports, and validation.

See the [core development guide](crates/source-mesh-core/DEVELOPMENT.md) for exporter internals and fixture maintenance.

## Tests and checks

Run commands from the repository root using the toolchain and components in [`rust-toolchain.toml`](rust-toolchain.toml).

```console
cargo test --release --locked --workspace
cargo clippy --release --all-targets --locked --workspace -- -D warnings
```

For the full test run, including ignored rust-analyzer and repository integration tests, use PowerShell:

```powershell
./scripts/Test-All.ps1
```

### Local coverage

```powershell
./scripts/Test-Coverage.ps1
```

The script installs or updates the required coverage tools and runs the ordinary workspace library and binary tests.
Integration tests, ignored tests, and doctests are not included. Open `target/coverage/html/index.html`;
use `-OutputDirectory` to change the output directory.

### Local mutation testing

Install the optional development tools (including `cargo-mutants`) once, then run targeted checks:

```powershell
./scripts/Install-DevTools.ps1
./scripts/Test-Mutants.ps1 -List
./scripts/Test-Mutants.ps1 -File '**/graph/*.rs'
./scripts/Test-Mutants.ps1 -File '**/graph/*.rs' -MutantPattern 'DependencyCount::increment'
```

`-List` lists mutants without building or testing. `-File` matches workspace-relative paths; `-MutantPattern` filters by regex.
Without filters, all of `source-mesh-core` is examined. Each mutant runs only the ordinary core library tests.

Runs use isolated copies and write results to `work/mutants/mutants.out`. Defaults are one worker, a 120-second test timeout,
and a 300-second build timeout; override them with `-Jobs`, `-TestTimeoutSeconds`, and `-BuildTimeoutSeconds`.

The tools installer leaves existing commands unchanged. To update the mutation runner, use `cargo install --locked cargo-mutants`.

## Local CLI

Run or install the CLI from the working tree:

```console
cargo run -p source-mesh -- --help
cargo install --path crates/source-mesh --locked
```

## Test layout and CI

Unit and component tests live under each crate's `src`. See the
[fixture guide](crates/source-mesh-core/src/fixtures/README.md) for analyzer-backed tests and fixture regeneration.
Tests under `crates/source-mesh/tests` check workspace boundaries and CLI behavior against
[expected outputs](crates/source-mesh/tests/expected/README.md).

[CI](.github/workflows/ci.yml) runs release builds, tests, and Clippy on Linux, macOS, and Windows,
and verifies workspace packaging.
