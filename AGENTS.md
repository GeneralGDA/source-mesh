# Project Rules

## Code Style

- Keep source identifiers, code comments, CLI messages, and repository documentation in English.
- Prefer precise names and cohesive functions over explanatory comments. Remove comments that restate code;
  keep only essential constraints or external behavior that code cannot express clearly, and verify their accuracy.
- Do not add section divider comments (e.g. `// --- Logging ---`, `// --- Main ---`).
  Let the code structure speak for itself.
- When a source-code comment spans multiple lines, use a block comment (`/* ... */`) instead of consecutive `//` comments.
- Annotate pure functions with `#[must_use]` only when the return type or returned trait is not already marked
  `#[must_use]` (e.g. `Result` and `Iterator` already carry that contract). Do not duplicate the attribute on such functions.
- Use only the bare `#[must_use]` attribute, without a custom message (`#[must_use = "..."]`) or an explanatory comment.
- Use the `const` qualifier on functions wherever possible.
- Do not use abbreviations in identifiers (e.g. use `destination` not `dest`, `directory` not `dir`,
  `source` not `src`, `buffer` not `buf`, `length` not `len`, `index` not `idx`, `capabilities` not `caps`).
  Acronyms that are widely understood as standalone terms (e.g. `url`, `json`, `api`) are fine.
- Name values by the domain entity and semantic kind they represent, not just their temporary role in an operation.
  Preserve meaningful type vocabulary without mechanically repeating type names (e.g. `cargo_build_output_directory`,
  not `output_directory`). Use terms that match what the code actually does: a downloaded and extracted directory is
  not an unexplained `installation_root` that implies a separate installation process.
- Judge whether a name has enough context from its enclosing function or type, not from the file's current sole task.
  General-purpose entry points such as `main` and build scripts do not provide dependency-specific context. Give cohesive
  domain-specific tasks a named scope and still keep the values within it clear.
  Short role names such as `source_directory` and `destination_directory` are appropriate in a generic copy helper whose
  contract makes their meaning explicit; do not add redundant domain prefixes where the context is already unambiguous.
- Prefer boilerplate-reduction crates such as `getset`, `bon`, and `derive_new` for new Rust types, reusing existing
  project dependencies where available.
- When a function has enough required parameters that its positional call sites are hard to scan or easy to misuse,
  prefer `#[bon::builder]` so each argument is supplied by a semantic name. Keep a direct function interface for short,
  obvious parameter lists; do not add a builder speculatively.
- Keep delegate, callback, and closure interfaces self-explanatory. When one receives several values — especially two or
  more values of the same type — prefer one small typed context with semantically named accessors over a positional
  argument list. Do not rely only on closure-local parameter names to distinguish values.
- Never expose fields in hand-written Rust types with `pub`, `pub(crate)`, or `pub(super)`. Keep fields private and expose
  required accessors with `getset`.
- Keep every type's API surface minimal: derive or implement only traits and expose only methods and accessors that are
  required by the code at the present time. Do not add capabilities speculatively.
- Prefer local `use` imports over long fully-qualified paths when they improve readability, especially in tests.
- Avoid duplicating string literals. If the same message or label is used in production code and tests,
  extract it into a named constant and reuse that constant.
- Inline a function that has a single call site unless it establishes a meaningful domain or responsibility boundary,
  such as a named task called by a general-purpose entry point. A single caller alone is not a reason to erase that
  context. For one-off computations that do not need such a boundary, use a block expression (`let value = { ... };`)
  when local variables are needed.
- Introduce a variable for a value used once only when it makes the consuming statement one line or substantially
  clearer; otherwise, inline the expression.
- When moving an item between modules, update every `use` site. Use a separate `use` statement for each imported item
  instead of merging imports into brace groups.
- Prefer API-provided inference over duplicating names or configuration in string literals. Specify a value explicitly
  when disambiguation is required.
- When a borrow must end before a later operation, prefer enclosing the borrowed value and its work in a block expression
  instead of calling `drop` explicitly. Use explicit `drop` only when a lexical scope would make the control flow less clear.
- When an API consumes a string only for the duration of a call, accept `impl AsRef<str>` when callers may naturally hold
  either borrowed or owned labels. Do not use `Into<&str>` for this: it cannot accept an owned `String` without a lifetime.
