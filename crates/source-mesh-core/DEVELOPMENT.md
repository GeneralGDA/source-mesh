# source-mesh-core development

## Running local tests

Run from the repository root:

```console
cargo test -p source-mesh-core --release --locked
```

For a focused run, filter by module; for example:

```console
cargo test -p source-mesh-core --release --locked --lib graph::
```

The default suite does not need an installed rust-analyzer. Analyzer process tests compile a test double,
so `rustc` must still be on `PATH`. After changing analyzer integration, also run the live-analyzer tests
linked under [Fixtures](#fixtures); offline replay alone cannot validate those changes.

## Adding regression tests

Keep unit and component tests under `src`, normally in the implementation file's inline test module.
Use separate suites for behavior shared across implementations, and keep test helpers with their owning tests.
Choose the layer that exercises the regression directly:

- `src/scip/`: parsing malformed indexes, resolving symbols, normalizing document paths, and classifying test references.
  These tests construct synthetic SCIP data; they do not need a source checkout for the indexed project.
- `src/graph/`: filtering, folder aggregation and weights, cycles, reports, and outgoing dependency limits on synthetic graphs.
- `src/analyzer/`: configuration, semantic lookup, repair, and subprocess/protocol failures using test doubles.
  `src/diagnostics/` checks emitted stderr. `src/backend.rs` covers input selection and live analysis of fixture projects.
- `src/application.rs` and `src/output_paths.rs`: saved-index replay, combined checks, output preservation on errors,
  and protection of inputs through renames and aliases.
- `src/export/`: format-specific tests stay with each format; the suites directly under `src/export/` exercise
  cross-format grouping, cycle styling, escaping, deterministic output, deep nesting, and writer failures.
  Reuse the shared graph fixtures in `src/export/rendering/` for cross-format cases.

## Regression pitfalls

- `#[ignore]` does not always mean a live-analyzer test. Resolver and diagnostic tests launch ignored helper entry points
  in child processes during ordinary test runs; running those entry points alone does not exercise the parent assertions.
- SCIP document paths may use Windows syntax even on Unix. When changing path handling, include both syntaxes in parser tests;
  do not validate saved-index paths against the local filesystem.
- Output protection needs file-identity cases, not only equal path strings. Keep rename and hard-link cases when changing
  output handling, and check that validation failures leave existing files untouched.
- Export templates disable HTML escaping. When adding interpolated data, cover format-specific escaping in
  `src/export/serialization_tests.rs`, including labels containing template delimiters. Rebuild after template edits;
  templates are embedded at compile time.
- For changes to folder grouping, keep deep-nesting coverage for both successful writes and writer errors: cleanup can
  overflow the stack too, even if rendering itself is iterative.

## Fixtures

The package includes the saved SCIP fixture for offline tests and the API example.
Cargo excludes nested fixture packages from published archives, so the ignored tests that run rust-analyzer
and the index regeneration tool require a repository checkout. Fixture projects retain standalone workspaces.

After changing `src/fixtures/dependency-graph/`, regenerate its saved index. Run the analyzer-backed tests after changing
any fixture project. See the [component fixture documentation](src/fixtures/README.md) for commands and covered scenarios.
