# Expected repository exports

These integration tests launch the compiled executable against this repository once per run with rust-analyzer, then invoke the executable with that temporary SCIP index and compare complete DOT, Mermaid, GraphML, and Markdown cycle reports with the committed files here. The initial invocation uses the relative `Cargo.toml` path from the repository root. They do not call the library API. Ten configurations cover each diagram format, for 30 export cases, plus two report-only cases and two architecture cases with and without test dependencies.

Export cases cover test filtering, folder grouping, root selection, depth limits, self-edges, and cycle highlighting. Replays run from temporary directories without a source checkout. Report-only cases verify that requesting either cycle report preserves the default diagram without highlighting. Each export checks that only the requested files are created.

The architecture cases invoke the executable to produce Markdown reports and reject file dependency cycles and cycles between distinct folders at every depth. The complete GraphML output determines the indexed folder depths. Folder self-edges are excluded from this check because aggregation turns dependencies within a folder into self-edges, even when the files are acyclic. The expected cycle reports retain `--include-self` to cover that export behavior. Updating expected outputs never disables the architecture checks.

The `repository_tests` module name and `use regenerate_dependency_graph_scip as main` are intentional: rust-analyzer's [SCIP exporter](https://github.com/rust-lang/rust-analyzer/blob/master/crates/rust-analyzer/src/cli/scip.rs) can assign the same symbol to matching names in different Cargo targets of one package. These names keep full-repository analysis unambiguous.

From the repository root, with rust-analyzer and rust-src installed:

```console
cargo test --release --locked --test repository -- --ignored
```

Normal runs only compare files; CI runs this command on all three operating systems. Comparisons normalize CRLF to LF and, only in file cycle reports, encoded Windows path separators (`&#92;`) to `/`.

After intentional changes to the graph or exports, explicitly regenerate the expected outputs and review the diff before committing. Rust/rust-analyzer updates can also change the resolved graph; inspect those differences before accepting new expected outputs.

Bash:

```sh
SOURCE_MESH_UPDATE_EXPECTED_OUTPUTS=1 cargo test --release --locked --test repository -- --ignored
```

PowerShell:

```powershell
try {
    $env:SOURCE_MESH_UPDATE_EXPECTED_OUTPUTS = '1'
    cargo test --release --locked --test repository -- --ignored
} finally {
    Remove-Item -LiteralPath Env:SOURCE_MESH_UPDATE_EXPECTED_OUTPUTS -ErrorAction SilentlyContinue
}
```
