---
name: verify-pr
description: "Give a t3term pull request an independent verdict, PASS, PASS+NOTES or FAIL, by driving its change in the base build and the head build with verify-t3term, then post the verdict on the pull request. Also the landing procedure for a stack: land only the contiguous verified run from the bottom. Use for \"verify PR N\", \"is this ready\", \"land the stack\" or before any merge."
---

# Verify a pull request

Green CI and an approving Greptile review are not a verdict, because neither runs the change the way a user does. A verdict comes from driving the change in the built binary, base against head, and it comes from an agent that didn't write the change. An author tends to check what it meant to build, not what it built.

Adapted from pstack's Shipping playbook and its verify-existing-fix reference (github.com/cursor/plugins, MIT) for this repo.

## Who gives the verdict

- Never the author. In this repo Claude Opus 5.5 writes the code and GPT-6.1 Sol verifies (AGENTS.md). When Sol isn't running the sprint, the orchestrator delegates the verdict to a fresh agent on another model through T3's `delegate_task`, or at least to a fresh subagent that never saw the implementation conversation. The verdict names who produced it.
- The verifier doesn't edit the pull request. It reports what it found. Fixes go back to the author.

## 1. Read the pull request and state its claims

```bash
gh pr view N --json number,title,body,headRefOid,headRefName,baseRefName,files,commits
gh pr diff N
```

Read the description, the decision log and any verification page the pull request adds under `docs/`. Then write each claim in a form a run can disprove: the state, the user's action, what the screen or output shows, and what must not happen. One to five claims. For example: "With the terminal drawer open, Ctrl+Alt+A opens the theme list over it, and the shell receives no keys while the list is open."

A pull request that changes nothing a user sees (CI, docs, a refactor) still gets a verdict. Its claims are "nothing a user sees changes", and the regression sweep in step 5 carries it.

## 2. Find the baseline and build both sides

- Head is `headRefOid`.
- Base is `git merge-base origin/<baseRefName> <head>`. For a pull request stacked on another, that is the parent pull request's branch, so the verdict covers this layer's patch alone.

```bash
T3V=.claude/skills/verify-t3term/scripts/t3v
git fetch origin "pull/N/head"
$T3V build <base sha>     # prints /tmp/t3v-bin/<sha>/t3term
$T3V build <head sha>
```

Record the patch id. It identifies the change independent of the commits, and step 7 uses it to decide whether a verdict still holds:

```bash
git diff <base sha> <head sha> | git patch-id --stable
```

## 3. Drive the claims on base, then on head

Read the matching file under `.claude/skills/verify-t3term/features/` first, and follow verify-t3term's Launch, Doctor, Drive and Evidence sections. Use the same scenario, size, color depth and steps on both sides, each in its own run:

```bash
$T3V start --binary /tmp/t3v-bin/<base>/t3term --evidence /tmp/t3v-verdict/N/base --cols 140 --rows 44
$T3V start --binary /tmp/t3v-bin/<head>/t3term --evidence /tmp/t3v-verdict/N/head --cols 140 --rows 44
```

- **A bug fix.** The symptom must show on base twice, from a reset state each time. If it doesn't, there is no baseline and the claim is inconclusive. Don't call the fix verified. Then the same steps on head, twice: the broken state is gone and the expected state is there.
- **A feature.** Base shows the feature absent or in its old form. Head shows it, through every entry point the pull request names: keys, clicks and the CLI.
- Capture before and after each action that matters. A final screen alone doesn't prove the action caused it.
- Server controls arrange a precondition, such as a link's state or a dropped socket. They never stand in for the user's action.

## 4. Check the rules and the counters on both runs

```bash
$T3V check --run <base run>; $T3V check --run <head run>
$T3V status --json --run <base run> > base-status.json
$T3V status --json --run <head run> > head-status.json
```

`check` must pass on head. Compare `rpcRequestsByTag` between the two runs for the same steps. Head sending a request base didn't send, or more of one, is a finding unless the pull request says why. Idle cost is a requirement in this repo (`.greptile/config.json`, rule `idle-cost`).

## 5. Sweep for regressions

First run every recipe on both sides. Each side runs its own checkout's recipes against its own build, so a pull request that changes a recipe on purpose is held to the new one:

```bash
git worktree add /tmp/t3v-verdict/N/base-tree <base sha>
git worktree add /tmp/t3v-verdict/N/head-tree <head sha>
/tmp/t3v-verdict/N/base-tree/.claude/skills/verify-t3term/scripts/smoke --binary /tmp/t3v-bin/<base>/t3term --evidence /tmp/t3v-verdict/N/smoke-base
/tmp/t3v-verdict/N/head-tree/.claude/skills/verify-t3term/scripts/smoke --binary /tmp/t3v-bin/<head>/t3term --evidence /tmp/t3v-verdict/N/smoke-head
git worktree remove /tmp/t3v-verdict/N/base-tree; git worktree remove /tmp/t3v-verdict/N/head-tree
```

