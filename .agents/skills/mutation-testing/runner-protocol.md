# Safe execution protocol

These are requirements for a small session-local runner or an existing mutation tool, not a request to build a general framework.
Check an installed tool's actual options and isolation guarantees before choosing it. Do not invent command-line flags.
For a manual runner, keep replacement data separate from execution logic and use only already-available dependencies.

## Session and snapshot boundaries

1. Resolve the repository root explicitly; do not infer it from how many parents the runner happens to have.
2. Allocate a new session directory under Git-ignored `work/mutation-testing/`. Choose a unique name and refuse to
   overwrite an existing session. Resolve concrete paths before issuing commands; do not leave placeholders in tool calls.
3. Create `before` and `after` snapshots from the actual working tree at the appropriate stages. Derive required inputs
   from the current workspace manifests and build configuration: include workspace members, referenced local dependencies,
   toolchain configuration, templates, and test fixtures. Do not use a fixed list of crate directories or a HEAD-only export
   that loses working changes. Exclude Git metadata, experiment output, and every nested build-artifact directory.
4. Allocate a distinct absolute target directory for every session and phase, outside its source snapshot. Pass that exact
   `--target-dir` to every Cargo build, test, and test-listing command in the snapshot. Never use the working checkout's
   target directory or reuse one phase's target directory for another source snapshot, even if the files look identical.
   Cargo freshness with copied timestamps previously caused a stale mutant binary to run against the working checkout.
5. Treat unmutated phase snapshots as immutable. Record source hashes, manifest hash, command/profile, toolchain version,
   test inventory/counts, and baseline result. If tests or production code change, make a new phase revision and baseline;
   do not reuse results for the old bytes. Never run agents that edit the same snapshot concurrently.
6. Warm and validate each baseline serially. Normal working-checkout baseline commands can use its usual target directory;
   copied workspaces must always use the explicit private target directory. No routine `cargo clean` is necessary.

If editor file tools cannot address an ignored artifact path but the terminal confirms it, use terminal file operations
only for those session-owned artifacts. Do not change `.gitignore` or repeatedly retry the same failed file-tool call.
Continue using normal file tools for working-tree source edits.

## Process and deadline budgets

- Give every build/test a bounded inner deadline and every tool invocation a longer outer deadline, with cleanup margin.
  A reasonable initial cold-build budget is 300 seconds inside a tool call of at least 360 seconds; measure and adjust
  transparently. Do not combine a cold baseline and a long mutant batch in one short tool call.
- Prefer one mutant per tool call. Batch only when the sum of all permitted inner deadlines plus setup/cleanup margin
  fits the outer deadline; average duration alone is not a safe bound. Do not launch a step without sufficient budget.
- Avoid concurrent Cargo builds initially: even private target directories can contend for the Cargo package cache and CPU.
  Independent code inspection and disjoint test edits can still be parallelized.
- On timeout, stop only the process tree started by the runner, wait for cleanup, restore the mutation, and record timeout.
  Do not terminate every Cargo/rustc process or another agent's work. Report the timeout; obtain the user's decision before
  increasing its deadline. Continuing smaller batches at the existing deadline is distinct from claiming a completed run.
- On Windows under Git Bash, shell invocation can rewrite `/PID`, `/T`, `/F`, or `/FI` as paths. Invoke native cleanup tools
  with an argument list from `subprocess`, with `shell=False`, or a verified platform adapter. The historical
  `taskkill.exe` strategy is Windows-specific; do not assume it works on another OS.

## Apply one mutation transactionally

Before trusting the runner, cheaply exercise its selection, restoration, and interruption paths on disposable files.
Then check a freshly derived control with a current distinguishing test. Do not count harness self-tests as project mutants.

For every selected ID:

1. Validate the manifest schema, unique ID, nonempty original snippet, and distinct original/replacement text.
2. Resolve the target strictly within the phase's source snapshot. Reject absolute paths, parent escapes, and symlinks
   resolving outside it. Verify the target is production code, not tests, fixtures, generated code, or another dependency.
3. Match and replace exact bytes, never use fuzzy matching. If manifest snippets encode LF but the target uses consistent
   CRLF, convert line breaks in both snippets to CRLF before matching. Do not normalize the entire file or strip whitespace.
   For mixed line endings, require explicitly byte-matching snippets instead of guessing. Require exactly one original
   match and inspect the current source/test boundaries to establish that it is production code.
   Retain untouched raw file bytes for hashes, backups, and restoration.
4. Verify that the current file matches its recorded unmutated hash. Persist original bytes and an active-mutation journal
   before writing anything: include the target path, mutation ID, original/mutated hashes, backup path, and owned process IDs.
5. Write the one replacement, then compile and run the phase's full library suite. Capture complete output in a separate
   attempt log. Scoring requires test execution, not just successful compilation or a nonzero exit status.
6. In `finally`, restore the saved bytes only if current content equals the expected mutant. If it already equals the
   original, accept that state. If it equals neither, stop and investigate; never overwrite an unexpected concurrent edit.
7. Verify byte-for-byte restoration. Save the result atomically and mark the journal complete. Preserve prior attempt logs.

A tool or machine can kill the runner before `finally` runs. At the start of every invocation, inspect incomplete journals
before permitting any new work. Check that owned processes have exited, compare file hashes, and restore only a recognized
mutant from its persisted original. Mark the old attempt interrupted, not killed or survived. A hash mismatch is a reason
to stop, not permission to reset files. Do not delete snapshots or logs needed to recover or explain an interrupted attempt.

## Commands, discovery, and outcome classification

Derive the core-library test command from current project configuration. Within a phase, use the same package, profile,
features, test selection, and private target directory for baseline and mutants. Keep dependencies locked and append the
explicit `--target-dir`. Use offline resolution when dependencies are already cached; an offline resolution failure is an
environment blocker, not a kill. A preceding `--no-run` build can distinguish compiler failures but cannot replace test execution.

Store the invocation, full log, return code, duration, summary counts, and failing test names. Use these categories:

| Outcome | Required evidence |
| --- | --- |
| `killed` | Compilation succeeded; actual tests ran and failed because of the semantic change |
| `survived` | The full suite completed successfully and its executed/discovered totals match the phase inventory |
| `build-error` | The mutant did not compile; exclude it from behavioral kills |
| `run-error` | Environment, launch, inventory, or unexplained execution failure; investigate separately |
| `timeout` | Inner deadline expired; inconclusive, never a kill |
| `interrupted` | Runner/tool ended before a conclusive saved result; recover before resuming |
| `inapplicable` | Stale, ambiguous, or out-of-scope replacement; it was not executed |

Equivalent mutations are a triage annotation on completed results, not an automatic inference from survival.
A signal, any Cargo exit code, or an isolated `test result: ok` line alone is insufficient. Reject zero executed tests,
missing summaries, wrong inventories, or lost registrations. Check individual new case names with `-- --list` and a focused
run before taking the `after` snapshot. In a stable suite, passed + failed + ignored totals should match its baseline.
Investigate discrepancies rather than relying on test counts from a previous session.

Before reporting a closed gap, retain the three relevant results: old tests survive the mutant, strengthened unmutated
code passes, and strengthened tests fail the same mutant. Replay all selected completed mutations for comparable totals.
Keep infrastructure failures and uncompleted attempts visible instead of rerunning silently until a favorable score appears.
