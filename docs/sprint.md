# Parity sprint

The sprint builds the feature groups in [parity.md](parity.md), in that order, one small pull request at a time. It started on 2026-10-08. AGENTS.md says who does what, and only the user merges.

## Audit every ten merged sprint PRs

Sprint PRs are the feature and infrastructure pull requests the sprint merges into master. The CI foundation is sprint PR 1. PR #7, which added the parity plan, came before the sprint and does not count.

When sprint PR 10, 20, 30 and so on merges, feature work stops for an audit:

1. Two models audit the work independently and read-only: Astra (`gpt-6-astra`, reasoningEffort=xhigh) and Claude Fable 5.1 (effort=xhigh). Each checks `.greptile/config.json`, `.greptile/files.json`, the CI workflows and the gaps in the tests against what those ten PRs changed and what Greptile said about them.
2. GPT-6.1 Sol reconciles the two audits.
3. If changes are warranted, Claude Opus 5.5 writes them in a maintenance review PR, Sol verifies it and the user merges it. If not, Sol records a no-op below with the evidence for it.

Maintenance review PRs are listed in their own section. They neither count toward the ten nor restart the count. Nothing runs the audit on a timer. Sol starts it after counting the merged rows below.

## Sprint PRs

Rows are tracked by URL, because GitHub numbers issues and pull requests from one sequence. GitHub is the record of whether a row merged: `gh pr view <url> --json state,mergedAt,mergeCommit`. Each new sprint PR adds its own row and fills in the merge commits of the rows above it.

| # | Pull request | What it does | Merge commit |
|---|---|---|---|
| 1 | https://github.com/krishhgg/t3term/pull/8 | CI, CLI exit-code tests, this record, nightly sync deferred | `1a7a5ae5d140c4e880fcaf19f5a646b13fcc7f92` |
| 2 | https://github.com/krishhgg/t3term/pull/9 | Pinned, Active, Snoozed and Settled shelves in the sidebar | `01b64a9e0f7a161c7275786e30da44c9608bb9c9` |
| 3 | https://github.com/krishhgg/t3term/pull/10 | The desktop's status words on sidebar cards | `cf1ff1ff1fe3b7efa622abce8aa699feb86619b8` |
| 4 | https://github.com/krishhgg/t3term/pull/11 | Build and Plan hidden unless the legacy plan setting turns them on | `ce171674f38ad416165a0b1b32f32c0b56923d0e` |
| 5 | https://github.com/krishhgg/t3term/pull/12 | The Plan agent hidden in model options unless the same setting turns it on | `3fde25a954e3581ec031ab7470a5ffd777b69f7c` |
| 6 | https://github.com/krishhgg/t3term/pull/13 | An opt-in Working shelf for threads busy without the user | `25477c1d01ca5e49fdba4568c80f7790c6be7a4c` |
| 7 | https://github.com/krishhgg/t3term/pull/14 | Proposed plans as cards that collapse when long | `65e3d4351a0f70826c943cc160b7da6d898cd511` |
| 8 | https://github.com/krishhgg/t3term/pull/15 | The running turn's tasks in a drawer above the composer, and checklist steps read in the nightly's shape | `6446163e335ad527f8d6681e8ef4cc5649828cac` |
| 9 | https://github.com/krishhgg/t3term/pull/16 | Context compaction markers with their state, token counts and summary, without handoff markers | `75d23abd5f4d746e950fc09e34017a4cbbc415ad` |
| 10 | https://github.com/krishhgg/t3term/pull/17 | Context handoff markers with their source and target models | `19f5db9942bfa594d86dda463a0f50c60da76e39` |
| 11 | https://github.com/krishhgg/t3term/pull/18 | Hide and show the main sidebar with Ctrl+B or its toggle, remembered between runs | `5defd769dfa89de1ccb734ededab9c811374c3c3` |
| 12 | https://github.com/krishhgg/t3term/pull/19 | Resize the main sidebar by dragging its edge or with [ and ], remembered between runs | `e90aa599784dd9795b62c647ecbcf5dc092b0030` |
| 13 | https://github.com/krishhgg/t3term/pull/20 | File changes in tool output: each edit's operations and a failed edit's error, with a bounded patch preview when a server sends one | `e8f9c864bf4fe0dbfc2da49e268e6fee17680dde` |
| 14 | https://github.com/krishhgg/t3term/pull/21 | A dismissible banner over the conversation for the thread's error, from a failed send or from T3 | `7ad958f357d4a45ba1a37cd884f831968c10463a` |
| 15 | https://github.com/krishhgg/t3term/pull/22 | Live server config in the TUI, so provider and model changes reach the chips and menus without a reread | `cb51f8c619f395c2aefce8fa81e83e18e8275847` |

