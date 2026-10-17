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
| CLI: commands, `--json` and exit codes against a fake server | [cli.md](cli.md) | `links` |

## Not mapped yet

These exist on master but have no file yet, because no scenario serves what they need or nobody has driven them. Each line names the missing precondition.

- **Lineage** in the thread details: needs a world with forks, agents and context transfers. The `links` threads have none, so the section shows no rows.
- **Sending a message, approvals, questions, interrupting and mode changes:** the fake server refuses every orchestration command (`message.dispatch`, `runtime-request.respond`, `run.interrupt`, `thread.runtime-mode.set`, `thread.interaction-mode.set`), so only t3term's refusal path is reachable.
- **The tasks drawer:** needs a `todo_list` plan on the open thread's current run.
- **The worktree setup card:** the fake server answers `subscribeWorktreeSetup` with null only.
- **The thread error and provider status banners:** need a failed run's error, a provider session error, or a provider that isn't ready.
- **The transcript and composer:** reachable in `links` (scrolling, `t`, typing, the model, effort and mode menus) but not yet written up.
