# Changelog

Release history for `source-mesh` and `source-mesh-core`.

## 1.2.1 - 2026-11-10

Housekeeping release focused on documentation and development tooling, with no changes to the CLI or core library code.

### Changed

- Reworked documentation.
- Moved coverage tool installation to development setup and CI; coverage runs now check prerequisites without installing tools.

## 1.2.0 - 2026-11-07

### Added

- Project configuration files (`source-mesh.toml`).
- File and folder exclusions in the CLI and core library.
- Overwrite protection for additional input files in the core library.
- Cycle reports in GitHub Actions summaries.

### Fixed

- Truncated saved-analysis files could be accepted.

### Breaking changes

- `Request::builder().build()` returns `Result<Request>` instead of `Request`.
- Saved-analysis files are read during request construction instead of on each execution.

## 1.1.1 - 2026-11-02

### Added

- Test coverage reports.

## 1.1.0 - 2026-10-26

### Added

- Outgoing dependency limits for files and folders, with violation reports, in the CLI and core library.
  Requested diagrams and reports are written before dependency-limit failures.

### Changed

- Shortened CLI help and error messages.

### Fixed

- Input files could be overwritten if their paths changed during analysis.
- Existing output files could be truncated before output validation completed.
- Valid source paths containing `..` were rejected.
- Stack and coordinate overflows in grouped GraphML export.

### Breaking changes

- `FileNode::new` accepts `SourcePath` and returns `FileNode` instead of `Result<FileNode>`.
- `export::Options::new` was removed; `export::Options::builder()` was added.
