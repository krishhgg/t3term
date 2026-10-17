---
name: tdd
description: "Fix a t3term bug test first: a focused test that fails before the fix, passes after it, and fails again when the fix is reverted. Also the bar every test here meets: it checks what a user, caller or T3 can observe, not how the code does it. Use when fixing a bug with a cheap local test, when asked for a regression test, or when reviewing whether a test proves anything."
---

# Test first

When a bug has a clear, cheap test path, make the broken behavior fail in a test before changing the code. The goal is a focused regression test that fails before the fix, passes after it and fails again with the fix reverted.

Adapted from pstack's tdd skill and its test-behavior-not-implementation principle (github.com/cursor/plugins, MIT) for this repo.

## Where tests go here

- **Unit and TUI tests** sit in each module's `#[cfg(test)]` block, or under `src/tui/tests/` for the TUI's larger ones. TUI tests draw frames on ratatui's `TestBackend` with a client that never connects, and check cells, the cursor and what the App asked the server for.
- **`tests/fake_server.rs`** runs the real RPC client against a fake Effect RPC server: reconnects, acks, cursors, refusals.
- **`tests/cli.rs`** runs the built binary with a temporary HOME and T3CODE_HOME against fake servers on 127.0.0.1: output, `--json`, exit codes.
- No test reads the real `~/.t3`, uses the Keychain or reaches a real T3 server (AGENTS.md). Copy the setup of a test near the one you're adding.

## Steps

1. **Understand the bug.** The intended behavior, the current behavior, the path that differs and the smallest observable reproduction. A verify-t3term drive often finds it. The test should fail at the same point that drive shows.
2. **Choose the narrowest test that can see it.** The closest existing kind of test for that code. If none is practical, don't build new test machinery just for this. Use the closest executable check instead (see below).
3. **Write the failing test first.** The smallest test that would have caught the bug. Derive the expected value from the requirement (the README, T3's source, the desktop's behavior), not from a run of the code.
4. **Run it before fixing.** `cargo test --locked <name>`. It must fail for the reason you expect. If it passes, or fails for another reason, fix the test before touching the code.
5. **Fix the bug** with the smallest change that gives the intended behavior and keeps nearby contracts.
6. **Run it again.** It passes. Then run `cargo test --locked` for everything.
7. **Revert the fix and run it once more.** It must fail. A test that still passes with the fix gone proves nothing. Put the fix back.
8. **Commit the test first.** Commit the failing test, then the fix on top, so the history reads as the argument: red, then green.

## What a test checks

A test is a claim about what the code does for its users: a person at the TUI, a script reading `--json`, T3 receiving a request. Judge each test against two imagined edits:

- **Break the behavior.** Return a wrong value, skip an effect, do it twice. If the test still passes, it proves nothing. The usual causes are an assertion looser than the contract, an expected value computed by the code under test, and an assertion on data the test built itself.
- **Change the internals.** Keep every observable result and restructure the code underneath. If the test now fails, it's pinned to the implementation and will need rewriting on every refactor without catching a bug.

Keep tests that catch the first edit and survive the second. In this repo the observable results are the cells and cursor a frame draws, the requests and their payloads the fake server receives, what the CLI prints and its exit code, and the files it writes in the temporary HOME. A call to a private helper isn't one.

## When a failing test isn't practical

Use the closest executable check instead: a verify-t3term drive with captures before and after, a `tests/cli.rs`-style run, a script that replays a recorded event stream. Prefer no new test over a bad one: one that mostly tests mocks, pins current internals, depends on timing, or would be deleted right after.

## Guardrails

- Don't change a test to match a wrong implementation.
- Don't loosen an assertion unless the intended behavior changed, and say why.
- Keep the test on the bug. No unrelated fixture churn.
- If the bug is flaky, make the test deterministic and name the signal it locks down.

## Report

- The failing-before test and the failure it produced.
- The passing-after run, and the run with the fix reverted that failed again.
- If you couldn't show failing-before, why, and the closest check you used instead.
