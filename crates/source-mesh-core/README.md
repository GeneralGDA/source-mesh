# source-mesh-core

Dependency graphs, cycle reports, and outgoing dependency limits for Rust projects.
Exports DOT, Mermaid, and `GraphML`.

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

## Inputs

`Input::Project(Project::new(project_path, configuration))` analyzes a Rust project. It requires a Rust toolchain with
`rust-analyzer` available on `PATH` and `rust-src` installed for the project's toolchain:

```console
rustup component add rust-analyzer rust-src
```

`configuration` is an optional path to rust-analyzer JSON configuration; use `None` for the default settings.
`.save_scip(path)` saves the analysis index for reuse.

`Input::ScipFile` loads the index during request construction and reuses it on subsequent runs without launching rust-analyzer
or reindexing the project. It cannot be combined with `save_scip`.

## Request options

Boolean flags default to `false`.

- `.format(...)` selects the diagram format; the default is `Format::Dot`.
- `.output(path)` writes the diagram to a file instead of the writer passed to `run`.
- `.cycles(...)`: `CycleOptions::builder()` configures highlighting, reports, and failure on cycles.
  Each report defaults to at most `DEFAULT_CYCLE_LIMIT` simple cycles.
- `.outgoing_dependencies(...)`: `OutgoingDependencyOptions::builder().maximum(5).build()` sets a dependency limit.
  The default scope is folders; `.mode(outgoing_dependencies::Mode::Files)` selects files, and `.output(path)` adds a Markdown report.
- `.exclusions(...)`: `Exclusions::new(...)` compiles project-relative glob patterns for all graphs and checks.
- `.analyzer(analyzer_options::Options::new(true))` enables verbose diagnostics.
- `.protected_inputs(...)`: `InputFile` values protect additional input files and their aliases from output overwrites.

## File filtering

`.root(path)` restricts the graph to a project-relative directory; it defaults to `.`. Files outside that directory and all
edges to or from them are removed. `.exclude_tests(true)` removes test dependencies, but keeps files even if they become isolated.

`Exclusions::new(&patterns)` removes matching files and all edges to or from them. Patterns are case-sensitive and relative to
the indexed project, not to `root`, with `/` separators. Use `src/generated/**` for a directory tree or `**/fixtures/**` for
fixture directories at any depth. `*` does not cross directory separators. Negation, absolute paths, `.` or `..` path components,
and backslashes are not supported. `.gitignore` is not used.

These filters apply to diagrams, cycle checks, and outgoing dependency limits after indexing; they do not restrict indexing
or change the saved SCIP index. An empty filtered graph is an error.

## Outgoing dependency limits

`OutgoingDependencyOptions::builder().maximum(n)` permits at most `n` distinct outgoing targets per source. A count equal to
`n` is allowed; `0` forbids outgoing dependencies. Repeated references and edge weights do not multiply the count. A target
reached by both production and test dependencies counts once.

- `outgoing_dependencies::Mode::Files` counts distinct target files before folder aggregation.
- `outgoing_dependencies::Mode::Folders` counts distinct target folders after `.depth(n)` folding. Folder depth is relative
  to `root` and is unlimited by default. Dependencies within the same resulting folder are omitted unless
  `.include_self(true)` is set, in which case the folder itself counts as one target.

Both modes respect `root`, `exclude_tests`, and `exclusions`. `depth` and `include_self` affect only the folder graph, not file counts.

## Outputs and failures

`run` writes requested outputs before returning cycle or dependency-limit failures. Validation and analysis errors preserve
existing outputs; output I/O errors can leave partial results. Input files remain protected against output overwrites through
renames and aliases.
