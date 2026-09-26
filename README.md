# source-mesh

![CI status](https://img.shields.io/github/actions/workflow/status/GeneralGDA/rust-arch/ci.yml?label=CI&logo=githubactions&logoColor=white)
![Linux](https://img.shields.io/badge/Linux-ubuntu--latest-FCC624?logo=linux&logoColor=black)
![macOS](https://img.shields.io/badge/macOS-macos--latest-000000?logo=apple)
![Windows](https://img.shields.io/badge/Windows-windows--latest-0078D4?logo=windows&logoColor=white)

Draw dependencies between physical folders in a Rust project. Export **GraphML for yEd**, **Mermaid**, or **DOT**, highlight cycles, and report cycles between folders or files.

## Install

Install the rust-analyzer component, then install the latest published release:

```console
rustup component add rust-analyzer
cargo install source-mesh --locked
```

Before the first crates.io release, install directly from GitHub:

```console
cargo install --git https://github.com/GeneralGDA/rust-arch --locked source-mesh
```

Cargo installs an optimized release build. Keep Cargo and rust-analyzer on `PATH`; install the rust-analyzer component for the toolchain selected by the project you analyze.

## Run

From the Rust project you want to analyze:

```console
source-mesh . --root src --format graphml --exclude-tests --output folders.graphml
```

Open `folders.graphml` in yEd and apply a layout. To find and highlight cycles:

```console
source-mesh . --root src --format graphml --exclude-tests --highlight-cycles --cycles-output folder-cycles.md --file-cycles-output file-cycles.md --output folders.graphml
```

| Option | Effect |
|---|---|
| `--depth 1` | Only folders immediately below `--root` (default: all depths) |
| `--group-folders` | Show subfolders inside their parent groups |
| `--format dot` / `--format mermaid` | Export another diagram format |
| `--exclude-tests` | Omit dependencies found only with test configuration enabled |
| `--highlight-cycles` | Give each connected group of cycles a distinct color |
| `--fail-on-cycles` | Exit unsuccessfully when file or folder cycle groups are found, after writing requested outputs |
| `--cycles-output FILE` | Write folder cycle chains to Markdown |
| `--file-cycles-output FILE` | Write file cycle chains before folder aggregation |
| `--verbose` / `-v` | Include diagnostic details and timings |

`A → B` means a file in A refers to a symbol defined in B. Edge labels count distinct file pairs. Folder aggregation can introduce cycles even when the file graph is acyclic.

Analysis of large projects can take several minutes. Use `--verbose` for diagnostic details and `source-mesh --help` for all options.

## GitHub Actions

`--fail-on-cycles` turns file or folder dependency cycles into a failing CI step. The diagram and requested reports are written before the failure, so the artifact is available for diagnosis. Adjust `--root src` for the source layout of the repository being checked.

```yaml
jobs:
  dependency-cycles:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7.0.1
        with:
          persist-credentials: false
      - run: rustup component add rust-analyzer rust-src
      - uses: Swatinem/rust-cache@v2.9.2
      - name: Install source-mesh
        run: cargo install --git https://github.com/GeneralGDA/rust-arch --locked source-mesh
      - name: Check dependency cycles
        run: |
          mkdir -p target/source-mesh
          source-mesh . --root src --format graphml --exclude-tests --highlight-cycles --fail-on-cycles --cycles-output target/source-mesh/folder-cycles.md --file-cycles-output target/source-mesh/file-cycles.md --output target/source-mesh/folder-dependencies.graphml
      - name: Upload cycle diagnostics
        if: ${{ failure() }}
        uses: actions/upload-artifact@v7.0.1
        with:
          name: source-mesh-cycle-diagnostics
          path: target/source-mesh
          if-no-files-found: warn
```

## Reusing analysis

Save analysis with `--save-scip analysis.scip`; reuse it with `--from-scip analysis.scip` to change formats, depth, or filters without analyzing the source again.

## Development

```console
cargo test --release --locked
cargo clippy --release --all-targets --locked -- -D warnings
```

On Windows, install the optional agent tools with:

```powershell
.\Install-AgentTools.ps1
```

The script installs missing `rg`, `fd`, `jaq`, and `ast-grep` through Cargo without replacing commands already on `PATH`.

Unit and component tests live under `src`; normal runs use synthetic data and a bundled SCIP fixture. [Live component tests](src/fixtures/README.md) require rust-analyzer. Integration tests under `tests` run the compiled application on this repository and check [expected exports and dependency cycles](tests/expected/README.md).

CI checks release builds, Clippy, unit and component tests, documentation tests, and repository integration tests on Linux, macOS, and Windows for every push and pull request.

## License

[MIT](LICENSE).
