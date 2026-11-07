# source-mesh-core

Dependency graphs, cycle reports, and outgoing dependency limits for Rust projects.
Exports DOT, Mermaid, and `GraphML`. The `source-mesh` package provides the CLI.

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
        .build()?;
    let mut diagram = Vec::new();
    source_mesh_core::run(&request, &mut diagram)?;
    assert!(!diagram.is_empty());
    Ok(())
}
```

`Input::Project(Project::new(project_path, configuration))` requires a Rust toolchain with rust-analyzer and rust-src.
`configuration` is an optional path to rust-analyzer JSON configuration.

`Input::ScipFile` loads the index during request construction and reuses it on subsequent runs. It cannot be combined with
`save_scip`.

Request options (boolean flags default to `false`):

- `.cycles(...)`: `CycleOptions::builder()` configures highlighting, reports, and failure on cycles.
  Each report defaults to at most `DEFAULT_CYCLE_LIMIT` simple cycles.
- `.outgoing_dependencies(...)`: `OutgoingDependencyOptions::builder().maximum(5).build()` sets a dependency limit.
  The default scope is folders; `.mode(outgoing_dependencies::Mode::Files)` selects files, and `.output(path)` adds a Markdown report.
- `.exclusions(...)`: `Exclusions::new(...)` compiles project-relative glob patterns for all graphs and checks.
  An empty filtered graph is an error.
- `.analyzer(analyzer_options::Options::new(true))` enables verbose diagnostics.
- `.protected_inputs(...)`: `InputFile` values protect additional input files and their aliases from output overwrites.

Counting and filtering rules are described under
[outgoing dependency limits](https://github.com/GeneralGDA/source-mesh/blob/main/README.md#outgoing-dependency-limits) and
[file exclusions](https://github.com/GeneralGDA/source-mesh/blob/main/README.md#excluding-files-and-folders).

`run` writes requested outputs before returning cycle or dependency-limit failures. Validation and analysis errors preserve
existing outputs; output I/O errors can leave partial results. Input files remain protected against output overwrites through
renames and aliases.

See the [changelog](https://github.com/GeneralGDA/source-mesh/blob/main/CHANGELOG.md) and
[development guide](https://github.com/GeneralGDA/source-mesh/blob/main/crates/source-mesh-core/DEVELOPMENT.md).
