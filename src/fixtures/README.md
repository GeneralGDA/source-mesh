# Component fixtures

`dependency-graph.scip` is a real rust-analyzer SCIP index of the adjacent `dependency-graph` crate,
with test dependencies classified by `source-mesh`. Tests replay it offline;
rust-analyzer is required only for regeneration and the ignored live tests.
Document paths are relative, and metadata uses the generic `file:///fixture/dependency-graph`
project root with no tool arguments, so the saved fixture contains no local checkout path.

The source deliberately covers:

- A direct qualified function call in `body_only/calls.rs`, without imports.
- Imported functions and types, including signature references.
- `#[path = "storage/db.rs"] mod persistence`, whose physical folder is storage.
- Two API files that depend on domain and storage, testing file-pair weights.
- Deep modules, same-folder file dependencies, and an isolated folder.

There are fourteen source files and nine directed file dependencies. With `--root src`
and no depth limit, seven edges cross folder boundaries. At depth one, the graph
has api -> domain (2), api -> storage (2), body_only -> storage (1), and
domain -> storage (1). The two remaining file dependencies stay within folders.

Regenerate from the repository root after changing fixture sources:

```console
cargo run --release --locked --example regenerate-dependency-graph-scip
cargo test --release --locked --lib backend::tests:: -- --ignored --test-threads=1
```

The generator runs both analyzer configurations, removes machine-specific metadata,
and verifies that the complete parsed dependency graph is unchanged before saving.

The four live backend tests call `RustAnalyzer` directly to verify dependency extraction.
The `dependency-graph` test compares the complete parsed file graph with the saved index
loaded through `ScipFile`. Export and command-line behavior are covered by the repository
integration tests.

The adjacent `test-dependencies` project covers inline and file-based `#[cfg(test)]` modules,
including dependencies shared by production and test code.

The `cargo-targets` project defines the same function names in two binaries and a library. Its live test checks semantic collision recovery, including local calls and explicit library calls from each binary, without false dependencies between binaries.

The `generated-methods` project calls derive-generated getters and setters through an inferred receiver type. Its live test checks that these calls retain dependencies on the defining source file, including test-only calls, even when the analyzer omits generated definitions from its exported index.
