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
| 16 | https://github.com/krishhgg/t3term/pull/25 | A banner over the conversation, above the thread's error, when the provider the composer would send with isn't ready or runs a version T3 marks broken or unsupported | `4d5a295c98d7dc9f7bbbf4c1a3d3641d3f5677d3` |
| 17 | https://github.com/krishhgg/t3term/pull/26 | Worktree setup stream foundation: a client watcher for one thread's `subscribeWorktreeSetup`, for the setup card that comes next. Nothing in the TUI or CLI shows a setup yet | `2b5e75f5ff2a1f81da92067442755c496989ffc3` |
| 18 | https://github.com/krishhgg/t3term/pull/27 | The open thread's worktree setup as a read-only card under its first prompt: stage progress, the setup script's last output and Details. No Cancel, Work locally, terminal or worktree preparation | `194943b5764256bbae6bc9c7a1d34948d33d9ddc` |
| 19 | https://github.com/krishhgg/t3term/pull/28 | The environment's label, host and port under the sidebar's wordmark, and a dot that follows the sidebar's own feed, not the open thread's watch. The breadcrumb's row shows both while the sidebar is hidden. No environment list or selector | `f210cd8ac137872b95388ac4e11c097d8b0cf159` |
| 20 | https://github.com/krishhgg/t3term/pull/29 | The open thread's project, workspace, recorded branch and environment, read-only, in a pane beside the conversation or a dialog on a narrow screen, opened with Alt+B or a button in the title row. No Git status, actions, scripts or tabbed right panel | `739cead6795dd0a80151d657c48721c109dea86f` |

## Maintenance review PRs

| Pull request | Audit | What it does | Merge commit |
|---|---|---|---|
| https://github.com/krishhgg/t3term/pull/23 | 1 | A CI concurrency group for each push to master, Greptile rule scopes and context brought up to date, and CLI tests for exit codes 7, 1 and 6 | `cf17b7ac16ccc00272e2c08ce9094e3bf261bc5c` |
| https://github.com/krishhgg/t3term/pull/24 | 1 | Plain CLI output without control characters from T3 text, ids, names and errors, with `--json` unchanged | `1efb6a3052a5d3448fdd7fbb8dbf2bac694f0b46` |
| https://github.com/krishhgg/t3term/pull/30 | 2 | Greptile context for the worktree setup stream, the TUI's real wake rules and the verification pages, and tests of one shared connection drop, a crowded thread and control characters across the whole screen | |

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

### Audit 2: sprint PRs 11 to 20

Sprint PR 20, #29, merged at `739cead6795dd0a80151d657c48721c109dea86f`. The audit covered #18 to #22 and #25 to #29, from `19f5db9` to that commit, and took #23 and #24 as shipped. Astra and Claude Fable 5.1 each finished an audit on their own. Neither found a confirmed application bug, and both recommended a small maintenance PR. Sol reconciled them into #30.

Every final PR head in the block passed macOS, the optional Linux job and Greptile Review. Greptile left four inline findings, and the merged source has the fix for each:

