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

### Local mutation testing

Install the optional tools once with `./Install-DevTools.ps1`, then run from PowerShell:

```powershell
./Test-Mutants.ps1 -List
./Test-Mutants.ps1 -File '**/graph/*.rs'
./Test-Mutants.ps1 -File '**/graph/*.rs' -MutantPattern 'DependencyCount::increment'
```

`-List` shows generated mutants without compiling or running tests; it can be combined with both filters.
File globs are matched against workspace-relative paths. Without filters, the script examines all of `source-mesh-core`,
which can take a long time. Each mutant runs the core library tests; CLI and ignored rust-analyzer tests are not run.
The script resolves the workspace from its own location and returns cargo-mutants' exit code unchanged.

cargo-mutants uses its default isolated-copy mode, leaving working sources untouched. The script defaults to one worker,
a 120-second test timeout, and a 300-second build timeout; override these with `-Jobs`, `-TestTimeoutSeconds`, and
`-BuildTimeoutSeconds`. Results are in Git-ignored `work/mutants/mutants.out`; cargo-mutants rotates the previous run to
`mutants.out.old`, replacing any older archive. No mutation job is added to CI.

cargo-mutants is a Cargo subcommand, not a rustup component, so `rust-toolchain.toml` cannot install it.
The tools installer requests the latest release without pinning its version; `--locked` uses that release's own dependency
lockfile. Existing commands are left untouched. A future release may require a newer Rust version; the installer reports
that failure rather than changing the project's toolchain. To update cargo-mutants explicitly, run:

```console
cargo install --locked cargo-mutants
```

### Agent-driven mutation testing

In a new Zed Agent conversation, invoke `/mutation-testing` by selecting the skill from the `/` menu.
The [project skill](.agents/skills/mutation-testing/SKILL.md) derives fresh mutations from the current computational core,
checks whether tests detect them, and adds verified regressions. It includes rules for isolation, test discovery,
and interrupted-run recovery without maintaining a fixed module map or mutation bank.
This is an agent workflow, not a standalone shell command or a new CI dependency.

## Local CLI

Both crates are default workspace members. Run the CLI from the repository root with `cargo run -p source-mesh -- ...`,
or install it locally with `cargo install --path crates/source-mesh --locked`.
Installation from crates.io remains `cargo install source-mesh --locked`.

## Optional development tools

With PowerShell (`pwsh`) and Cargo on `PATH`, install the optional development tools on Linux, macOS, or Windows:

```console
pwsh -File ./Install-DevTools.ps1
```

The script installs missing `rg`, `fd`, `jaq`, `ast-grep`, and `cargo-mutants` through Cargo without replacing commands
already on `PATH`. Installation downloads packages and compiles them locally; the first run can take several minutes.

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