A recipe that passes on base and fails on head is a regression unless the pull request changed it. A recipe the pull request changed must pass on head. A side that predates smoke has no recipes. Then run master's smoke and recipes against both builds. A head failure in a feature the pull request changes on purpose means the recipe needs updating, so drive that feature by hand and ask the author to update its feature file. Any other head failure that base passes is a regression.

Then list the features the diff can reach: map each changed file under `src/` to the feature files that drive it, and add the feature the claims name. Drive each of those features' recipes on head. For each one that the pull request doesn't mean to change, capture the same screens on base and head and compare them:

```bash
diff /tmp/t3v-verdict/N/base/<name>.txt /tmp/t3v-verdict/N/head/<name>.txt
```

Any difference the pull request doesn't explain is a finding. For a small diff you don't trust, run the blast-radius skill and prove the one fact the change is safe because of.

## 6. Check the tests and the review

- CI on the head SHA: `gh pr checks N`. macOS must pass.
- Greptile: every finding fixed or answered. A thread counts as answered when someone other than Greptile replied or it is resolved.
- A bug fix carries a test that fails with the fix reverted. In a scratch worktree at head, revert the non-test part of the fix, run that test and see it fail, then delete the worktree. A test that still passes proves nothing (the tdd skill).
- The pull request adds or updates its feature file under `.claude/skills/verify-t3term/features/` when it adds or changes something a user sees.

## 7. Decide

- **PASS.** Every claim held on head and the baseline showed the difference. The sweep found no regression, `check` passed, CI is green and Greptile is answered.
- **PASS+NOTES.** The same, with notes that don't block: a missing feature file, a cosmetic difference, an edge case the pull request's Known limits already names.
- **FAIL.** A claim didn't hold, the sweep found a regression, a rule broke, a regression test passes with its fix reverted, or CI is red.

A claim that couldn't run is inconclusive. Say which half couldn't run and why. A pull request with an inconclusive claim has no verdict yet, so it can't land.

## 8. Post the verdict

Write it to a file and post it as one comment. Start with the marker line so later passes can find it.

```markdown
<!-- t3v-verdict -->
**PASS** for head `<head sha>`, patch id `<patch id>`.
Verifier: <agent and model>, not the author. Base `<base sha>`. Scenario `links`, 140x44, truecolor.

| Claim | Base | Head |
| --- | --- | --- |
| Ctrl+Alt+A opens the theme list over the drawer | list doesn't exist | opens, shell got 0 keys |

Regression sweep: smoke 5 of 5 on base and head. Sidebar, thread details, linked pull requests driven: no difference outside the change.
Rules: `check` passed on both runs. Requests: the same on both runs.
CI: macOS passed. Greptile: 2 threads, both answered.
Notes: none.
```

```bash
gh pr comment N --body-file /tmp/t3v-verdict/N/verdict.md
```

Images go in the comment only from renders of captures (`t3v render`). Push them as a parentless commit to a `media/*` branch and link their blobs by SHA, so they stay out of the code history.

## Landing a stack

Only when the user has said an agent may merge. Otherwise stop at the verdict and hand the pull request to the user.

1. **Find the verified run.** Walk up the stack from the lowest unmerged pull request and stop at the first one without a PASS or PASS+NOTES verdict that still holds. The pull requests below it are the run. A verified pull request above an unverified one waits, because merging it would pull the unverified code in underneath.
2. **Check that each verdict still holds.** Compute the patch id again. If it equals the verdict's, the verdict holds. If it differs only in tests, docs or comments, the drive results stand, but the checks and the review run again. Otherwise verify again.
3. **Check it against today's master.** When master moved since the verdict, build GitHub's test merge (`git fetch origin pull/N/merge`, then `t3v build FETCH_HEAD`) and run the verdict's head steps on it. Two pull requests can each pass alone and break together. A difference from the verdict's head captures is a finding.
4. **Merge the lowest first.** `gh pr merge N --merge`. Then read it back: `gh pr view N --json state,mergedAt,baseRefName` must say MERGED with base `master`. A command that reports success can still leave the pull request open.
5. **Retarget before deleting.** Run `gh pr edit M --base master` on every pull request stacked on the merged one before the merged branch goes. Deleting a base branch closes the pull requests stacked on it. Then confirm M's patch id didn't change, and wait for its checks.
6. **Stop at the ceiling.** Going further needs new verdicts.

**Reply:** each pull request's verdict and who gave it, what landed and how you confirmed it, and what the next unverified pull request needs.
