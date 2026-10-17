---
name: maintain-verify-t3term
description: "Periodic pass that keeps the verify-t3term skill and its feature map true to the app: one read-only source reader per feature file, one live pass that drives every feature, and at most one pull request of proven corrections to the skill's own files. Use for /maintain-verify-t3term, \"audit the verify skill\", or after a run of merges that changed what users see."
disable-model-invocation: true
---

# Maintain verify-t3term

The feature map goes stale as soon as the app changes. This pass keeps `.claude/skills/verify-t3term/` honest. Each feature gets read from source and driven live. Not every sentence needs a run.

Adapted from pstack's maintain-verification-skill (github.com/cursor/plugins, MIT) for this repo.

## Outcomes

End with exactly one, and say which:

- **clean.** Every feature was read from source and driven live, and nothing needs changing. No branch, no pull request.
- **changed.** One pull request ships proven corrections to the skill's files.
- **blocked.** Coverage couldn't finish, or a proven fix couldn't ship safely. Say exactly what blocked it.

## What it may edit

Only `.claude/skills/verify-t3term/`: SKILL.md, `features/` and `scripts/`. Never product code under `src/`, `tests/` or anything else. When the map describes behavior the app no longer has, decide which it is. If the app changed on purpose, the map is stale, so fix the map. If the app broke, that's a regression: report it to the user and leave the map as it was, so the next run still catches it.

## The pass

0. **Start from master.** Fetch and work in a fresh worktree from `origin/master`, so the pass checks what users get. `cargo build --locked` there.

1. **Index.** Read `features/README.md` and list the files beside it. Fix entries that are missing, extra, duplicated or dead.

2. **Source wave.** One read-only subagent per feature file, all launched in one message (`subagent_type: "Explore"`). Tell each one not to edit files, and check `git status` afterward. Each reads its feature file and the code behind it (`src/tui/`, `src/main.rs`, `src/client.rs`) and returns four things: how the feature works from the user's side, with the entry points in source; claims in the file the code no longer supports, each with a `file:line`, or "none"; keys, labels or texts the file should use as handles; and one recipe for the live pass.

3. **Reconcile.** Every feature file has an answer. Merge recipes that share a scenario and screen size into as few runs as you can. Spot-check each cited drift in the source yourself; don't re-prove claims that came back clean. Then sweep recent merges for user-facing changes the map lacks: `git log --merges --since=<last pass> --format='%h %s' origin/master`, and the README's key table and "What you get" sections. Name a concrete source path before you call a feature missing.

4. **Live pass.** Required even when the source wave found nothing. You drive. Subagents don't. Start with `scripts/smoke`, which runs every recipe as written against master's build. A recipe it fails is drift or a regression, so triage it in step 5. Then follow verify-t3term's Launch section for what the recipes don't cover: one run per scenario, driven in order. Drive every feature at least once and hold these the whole time:
   - Run `t3v doctor` before the first drive of each run and after any failed drive. When doctor can't see the problem, such as a wedged screen on a healthy process, `t3v restart` or start a new run rather than hope.
   - The evidence captured so far survives every cleanup. Check its directory, don't assume.
   - Nothing a drive started outlives its use. Clean up failed attempts too, with `t3v stop` and `t3v cleanup`, never by process name.

   A doctor failure caused by a stale skill is drift. Fix it within the edit scope and try once more, restarting only what the fix touched, before you call the pass blocked. A feature you can't reach is "verified unreachable" only with the missing precondition named (a scenario the driver lacks, a T3 capability the fake doesn't serve) and the route you tried. If the map doesn't name that precondition, that's drift. Drive any driver fix live again before it ships. Tear down after the last drive, re-proofs included.

5. **Triage.** A wrong or missing description from the user's side is drift: fix it. Behavior that works but the driver can't reach is a driver gap: fix it in `scripts/`, keep the script executable and show its use in SKILL.md. Behavior that is broken is a regression: write it up for the user with the recipe and the captures, and keep it out of this pull request.

6. **Ship or stop.** For **changed**, open one pull request of proven corrections. Read every changed file again first, and give it the decision log AGENTS.md asks for. A pull request needs a verdict from someone other than its author (the verify-pr skill). For **clean** or **blocked**, open nothing. Report the outcome and what the pass covered.

Keep short run notes in `/tmp/t3v-maintain/<date>/`: the features covered, the unreachable ones and why, the drift confirmed and the outcome. Don't commit them.

**Reply:** the outcome, features covered (source and live), drift fixed, regressions found with their recipes, and the pull request if there is one.