- Construct native filesystem paths from individual components with `Path::join`, `PathBuf::push`, or `PathBuf::from_iter`,
  rather than embedding `/` or `\` between components in string literals or concatenating or formatting path strings.
  Apply this to production code and test setup. Keep literal separators in tests that intentionally exercise raw path
  syntax, in data with format-defined separators (e.g. URLs and archive entry names), and in compile-time path literals
  (e.g. `include_str!`). Do not replace separators in such data with the host platform's separator.
- Write a standard formula with its literals inline, exactly as it appears in the reference, and name the reference in a
  comment above it. Do not split its coefficients into named constants: they carry no meaning of their own, are not
  reused, clutter the namespace, and make the formula impossible to compare with the reference by eye.
- Derive buffer sizes and related element counts from their Rust element or container types using `size_of`, rather
  than duplicating byte counts as numeric literals. Derive dependent constants from one source of truth (e.g. buffer byte
  size from element count and element type).

## Error Handling

- Every function must validate its input preconditions at entry. Prefer parameter types that encode invariants; when that
  is impractical, use `assert!` with a descriptive message for programmer contract violations. Reuse shared precondition
  helpers for recurring checks instead of duplicating assertion sequences. Do not silently accept an invalid input by
  clamping or normalizing it. Assertions are for caller bugs, not recoverable runtime failures.
- Avoid `unwrap()` and `expect()`. Prefer restructuring code so they are not needed —
  for example, by calling methods on a local variable and only storing it in its final
  location after the call succeeds.
- If a `None` or `Err` case is logically unexpected but cannot be eliminated structurally,
  handle it with `if let Some(...) { ... }` and log a warning. Do not panic in
  production code paths.
- Do not silence callback or channel operation results by assigning them to an underscore-prefixed variable. Handle the
  error explicitly; in tests, assert success with a descriptive message.

## Tests

- Keep unit and component tests under `src`, near the implementation they exercise, including synthetic graphs,
  SCIP fixtures, argument validation, and application pipeline checks. Keep their fixture projects under `src/fixtures`.
- Reserve `tests` for repository-level checks that launch the compiled application on this repository and compare
  its output with expected output files under `tests/expected` or enforce repository architecture constraints. Do not call production library APIs
  from these tests. A fresh temporary index may be shared between executable runs within one test session.
- Keep successful CLI export combinations in repository integration tests. Keep argument validation and
  output preservation on errors as focused command tests; exercise graph algorithms, exporters, and analyzer
  semantics directly in their owning component tests instead of repeating the full command pipeline.
- Express test expectations with `assert!`, `assert_eq!`, or `assert_ne!`; use `pretty_assertions::assert_eq!`
  for text or structured values where a diff helps diagnosis. Reserve `Result` and `?` for fallible fixture setup
  and test operations; do not use `anyhow::ensure!` for expectations.
- Name test functions after the behaviour or property under test, not the expected result
  (e.g. `selection_key`, not `source_returns_original_format`).
- Prefix all test function names with `test_` (e.g. `test_selection_key`).
- Name the instance of the type under test `system_under_test` (e.g. `let system_under_test = MyStruct::new();`).
- Prefer `Fixture` terminology for shared test resources/environment types and variables (e.g. `TestFixture`, `let fixture = ...`).
- Structure test bodies as clear phases: fixture setup, system-under-test exercise, assertions, and cleanup when needed.
  Separate these phases with blank lines where it improves readability.
- Keep side effects and result assertions in separate statements. Assign an operation's `Result` first, then pass that
  already-computed result to `assert_ok!` or another assertion macro; do not hide callbacks or other operations
  inside assertions.
- Keep constants that describe one test scenario local to that test, rather than at test-module scope.
- Use parameterized `rstest` cases when the same behaviour is exercised with multiple input/expected-value combinations,
  including platform variants. Do not collect those cases into an array and manually unpack and check every result in one
  test, or duplicate the test function for each case.
- Use `#[should_panic]` without its `expected` parameter.
- In test doubles, use `unreachable!()` for trait methods or code paths that must not be invoked by the test,
  with a message that names the unexpected call. Do not use `panic!()`, `todo!()`, or `unimplemented!()` for these cases.
- Keep production types and APIs free of test-only constructors, methods, or behaviour. In particular, do not add
  `#[cfg(test)]` methods to production types solely to inspect private state: child test modules can access their
  parent module's private fields directly. Put genuinely useful test helpers in the tests that use them or in shared
  test utility modules.
- Put setup that every test of a type needs into the fixture constructor instead of repeating it in each test.
- When a fixture owns a resource that a test has to consume while immutable borrows of the fixture are alive,
  store it as `RefCell<Option<...>>` and `take()` it, instead of taking `&mut self` — a `&mut self` method would
  conflict with borrows handed out by the fixture's methods.
- Share test setup through a plain function returning a tuple, not through a helper that takes the test body as a closure.
  Test bodies must stay flat.
- Extract repeated construction of test input data into a named helper, but do not use such a helper in the tests of the
  very type it builds — there the construction is the subject under test.
- Group shared test utilities into several modules by topic rather than one catch-all `test_utils`. Do not split down to
  one helper per file: a lone helper belongs in the closest existing test module by topic.

