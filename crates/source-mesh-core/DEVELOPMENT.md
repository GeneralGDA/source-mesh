# source-mesh-core development

For workspace setup, test commands, and CI, see the [repository development guide](../../DEVELOPMENT.md).

## Exporters

Each exporter lives in its own directory under `src/export/`: `dot/`, `graphml/`, or `mermaid/`.
Its implementation, Askama template, and format-specific tests stay together.
The shared exporter contract, presentation structures, styles, and reusable test fixtures live in `src/export/rendering/`;
the format factory and cross-format tests remain directly under `src/export/`.
Concrete exporters depend on shared rendering code, not on the format factory, keeping folder dependencies acyclic.

Templates are checked and embedded at compile time; no template files are loaded at runtime.
Exporters prepare typed presentation data and render directly to the supplied writer.
Folder groups use flat event sequences so rendering and cleanup do not recurse with folder depth.
Templates disable HTML escaping: each exporter applies its own DOT, Mermaid, or XML escaping before rendering.

## Fixtures

The package includes the saved SCIP fixture for offline tests and the API example.
Cargo excludes nested fixture packages from published archives, so the ignored tests that run rust-analyzer
and the index regeneration tool require a repository checkout. Fixture projects retain standalone workspaces.

See the [component fixture documentation](src/fixtures/README.md) for regeneration and analyzer-backed test commands.
