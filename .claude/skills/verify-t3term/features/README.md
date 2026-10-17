# Feature map

One file per feature a user can reach in t3term. Each file has the same four sections, in this order:

- **Sub-features.** What the feature does, in the words and glyphs the screen uses.
- **How to get to it (user POV).** The keys and clicks a person uses.
- **Driving it with t3v.** A recipe that ran against master, with the scenario, size and waits.
- **Gotchas.** What makes a drive go wrong.

Read the file for a feature before you drive it, and fix the file when a drive shows it is wrong. A pull request that changes a key, a label, a click target, a status word or a flow updates the file that names it, or adds a file and lists it here.

| Feature | File | Scenario |
|---|---|---|
| Sidebar: shelves, cards, opening threads, hiding and width | [sidebar.md](sidebar.md) | `links` |
| Environment and connection: the feed dot, the thread watch, reconnects | [connection.md](connection.md) | `links` |
| Thread details: opening, pane or dialog, tabs, the Thread tab's facts | [thread-details.md](thread-details.md) | `links` |
| Linked pull requests: the details' Pull requests tab | [linked-pull-requests.md](linked-pull-requests.md) | `links` |
| Pull request page: what a link's details read from its host | [pull-request-page.md](pull-request-page.md) | `links` |
| Lineage: related threads, forks and agents in the details | [lineage.md](lineage.md) | `lineage` |
| CLI: commands, `launch`, `--json` and exit codes against a fake server | [cli.md](cli.md) | `links` |
| Saved environments: the `environment` commands, `environment status` and the descriptor cap | [environments.md](environments.md) | `links` |
| Transcript: how a thread's replies draw | [transcript.md](transcript.md) | `links` |

## Not mapped yet

These have no file yet, because master doesn't have them, no scenario serves what they need or nobody has driven them. Each line names the missing precondition.

- **The terminal drawer** isn't on master yet (#41). The `terminal` scenario serves it, and its docstring lists 27 cases to drive. Its feature file comes with the drawer, so smoke doesn't run it against a master without one.
- **Sending a message, approvals, questions, interrupting and mode changes:** the fake server refuses every command that comes through `orchestration.dispatchCommand` (`message.dispatch`, `runtime-request.respond`, `run.interrupt`, `thread.runtime-mode.set`, `thread.interaction-mode.set`), so only t3term's refusal path is reachable.
- **The tasks drawer:** needs a `todo_list` plan on the open thread's current run.
- **The worktree setup card:** the fake server answers `subscribeWorktreeSetup` with null only.
- **The thread error and provider status banners:** need a failed run's error, a provider session error, or a provider that isn't ready.
- **The composer and the transcript's keys:** reachable in `links` (scrolling, `t`, typing, the model, effort and mode menus) but not yet written up. transcript.md covers only how replies draw.
