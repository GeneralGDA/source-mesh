# source-mesh-core

Analyze Rust file and folder dependencies, export DOT, Mermaid, or `GraphML` diagrams,
and produce file and folder cycle reports. The `source-mesh` package provides the CLI;
this package exposes the same pipeline without command-line parsing.

```rust
use std::path::PathBuf;
use source_mesh_core::Input;
use source_mesh_core::Request;
use source_mesh_core::export::Format;

# fn main() -> anyhow::Result<()> {
let index = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    .join("src").join("fixtures").join("dependency-graph.scip");
let request = Request::builder()
    .input(Input::ScipFile(index))
    .format(Format::Mermaid)
    .build();
let mut diagram = Vec::new();
source_mesh_core::run(&request, &mut diagram)?;
assert!(!diagram.is_empty());
# Ok(())
# }
```

For live analysis, use `Input::Project(Project::new(project_path, configuration))`.
Live analysis requires the Rust toolchain and rust-analyzer; replaying a saved index
does not. `configuration` is an optional path to rust-analyzer JSON configuration.
All boolean options default to false. Configure reports and cycle gating through
`CycleOptions::builder()` and pass the result to the request's `.cycles(...)`
setter. Reports default to at most `DEFAULT_CYCLE_LIMIT` simple cycles each.
Pass `analyzer_options::Options::new(true)` to `.analyzer(...)` for verbose diagnostics.

Validation and analysis errors preserve existing outputs. Cycle gating with
`CycleOptions::builder().fail_on_cycles(true).build()` reports failure after writing requested outputs. Output I/O
errors can leave earlier files written; output writes are not a transaction.

The low-level analyzer, graph, cycle, and exporter APIs remain available.

## Development and publishing

Publish `source-mesh-core` before publishing the matching `source-mesh` CLI release.
The package includes the saved SCIP fixture for offline tests and the API example.
Cargo excludes nested fixture packages from published archives, so ignored live
tests and index regeneration require a repository checkout. Fixture projects
retain standalone workspaces. Run the regeneration tool from the checkout:

```sh
cargo run -p source-mesh-core --example regenerate-dependency-graph-scip
```
