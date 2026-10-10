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

## Maintenance review PRs

None yet.

## Audits

None yet. The first is due when sprint PR 10 merges.
