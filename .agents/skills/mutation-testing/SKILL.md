---
name: mutation-testing
description: Audit core tests using fresh, code-derived mutations and add verified regressions without changing production code.
disable-model-invocation: true
---

# Mutation testing of the computational core

Execute a bounded session, not just a plan. Follow repository `AGENTS.md` and any scope or budget supplied by the user.
Read [runner-protocol.md](runner-protocol.md) before running mutations. This skill is an agent workflow, not a shell program.

## Stable architectural scope

source-mesh analyzes source dependencies and detects graph cycles. Focus on the computational behavior of its core library:
analysis semantics, graph transformations, and graph algorithms. Exclude CLI handling and process, filesystem, or formatting
wrappers unless requested. Discover current packages, modules, and tests from the working code; do not assume a fixed layout.

## 1. Establish the current baseline

1. Briefly state the scope. Record `git --no-pager --no-optional-locks status --short` and preserve all existing user changes.
2. Identify the core package from current Cargo manifests and development instructions. Read the relevant implementations
   with their tests; keep discovery focused on the selected responsibilities instead of mapping the entire repository.
3. Check the actual toolchain and available mutation tools once. Do not assume an old environment or session runner still
   applies. Use a small session-local runner if needed, without adding dependencies or building a permanent framework.
4. Derive and run the core-library test and lint baseline commands from the current project. Record the exact package,
   profile, features, discovered tests, and passing/ignored counts. Use bounded tool calls.
   A failing test baseline invalidates mutation scores; distinguish pre-existing issues and do not fix unrelated problems.
5. Freeze an isolated `before` snapshot of the current working files before editing tests, including legitimate uncommitted
   changes. Confirm its unmutated baseline with its own target directory, as required by the execution protocol.

## 2. Derive new mutations from current behavior

Do not replay a stored mutation bank or copy old text replacements. Generate candidates anew from the implementation,
its contracts, and the assertions that exist now. Prefer plausible single-site semantic mistakes over arbitrary breakage.
Look for conditions, boundaries, direction, information preservation, aggregation, and iteration whose behavior matters
to the result. These are selection heuristics, not a fixed checklist of code edits.

For every candidate, record in a session-local manifest:

- A unique ID, current source location, and exact original/replacement text.
- The observable behavior that would change and the contract establishing the correct result.
- Why existing assertions might miss it and an input that could distinguish the implementations.

Choose a bounded batch across independent core responsibilities. Include a freshly derived control mutation for which a
current assertion should fail, to verify the harness. Label controls separately from exploratory candidates. Do not add
mutations just to reach a quota, and do not equate a selected-set score with repository-wide coverage.

Independent code/test analysis can be delegated. Request precise replacements and behavioral reasoning, not speculative
lists. Assign disjoint write scopes for later test edits; never let multiple agents mutate the same snapshot.

## 3. Execute and classify

Follow [runner-protocol.md](runner-protocol.md) for isolation, deadlines, durable recovery, and result recording.

- Apply one mutation at a time to production code in the isolated snapshot, never to the checkout or test assertions.
- Run the full selected non-ignored core-library suite for each scored mutant. A passing focused test is not proof that
  the full suite lets the mutant survive.
- Record complete logs and results immediately, then restore and verify the original bytes before proceeding.
- Run Cargo serially by default; parallelize reading and disjoint test edits rather than competing cold builds.
- Distinguish actual test failures from build/run errors, timeouts, interrupted attempts, and inapplicable replacements.
  Neither an arbitrary nonzero exit code nor a successful zero-test run is meaningful mutation evidence.

## 4. Investigate survivors and prove regressions

First distinguish a real test gap from an equivalent change, an unsupported input, or a harness problem. Explain the
relevant contract. Do not invent brittle assertions to kill equivalent mutants or call an injected defect a production bug.

For a genuine gap, construct a minimal independent expected result. Avoid fixtures where another input masks the error
through deduplication or aggregation. Exercise actual writes, multiple elements, and unaffected neighboring cases when
relevant. Check complete results when counts alone could hide missing or substituted entries.

Add focused tests in the working checkout according to current repository conventions. Do not change production behavior.
If a proposed test fails on the unmutated implementation, investigate the oracle or a possible production defect separately.

Verify test discovery explicitly and run each new or strengthened test family. Require the expected named cases and a
nonzero executed count, not just a successful Cargo exit status. If using `rstest`, avoid the case name `test`; a descriptive
name such as `test_only` avoids the previously observed registration problem. Verify registration regardless of naming.

Run the strengthened unmutated suite and explain any test-count changes. Freeze a separate `after` snapshot with its own
baseline and target directory; never change the `before` tests. Replay the same session's mutations against `after`.
For every claimed closed gap, retain evidence that:

1. The original tests passed with the mutation.
2. The strengthened tests pass on the unmutated implementation.
3. The same mutation now fails on the new or strengthened behavioral assertion.

## 5. Validate and summarize

Run the owning package's full tests, including documentation tests, and the relevant lint checks using current project
commands. Run `git --no-pager diff --check` and review the complete targeted diff. Verify production code and unrelated user
changes were preserved, owned mutations were restored, and no experimental process remains. Do not format, commit, update
dependencies, or clean the user's Cargo cache as part of this audit.

Keep manifests, logs, result records, snapshots, and recovery data in the unique Git-ignored session directory.
Summarize in the final chat response; do not create a separate report file unless explicitly requested. Include the scope,
actual commands and test counts, before/after outcomes, confirmed gaps and regressions, artifact paths, and limitations.
State the score denominator, distinguish controls and proven equivalents, and list infrastructure failures or unfinished
attempts separately. Never present an incomplete campaign as a complete score or a curated score as total coverage.

If no gaps are found, report that without unnecessary edits. Respond in the conversation's language.
