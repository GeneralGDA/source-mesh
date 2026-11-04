# source-mesh

[![crates.io](https://img.shields.io/crates/v/source-mesh.svg?style=for-the-badge&label=crates.io&color=orange&logo=rust&logoColor=white)](https://crates.io/crates/source-mesh)
![Linux CI](https://img.shields.io/github/check-runs/GeneralGDA/source-mesh/main.svg?nameFilter=ubuntu-latest&style=for-the-badge&label=Linux&logo=linux&logoColor=white)
![macOS CI](https://img.shields.io/github/check-runs/GeneralGDA/source-mesh/main.svg?nameFilter=macos-latest&style=for-the-badge&label=macOS&logo=apple&logoColor=white)
![Windows CI](https://img.shields.io/github/check-runs/GeneralGDA/source-mesh/main.svg?nameFilter=windows-latest&style=for-the-badge&label=Windows)
[![Coverage](https://img.shields.io/endpoint?url=https%3A%2F%2Fgeneralgda.github.io%2Fsource-mesh%2Fcoverage.json&style=for-the-badge&logo=rust&logoColor=white)](https://generalgda.github.io/source-mesh/)

Draw dependencies between physical folders in a Rust project. Export **GraphML for yEd**, **Mermaid**, or **DOT**, highlight cycles, and check cycles or outgoing dependency limits between folders or files.

## Install

Install the analysis components, then install the latest published release:

```console
rustup component add rust-analyzer rust-src
cargo install source-mesh --locked
```

Before the first crates.io release, install directly from GitHub:

```console
cargo install --git https://github.com/GeneralGDA/source-mesh --locked source-mesh
```

Cargo installs an optimized release build. Keep Cargo and rust-analyzer on `PATH`; install rust-analyzer and rust-src for the toolchain selected by the project you analyze. source-mesh checks both prerequisites before starting analysis.

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
| `--exclude 'src/generated/**'` | Remove matching files and their incoming/outgoing dependencies; repeat for multiple patterns |
| `--settings FILE` | Load an explicit TOML configuration instead of the project's `source-mesh.toml` |
| `--no-settings` | Disable TOML configuration loading |
| `--highlight-cycles` | Give each connected group of cycles a distinct color |
| `--fail-on-cycles` | Exit unsuccessfully when file or folder cycle groups are found, after writing requested outputs |
| `--cycles-output FILE` | Write folder cycle chains to Markdown |
| `--file-cycles-output FILE` | Write file cycle chains before folder aggregation |
| `--max-outgoing-dependencies N` | Fail when a file or folder has more than N distinct outgoing targets |
| `--outgoing-dependencies-mode files\|folders` | Select the threshold check's scope (default: `folders`) |
| `--outgoing-dependencies-output FILE` | Write outgoing dependency violations to Markdown; requires a threshold |
| `--verbose` / `-v` | Include diagnostic details and timings |

`A → B` means a file in A refers to a symbol defined in B. Edge labels count distinct file pairs. Folder aggregation can introduce cycles even when the file graph is acyclic.

Analysis of large projects can take several minutes. Use `--verbose` for diagnostic details and `source-mesh --help` for all options.

## Project configuration

`source-mesh.toml` is loaded automatically from the selected project directory (current directory by default).
Use `--settings FILE` to choose another file or `--no-settings` to skip it. All settings are optional:

```toml
[graph]
root = "."
depth = 3
include-self = false
exclude-tests = true
exclude = ["crates/*/src/generated/**", "**/fixtures/**", "crates/server/src/compat.rs"]

[export]
format = "graphml"
output = "folders.graphml"
group-folders = true

[cycles]
highlight-cycles = true
fail-on-cycles = true
cycles-output = "folder-cycles.md"
file-cycles-output = "file-cycles.md"
cycle-limit = 10000

[outgoing-dependencies]
maximum = 10
mode = "folders"
output = "dependencies.md"

[analysis]
verbose = false
```

- **Priority:** explicit CLI values > TOML values > built-in defaults. Use `--flag=false` to disable a configured flag.
- **Paths:** file paths in TOML are relative to its directory; CLI file paths are relative to the working directory.
  `root` remains project-relative.
- **Exclusions:** CLI `--exclude` values replace the TOML list, rather than append to it.

`[analysis]` also accepts `from-scip`, `save-scip`, and `config` (the rust-analyzer JSON configuration).

### Excluding files and folders

`--exclude` / `[graph].exclude` remove matching files and their dependencies from diagrams and all checks, not from indexing.

Patterns are case-sensitive and project-relative (not relative to `--root`), with `/` separators.
Use `src/generated/**` for a directory tree or `**/fixtures/**` for fixture directories at any depth. `.gitignore` is not used.

## Outgoing dependency limits

Limit each folder to at most five distinct dependency targets:

```console
source-mesh . --root src --exclude-tests --max-outgoing-dependencies 5 --outgoing-dependencies-output dependencies.md --output folders.dot
```

To check individual files instead, add `--outgoing-dependencies-mode files`. The default mode, `folders`, checks the final
folder graph after `--root` filtering and `--depth` folding. File mode checks the file graph before folder aggregation, so
folder depth does not affect its counts. Both modes respect `--root`, `--exclude-tests`, and `--exclude`.
`--group-folders` only changes diagram presentation, not dependency counts.

Each distinct target counts once, even if referenced repeatedly or by both production and test code. Folder edge weights
(the number of file pairs) are not summed. A retained folder self-edge counts as one target when `--include-self` is enabled.
`N` must be a non-negative integer: equality passes, and `0` forbids any outgoing dependencies. Without a threshold, no check runs;
an explicit mode or report path requires `--max-outgoing-dependencies`.

On a violation, source-mesh writes the diagram and requested reports, then exits with code `1`. Stderr lists every violating
file or folder with its count and the threshold. The optional Markdown report lists the same violations, or explicitly states
that none were found. When combined with `--fail-on-cycles`, diagnostics include both kinds of failure.

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
        run: cargo install --git https://github.com/GeneralGDA/source-mesh --locked source-mesh
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

For contributor documentation, see the
[development guide](https://github.com/GeneralGDA/source-mesh/blob/main/DEVELOPMENT.md).

## License

[MIT](LICENSE).