## Maintenance review PRs

| Pull request | Audit | What it does | Merge commit |
|---|---|---|---|
| https://github.com/krishhgg/t3term/pull/23 | 1 | A CI concurrency group for each push to master, Greptile rule scopes and context brought up to date, and CLI tests for exit codes 7, 1 and 6 | |
| https://github.com/krishhgg/t3term/pull/24 | 1 | Plain CLI output without control characters from T3 text, ids, names and errors, with `--json` unchanged | |

## Audits

### Audit 1: sprint PRs 1 to 10

Sprint PR 10, #17, merged at `19f5db9942bfa594d86dda463a0f50c60da76e39`. The audit covered #8 to #17, from `54a5850` to that commit. Astra and Claude Fable 5.1 each finished an audit on their own, and both recommended maintenance. Sol reconciled them. Sprint PRs 11 to 14, #18 to #21, were already prepared and merged on the user's explicit go-ahead. Sol checked master at `7ad958f` after them. CI passed, its tree matches the tested head of #21, `e15c522`, and all 277 tests, a release install and the no-server smoke test passed. Sprint PR 15, #22, then merged at `cb51f8c` while #23 was a draft, and master's CI passed there too.

| Finding | Found by | Outcome |
|---|---|---|
| `wait` and `send --wait` formatted a handoff they had already printed, and scanned its runs again, on every event | Astra | Fixed by #20, which describes an item only once it has settled and only if it hasn't printed. No change here |
| The master ruleset required only the Greptile Review check | Astra and Fable | Sol changed [ruleset 24698953](https://github.com/krishhgg/t3term/rules/24698953) to require Greptile Review and macOS, and checked the result. Linux stays optional, a branch need not be up to date with master, and the ruleset's other rules, conditions, bypass actors and enforcement are unchanged |
| GitHub cancelled pending master CI runs when a newer merge joined the same concurrency group | Fable | Fixed by #23. 10 of the 14 merge pushes from #8 to #21 were cancelled before any job started, the audit's checkpoint `19f5db9` among them. Sol found no untested merge. Each merged tree matched a PR head that CI had passed, and master at `7ad958f` passed |
| Greptile's protocol-fidelity rule left out main.rs, transcript.rs and src/tui/, which read the nightly's fields. Its idle-cost rule left out main.rs and transcript.rs, where the CLI formats items on each event | Astra and Fable | Fixed by #23. Sol's review of #23 also added models.rs, which reads the providers, models and option descriptors in the server config. It corrected the rule's reconnect wording too. The shell and thread subscriptions resume with `afterSequence`, and `subscribeServerConfig` sends an empty payload and starts from a fresh snapshot on each reconnect, as #22 does |
| `.greptile/files.json` called research/next-steps.md the agreed plan. It is the earlier proposal, and its CLI syntax never shipped | Sol's reconciliation | Fixed by #23. The context now gives README.md as the current commands, docs/parity.md as the pinned plan whose statuses are the baseline, docs/parity-inventory.md for the desktop's source, as Fable proposed, and this file for the process and what merged |
| No CLI test reached exit 7 or 6 through a real wait, or exit 1 through a failed run | Fable | Fixed by #23 |
| Plain CLI output still prints thread titles, messages and tool text from the server with their control characters, in `read`, `projects`, `threads` and the `wait` stream, through `transcript::plain_text` and `describe_plain`. Checklist steps, compaction summaries, handoff endpoints and edit file names are cleaned, and `--json` escapes everything. The gap predates the sprint, and #15 and #16 noted it | Fable | Fixed by [#24](https://github.com/krishhgg/t3term/pull/24), which also cleans `watch`, `requests`, `approve`, `interrupt`, `models`, `settings`, `doctor` and the error report. `--json` keeps every value as T3 sent it |

An older concern that the TUI's App couldn't be tested offline no longer holds, because #21 made offline App tests possible.

The rest needed no change. The tests reach only fake servers on 127.0.0.1 and temporary homes. The workflow pins each action to a commit SHA, reads the repository with `contents: read` and needs no secrets. Rust stays pinned to 1.95.0. The release smoke test checks that `doctor --json` exits 5 with no server and prints exactly one JSON object. macOS stays the only supported platform, with Linux as an optional build. The existing regression tests for the sidebar, settings, scrolling, the tasks drawer and caches were enough. The audit added no benchmarks, test frameworks, media migration or nightly automation.

15 sprint PRs have merged. The next audit is due when sprint PR 20 merges.
