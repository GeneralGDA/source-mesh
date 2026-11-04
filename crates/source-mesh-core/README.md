# source-mesh-core

Analyze Rust file and folder dependencies, export DOT, Mermaid, or `GraphML` diagrams,
and produce file and folder cycle reports or check outgoing dependency limits. The `source-mesh` package provides the CLI;
this package exposes the same pipeline without command-line parsing.

```rust
use std::path::PathBuf;
use source_mesh_core::Input;
use source_mesh_core::Request;
use source_mesh_core::export::Format;

fn main() -> anyhow::Result<()> {
    let index = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src").join("fixtures").join("dependency-graph.scip");
    let request = Request::builder()
        .input(Input::ScipFile(index))
        .format(Format::Mermaid)
        .build();
    let mut diagram = Vec::new();
    source_mesh_core::run(&request, &mut diagram)?;
    assert!(!diagram.is_empty());
    Ok(())
}
```

To analyze project sources, use `Input::Project(Project::new(project_path, configuration))`.
This runs rust-analyzer and requires the Rust toolchain, rust-analyzer, and rust-src;
replaying a saved index does not. `configuration` is an optional path to rust-analyzer JSON configuration.
All boolean options default to false. Configure reports and cycle gating through
`CycleOptions::builder()` and pass the result to the request's `.cycles(...)`
setter. Reports default to at most `DEFAULT_CYCLE_LIMIT` simple cycles each.
Pass `analyzer_options::Options::new(true)` to `.analyzer(...)` for verbose diagnostics.

Validation and analysis errors preserve existing outputs. Cycle gating with
`CycleOptions::builder().fail_on_cycles(true).build()` reports failure after writing requested outputs. Output I/O
errors can leave earlier files written; output writes are not a transaction.

Configure an outgoing dependency check with `OutgoingDependencyOptions::builder().maximum(5).build()` and pass it to
`.outgoing_dependencies(...)` on the request. The default mode is `outgoing_dependencies::Mode::Folders`; use
`.mode(outgoing_dependencies::Mode::Files)` to check files before folder aggregation. An optional `.output(path)` writes a
Markdown violation report. Each distinct target counts once regardless of dependency kind or folder edge weight;
retained self-edges count as one target. Root and test filters apply in both modes, while depth folding affects only folders.
Counts equal to the maximum pass, and zero is supported. A violation fails after outputs have been written and includes every
violating source and count in the error; simultaneous cycle failures are included too.

To exclude files from every graph and check, compile project-relative glob patterns with
`Exclusions::new(&["src/generated/**".to_owned()])?` and pass them to `.exclusions(...)` on the request.
Filtering removes matching files and all their incident edges before folder aggregation, including during saved-index replay.
An empty filtered graph is an error. The low-level equivalent is `FileGraph::excluding(&exclusions)`.

TOML discovery and CLI precedence belong to the `source-mesh` executable, not this library. Callers that load additional input
files can retain them as `InputFile` values and pass them to `.protected_inputs(...)` so outputs cannot overwrite those files
or their aliases. `InputFile::read` reads through the same retained handle used for output protection.

The low-level analyzer, graph, cycle, outgoing dependency analysis, and exporter APIs remain available.

For contributor documentation, see the
[development guide](https://github.com/GeneralGDA/source-mesh/blob/main/crates/source-mesh-core/DEVELOPMENT.md).
