# Development

## Workspace

The root `Cargo.toml` defines a virtual workspace with shared package metadata, dependencies, and lints:

- `crates/source-mesh`: CLI argument parsing, help, and process exit handling.
- `crates/source-mesh-core`: analysis, graph processing, exports, and validation.

See the [core development guide](crates/source-mesh-core/DEVELOPMENT.md).

## Development tools

The script installs the additional tools for testing, coverage, and agent workflows in one run.

```powershell
./scripts/Install-DevTools.ps1
```

The installer leaves existing commands unchanged.

## Tests and checks

Run commands from the repository root:

```console
cargo test --release --locked --workspace
cargo clippy --release --all-targets --locked --workspace -- -D warnings
```

The standard test command skips tests marked `#[ignore]`.
To run all tests, including rust-analyzer and repository integration tests:

```powershell
./scripts/Test-All.ps1
```

### Documentation link checks

Checks documentation for broken links.

```powershell
./scripts/Test-Links.ps1
```

Use `-Offline` to check only local links.

### Coverage

```powershell
./scripts/Test-Coverage.ps1
```

Runs the workspace library and binary tests.
Integration tests, ignored tests, and doctests are not included. Open `target/coverage/html/index.html`;
use `-OutputDirectory` to change the output directory.

### Mutation testing

Runs the non-ignored library tests against generated mutations of `source-mesh-core` in isolated copies.

```powershell
./scripts/Test-Mutants.ps1 -List
./scripts/Test-Mutants.ps1 -File '**/graph/*.rs'
./scripts/Test-Mutants.ps1 -File '**/graph/*.rs' -MutantPattern 'DependencyCount::increment'
```

`-List` lists mutants without building or testing. `-File` uses workspace-relative globs; `-MutantPattern` uses a regex.
Results: `work/mutants/mutants.out`.

## Test layout and CI

Unit and component tests live under each crate's `src`. See the
[fixture guide](crates/source-mesh-core/src/fixtures/README.md) for analyzer-backed tests and fixture regeneration.
Tests under `crates/source-mesh/tests` check workspace boundaries and CLI behavior against
[expected outputs](crates/source-mesh/tests/expected/README.md).

[CI](.github/workflows/ci.yml) runs release builds, tests, and Clippy on Linux, macOS, and Windows,
and verifies workspace packaging.
