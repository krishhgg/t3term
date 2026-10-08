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
| 1 | https://github.com/krishhgg/t3term/pull/8 | CI, CLI exit-code tests, this record, nightly sync deferred | |
| 2 | https://github.com/krishhgg/t3term/pull/9 | Pinned, Active, Snoozed and Settled shelves in the sidebar | |
| 3 | https://github.com/krishhgg/t3term/pull/10 | The desktop's status words on sidebar cards | |
| 4 | https://github.com/krishhgg/t3term/pull/11 | Build and Plan hidden unless the legacy plan setting turns them on | |
| 5 | https://github.com/krishhgg/t3term/pull/12 | The Plan agent hidden in model options unless the same setting turns it on | |

## Maintenance review PRs

None yet.

## Audits

None yet. The first is due when sprint PR 10 merges.