| Pull request | Greptile's finding | Fix in the merged source |
|---|---|---|
| [#20](https://github.com/krishhgg/t3term/pull/20#discussion_r4233764369) | The CLI built file-change previews it never printed, and described printed edits again on every event | `describe_plain` in transcript.rs, called only for an item that has settled and hasn't printed |
| [#25](https://github.com/krishhgg/t3term/pull/25#discussion_r4252516592) | A replaced provider notice kept the old notice's scroll keys for one frame | Keys and the wheel check the banner's current expanded state before they scroll it |
| [#27](https://github.com/krishhgg/t3term/pull/27#discussion_r4253567356) | A shell update could open a setup watch again after the thread's watch closed | `follow_setup` returns early while the thread's watch is closed |
| [#28](https://github.com/krishhgg/t3term/pull/28#discussion_r4254647770) | A five-digit port lost a digit at 80 columns | The line keeps the whole port or leaves the address out, as `every_width_keeps_the_whole_port` checks |

The other six PRs had no inline findings. Greptile reviewed #18, #19, #21, #22 and #26 only at their final heads, and first checked #29 at `e1df4c0`, which already bounded the blank-path scan Sol caught. The fixes Sol asked for in those PRs came before Greptile saw them, so the audit can't tell whether the rules would have caught them, and it doesn't count them as misses of the config.

| Finding | Found by | Outcome |
|---|---|---|
| The instructions and the protocol-fidelity rule didn't describe `subscribeWorktreeSetup`, which #26 added | Astra | Fixed by #30. They now give its `{threadId}` payload, its items of a whole setup or null and its resubscribe with no cursor, and say a setup's sequence can start over after a null or a reconnect. The rule doesn't ask the watcher to drop repeated items |
| idle-cost said the TUI draws at most 30 times a second and only after input or an event. A layout change draws at once, and a tick or a snooze can wake it. The rule also didn't name the repeated work Sol caught in #21 and #29 | Astra | Fixed by #30. The rule now gives the 33 ms wait, the immediate layout frame and the two timers. It flags building an unchanged banner or the details again on an unrelated update, and width or cleaning work over huge text before it is cut. Its scope is unchanged |
| measured-claims couldn't see the verification pages, which count requests and output bytes | Astra and Fable | Fixed by #30 with `docs/*-verification.md`. Fable proposed `docs/**`. Sol kept the narrower pattern so the rule doesn't reach other numbers in docs |
| platform-gating called macOS the only tested platform, though CI runs an optional Linux build and test job | Astra | Fixed by #30. It now calls macOS the only supported platform and names the optional Linux job. The cfg gate and Keychain fallback rules stay |
| The parity inventory's context reached only src/tui/ and transcript.rs, though the inventory cites fields and transports in client.rs, projection.rs, models.rs and tests/fake_server.rs | Astra | Fixed by #30, which adds those four. It adds no entry for each verification page, as Fable advised |
| No test dropped the one connection the shell, thread, config and setup watches share | Astra and Fable | Fixed by #30 with `shared_watches_come_back_on_one_new_connection_after_a_drop` in client.rs, through the real `Client::rpc`. A gap in coverage, not a known defect |
| No App test put a running setup, a provider notice, a thread error, an approval, the tasks drawer, the model menu and the details on one screen | Fable | Fixed by #30 with `a_crowded_thread_stays_on_screen_and_hands_each_key_to_the_right_place`, at 80x24 and 40x12 |
| The hostile-field test checked only the details' cells, while raw titles reach the sidebar's card and the breadcrumb through `fit` | Astra and Fable | #30 extends `hostile_and_huge_fields_stay_inside_the_details_at_every_size` to every cell, with the sidebar shown and hidden. ratatui-core 0.1.2 drops any grapheme holding a control character before it reaches a cell, so the test guards that filter. It isn't an injection fix. Printable fragments such as `]52;c;` still print. Cleaning in `fit` and `clip`, Fable's optional follow-up, needs its own PR |
| Row 20 had no merge commit, and this record still said 19 sprint PRs had merged | Astra and Fable | Bookkeeping, fixed by #30 |

CI and the ruleset need no change. The workflow reads the repository with `contents: read`, checks out with `persist-credentials: false`, pins each action to a commit SHA and keeps Rust at 1.95.0. Pull requests only restore the cache, and master pushes save it. The pushes that merged #18, #19 and #20 were cancelled before #23's concurrency fix, as audit 1 records. Every master run from `7ad958f` on finished and passed, the latest being run 38422824491 at `739cead`. [Ruleset 24698953](https://github.com/krishhgg/t3term/rules/24698953) is active on master and requires Greptile Review and macOS. Linux stays optional, a branch need not be up to date, no one can bypass it, and it blocks deletion and force pushes. It doesn't require review threads to be resolved, so that rests on AGENTS.md. All four of this block's finding threads are resolved.

#30's checks await Sol's run, and their results go here once Sol has them.

20 sprint PRs have merged. The next audit is due when sprint PR 30 merges.