## Architecture

- Keep CLI help, normal output, and the README focused on dependency graphs and cycle checks. Backend formats and
  internal diagnostics are implementation details; expose raw backend output only with `--verbose`. Errors must explain
  why the requested result could not be produced, without treating internal backend messages as defects in the user's code.

- Keep `mod.rs` and `lib.rs` strictly for module organization: module declarations, visibility,
  re-exports, and related attributes or documentation. Put functions, types, traits, implementations,
  constants, macros, and inline module or test bodies in ordinary `.rs` files. Do not hide implementation
  in these entry files through `include!`. This rule also applies to tests and fixtures.
- Shared script utilities are for building blocks used by multiple scripts. Keep code specific to a single script,
  including its tests, private to that script. Do not put single-script logic in shared utilities under a generic name
  or extract it there for speculative reuse.
- When a function needs all members of an existing cohesive context type, accept that context type instead of passing its
  members separately. Do not use a broad context type when the function needs only a small unrelated subset of its interface.
- Localize platform-dependent behaviour behind a dedicated abstraction with platform-specific implementations. Keep
  production `#[cfg(target_os = ...)]` and `#[cfg(target_arch = ...)]` directives at the narrow implementation-selection
  boundary instead of scattering them through common code. Common code must express the platform capabilities it needs
  through the abstraction and delegate OS-specific operations to it. Treat minimizing target-specific `#[cfg]` sites as an
  architectural requirement, not merely a cosmetic cleanup.
- Do not treat generated files as the source of truth. If a requested change affects generated code, prefer updating
  the generator and/or its templates rather than editing generated files directly.
- Direct edits in generated files are acceptable only as a temporary inspection aid or when the user explicitly asks
  for them; otherwise, make the durable change in the generator or its templates.
- Never format generated files, including indirectly through workspace-wide formatting commands. Generated files may
  change only through the generation pipeline.

## Agent Tools

- Prefer `rg`, `fd`, `jaq`, and `ast-grep` when available and when they can return less, more relevant output than
  general-purpose shell tools. Check availability; do not assume this repository installs them.
- Keep command output focused to conserve context: scope searches by path and file type, refine broad queries, project
  only required JSON fields with `jaq`, and prefer `ast-grep` over text search when code structure matters.
- Do not use interactive selectors, pagers, file watchers, or decorative output in autonomous workflows. They can block
  execution or add ANSI and formatting noise without giving the agent useful information.

## Workflow

- Keep GitHub Actions workflow files as thin declarative wrappers: configure jobs, environment, and action/script calls
  in YAML. Put nontrivial shell or PowerShell logic (conditionals, loops, validation, downloads, installation) in separate
  files under `.github/scripts/`, not inline `run` blocks. Simple script or tool invocations may remain in YAML.
- Before introducing a constant, path, helper, or abstraction, search the repository for the same knowledge and
  identify its current owner. Extend the owning abstraction instead of creating a second source of truth.
- After centralizing knowledge, search for the original literals and symbols again. Verify that every remaining occurrence
  is intentional and that consumers use the owning abstraction.
- Before adding logic to an existing type, state that type's current responsibility. If the new logic performs unrelated
  parsing, file access, policy decisions, or validation, place it in a dedicated abstraction and let the existing type
  delegate.
- Do not silently implement a shortcut that contradicts the design already proposed to or accepted by the user. If the
  design calls for a reusable parser or validator, implement that boundary rather than embedding the logic in the first
  available consumer.
- After implementation, perform a reviewer pass rather than only a compiler pass. Check specifically for duplicated
  knowledge, misplaced responsibilities, unnecessary dependencies, speculative behavior, excessive API changes, and error
  messages based on false assumptions.
- Inspect the complete targeted diff before reporting completion. Remove incidental formatting and unrelated edits, then
  verify that dependency manifests and lock files changed only when required.
- When the user corrects a design assumption, discard the previous mental model and restate the contract before making
  another patch. Do not incrementally repair an implementation built on the rejected assumption.
- After a refactoring, run `cargo test -p <crate>`.
- Never run `rustfmt`, `cargo fmt`, editor formatting actions, or any other formatting command. Leave all formatting to the user or their editor.
- Never suppress Clippy lints with `#[allow]`, `#[expect]`, crate- or module-level lint overrides, or command-line flags.
  The only exception is `#[expect(clippy::panic_in_result_fn, reason = "...")]` on a test-only module: tests may return
  `Result` for fallible fixture setup while using direct assertions. Rewrite production code to satisfy every lint using
  safe or checked operations and explicit error handling. If no compliant solution is available, stop and ask the user
  instead of suppressing the lint.
- If `cargo clippy --all-targets` is already failing before the change, report that as a pre-existing state instead of
  mixing it into the results of the refactoring.
