---
name: verify-this
description: "Prove or disprove one claim about t3term with fresh evidence: state it so a run can disprove it, capture a baseline and a treatment the same way, compare the raw artifacts and return VERIFIED, NOT VERIFIED or INCONCLUSIVE. Use for \"verify this\", \"did this fix it\", \"prove it works\" or \"show me the evidence\" about one behavior. For a whole pull request, use verify-pr."
---

# Verify this

Verification isn't a recap. It proves or disproves one specific claim with evidence someone else can rerun.

Adapted from cursor-team-kit's verify-this skill (github.com/cursor/plugins, MIT) for this repo.

Don't use it for claims nothing can measure, like "the code is cleaner". Ask for a claim a run can check first.

## Steps

1. **State the claim so a run can disprove it.** The condition, what you observe and the threshold. "After `t3v reconnect`, the sidebar's feed dot is green again within 5 s and the open thread's draft is unchanged."
2. **Pick the smallest surface that can disprove it.**
   - The TUI: a verify-t3term drive, with captures.
   - The CLI: the command run against a `t3v` fake server (`t3v env` prints the line), with stdout, stderr and the exit code.
   - The RPC client: a test in `tests/fake_server.rs`.
   - One function's behavior: a focused `cargo test`.
   - CPU, memory or energy: `benchmarks/`, vetted with the benchmark-checklist skill.
3. **Capture a baseline** from the old state: the merge base, the parent commit or the build that shows the bug. `t3v build <rev>` builds any commit without touching your checkout.
4. **Capture the treatment** from the changed state with the same command, scenario, size, color depth and steps.
5. **Compare the raw artifacts:** screen captures (`diff` the `.txt` files), rendered PNGs, `t3v status --json` counters, CLI output and exit codes, test output.
6. **Return exactly one verdict.**

## Verdicts

- **VERIFIED.** Baseline and treatment differ the way the claim predicts, by its threshold, with nothing else that explains it.
- **NOT VERIFIED.** The behavior didn't change, changed the wrong way or missed the threshold.
- **INCONCLUSIVE.** No valid baseline, a noisy signal, a failed run, or a difference between the two setups that spoils the comparison.

Don't soften a negative result. A clear NOT VERIFIED is useful.

## Where artifacts go

`/tmp/verify-this/<claim-slug>/` with `claim.md`, `baseline/`, `treatment/`, `diff/` and `verdict.md`. `t3v start --evidence` points a run's captures there. Keep them out of the repository.

## Reply

```text
VERIFIED | NOT VERIFIED | INCONCLUSIVE
Claim: <the claim as a run can disprove it>

Evidence:
<artifact>: baseline=<...>, treatment=<...>, difference=<...>, threshold=<...>

Reasoning:
<one short paragraph naming the evidence and anything that could confound it>
```
