# Parity with the T3 Code nightly

This lists everything the T3 Code desktop app can do next to what t3term does today, so we can pick what to build and in what order. It's pinned to nightly `v0.0.46-nightly.20261007.2787`, commit `f570bd2`.

It comes from reading the source, not from clicking around the app. Codex, running gpt-6.1-sol at extra-high reasoning, read the T3 Code source and wrote the full inventory in [parity-inventory.md](parity-inventory.md), with 1,430 citations down to the file and line. Claude then checked it. Every citation resolves, the command list was counted again by hand, and every row marked "partial" was rechecked against t3term's code.

## Where t3term is

The desktop app has 302 features. t3term does 11 of them fully and 30 partly, and is missing the other 261.

Most of that can be built. 244 features port to a terminal as they are. 42 can be done with something lost, like a background blur or a second font, which a terminal can't do. 16 can't be done in a terminal at all, and those are mostly the embedded browser, the device simulator and rendered HTML. That count assumes a plain terminal, though. Ghostty, kitty and iTerm2 can draw images, so image previews could move from "can't" to "approximate" in those.

The commands tell the same story. A client can send T3's server 50 kinds of command, and t3term sends 6: send a message, interrupt, answer an approval or a question, and set the model, the permission mode and Build or Plan. The desktop app sends 30 of the other 44. The last 14 come from the server, from agents through MCP, or from older clients, so they aren't something t3term is missing. Subagents are one of those. An agent starts them through MCP, and the desktop shows them in the Lineage panel as threads you can open, which is in the plan below. Past commands, the desktop also calls whole groups of API that t3term never touches: projects, git, diffs, terminals, the browser preview, PRs, scheduled tasks, providers and settings.

All of orchestrator V2 is covered. Every command type the desktop sends maps to a row in the plan below, and so does every orchestration API call one of its screens makes. The commands with no row are sent by the server or by agents, such as the bookkeeping for when a subagent's result wakes its parent, so there's nothing for a client to draw.

## Why it looks older than the nightly

The nightly changed its layout, and t3term still follows the older one. These are the visible differences, and the first two groups below cover all of them except themes.

- **Sidebar.** The nightly's sidebar is one inbox split into shelves: drafts on top, then Pinned, Active, Snoozed and Settled. Each row shows one status word, picked in this order: Approval, Input, Working, Waiting, Limited, Failed, Woke, Done. Working reads Goal while the agent works toward a set goal. Rows also carry a pin, a PR badge and a terminal badge. t3term shows Input or Approval, a spinner with a timer, Failed, Queued or the age, plus one toggle for settled threads.
- **Header.** The nightly shows the project, a slash, then the thread title, and you can rename the thread from there.
- **Messages.** Replies are headed "T3 Code". After the last message, one quiet line says the thread is settled, snoozed or just woke, with a single action next to it.
- **Composer.** The nightly took the Build/Plan toggle and the context meter out of the default composer, and both now sit behind legacy settings. t3term still shows the Plan chip. In the nightly, queued messages and the approval or question drawer stack on top of the composer. The model controls sit at the bottom left, send is on the right, and a strip under the composer shows the host, workspace and branch.
- **Thread details.** A card next to the conversation shows the worktree, an Open in your editor button, project scripts, the branch, linked PRs with their checks, uncommitted changes, automations, and Lineage, the list of parent and subagent threads.
- **Empty draft.** A new thread opens on "What should we build in {project}?" above the composer.
- **Themes.** t3term has one hardcoded dark palette. The nightly has system, light and dark, plus built-in themes called t3-chat, grove, ocean, ember and iris. t3term's dark colors already match the nightly's base dark theme closely, so this one is about choice, not about wrong colors.

## The plan, in my recommended order

Each group is one PR, or a short series if it's big. Sizes are rough estimates from reading the source, not measured effort: S is one action or control, M is a stateful flow or view, L is a whole subsystem. Every group except Notifications changes what you see, so you merge those after looking at screenshots.

| Rank | Group | Features | Work today | Partly | Missing | Can't in a terminal | S / M / L |
|---|---|---|---|---|---|---|---|
| 1 | [Look like the nightly](#1-look-like-the-nightly) | 19 | 5 | 8 | 6 | 0 | 9 / 9 / 1 |
| 2 | [Thread details pane](#2-thread-details-pane) | 8 | 0 | 3 | 5 | 0 | 2 / 6 / 0 |
| 3 | [Start threads](#3-start-threads) | 12 | 0 | 0 | 12 | 0 | 6 / 4 / 2 |
| 4 | [Organize the inbox](#4-organize-the-inbox) | 16 | 0 | 2 | 14 | 0 | 10 / 6 / 0 |
| 5 | [Queue, steering and requests](#5-queue-steering-and-requests) | 23 | 2 | 8 | 13 | 0 | 12 / 11 / 0 |
| 6 | [Notifications](#6-notifications) | 4 | 0 | 0 | 4 | 0 | 2 / 2 / 0 |
| 7 | [History, diffs and rewind](#7-history-diffs-and-rewind) | 20 | 0 | 2 | 18 | 0 | 5 / 14 / 1 |
| 8 | [Machines](#8-machines) | 7 | 0 | 1 | 6 | 0 | 0 / 1 / 6 |
| 9 | [Composer](#9-composer) | 24 | 3 | 2 | 19 | 1 | 7 / 15 / 2 |
| 10 | [Search, palette and navigation](#10-search-palette-and-navigation) | 4 | 0 | 1 | 3 | 0 | 1 / 3 / 0 |
| 11 | [Git and workspaces](#11-git-and-workspaces) | 20 | 0 | 0 | 20 | 0 | 5 / 12 / 3 |
| 12 | [Pull requests](#12-pull-requests) | 21 | 0 | 0 | 21 | 0 | 8 / 9 / 4 |
| 13 | [Projects and files](#13-projects-and-files) | 22 | 1 | 1 | 20 | 2 | 4 / 13 / 5 |
| 14 | [Settings page](#14-settings-page) | 36 | 0 | 0 | 36 | 4 | 23 / 10 / 3 |
| 15 | [Embedded terminal](#15-embedded-terminal) | 9 | 0 | 0 | 9 | 0 | 2 / 5 / 2 |
| 16 | [Providers, automations and usage](#16-providers-automations-and-usage) | 20 | 0 | 2 | 18 | 0 | 3 / 5 / 12 |
| 17 | [Browser, capture, devices and rich media](#17-browser-capture-devices-and-rich-media) | 37 | 0 | 0 | 37 | 9 | 7 / 15 / 15 |

### Why this order

- **Look like the nightly** goes first because it's the first thing you noticed, and it's mostly redrawing data t3term already gets. Everything after it then lands in the new layout instead of the old one.
- **Thread details pane** comes right after, because it's the other half of the nightly's thread view and it's how you'll reach subagents.
- **Start threads** is the gap that sends you back to the desktop app every day.
- **Organize the inbox** is how the nightly keeps the sidebar short. Without settle, snooze and pin, the list only grows.
- **Queue, steering and requests** finishes the conversation itself, so you never need the app while a run is going.
- **Notifications** is small, and it's what lets you close the desktop app and still find out when a thread needs you. That matters for the battery point, since the app only stops using power once it's closed.
- **History, diffs and rewind** lets you review what an agent changed without opening the app.
- **Machines** is big, so it sits in the middle. The connection code should still be written for several servers from the first PR on, so this group doesn't mean rewriting the groups before it.
- **Composer**, **Search, palette and navigation**, **Git and workspaces**, **Pull requests** and **Projects and files** are whole features you reach for less often.
- **Settings page** and **Providers, automations and usage** are mostly things you set once. Ranking them low doesn't mean cutting them.
- **Embedded terminal** sits near the bottom because you're already in a terminal, and a tmux pane does the same job.
- **Browser, capture, devices and rich media** is last because most of it can't run in a terminal. The part that can is opening things in your real browser and showing a text summary.

## Keeping up with new nightlies

A new nightly changes two kinds of things, and only one of them needs work in t3term.

**Server and agent changes reach t3term on their own.** t3term doesn't bundle T3. It talks to whatever server your desktop app or `t3` is running, so when a nightly fixes a provider, changes how agents run or adds a model, t3term gets it as soon as your server updates. The model picker reads the model list from the server, so new models show up without a t3term release.

**Anything a client has to draw or send needs code.** A new command, API call, setting, shortcut or kind of timeline row only shows up in t3term once someone builds it. Nothing can write that UI unattended and get it right, so the goal is to never miss one:

1. A file called `T3CODE_NIGHTLY` records the nightly t3term was last checked against.
2. A GitHub Action runs once a day. When pingdotgg/t3code has a nightly tag newer than that file, it fetches the command list, the API list, the settings and the default shortcuts at both tags, and opens an issue listing what was added and what was removed.
3. Each issue becomes a PR that builds what's new and moves `T3CODE_NIGHTLY` forward. To get closer to automatic, a daily T3 scheduled task can pick up each new issue and open that PR, and you review it like any other UI change.
4. `t3term doctor` and the TUI's status line compare your server's version with `T3CODE_NIGHTLY` and say when the server is newer. The existing check stays as it is: if the server moves past orchestration protocol 2, t3term refuses to connect and tells you to update, instead of guessing.

## Every feature, by group

The t3term column says what works at commit `a244ccd`. The terminal column says whether the feature can be built in a TUI: **port** means it can, **approximate** names what gets lost, and **can't** means it needs a real graphical surface. Each row's source citations are in [parity-inventory.md](parity-inventory.md); search it for the feature name.

### 1. Look like the nightly

The sidebar shelves and status words, the header breadcrumb, the status line after the last message, the composer stack and context strip, the thread-details card, and hiding the two things the nightly hid, the Plan chip and the context meter. Most of this redraws data t3term already receives.

| Feature | Where it is in the app | t3term today | In a terminal | Size |
|---|---|---|---|---|
| View existing thread | Sidebar, CLI-equivalent history view | yes | port | S |
| Live transcript, reconnect, and replay | Open thread | yes | port | S |
| Sidebar live thread/project list | Main navigation | yes | port | S |
| Pinned/Active/Snoozed/Settled shelves | Default sidebar | partial: Active/Settled only, sorted by update time | port | M |
| Working/monitoring shelf | Sidebar; opt-in beta setting | no | port | M |
| Build/Plan interaction mode | Composer/slash commands only with legacy Plan setting | yes; plan control is visible in t3term, unlike default nightly | port | S |
| Context window meter | Composer; hidden unless legacy setting enabled | no | port | S |
| Proposed plan reading and expansion | Timeline plan card | partial: displays plan text, no card interactions | port | S |
| Reasoning/thinking and tool activity | Timeline work log | yes for textual reasoning, command output, searches, task summaries | port | M |
| Task checklist/progress drawer | Composer shoulder tab | partial: todo text only | port | M |
| Markdown assistant prose/code/tables | Timeline | partial: terminal Markdown styling, no browser layout or interactive links | approximate; text wrapping/tables rather than HTML typography | M |
| Error and provider status banners | Top overlay; composer stack | partial: error text/status line, no stacked banners | port | M |
| Context compaction/handoff markers | Timeline event rows | partial: generic system/event title only | port | S |
| Changed-file summary in tool output | Timeline file-change rows | partial: file path and +/- statistics, no patch body | port | S |
| Worktree setup progress, open setup terminal | Timeline worktree setup card | no | port | M |
| Resizable/hideable main sidebar | Workspace left edge, Mod+B | no equivalent resize/hide; fixed quarter-width clamped sidebar | port | M |
| Right-panel surface tabs, add/close/reopen | Workspace right side | no | port for textual surfaces; browser/device tabs external | L |
| Maximize right panel and adapt to narrow sheet | Panel controls | no | port for text panels | M |
| Connection-status indicator | Sidebar/environment chrome | partial: terminal status/error line; no desktop dot/host chrome | port | S |

### 2. Thread details pane

The card from the desktop's thread view: the worktree, open in your editor, project scripts, the branch, uncommitted changes, and Lineage. Lineage lists the parent thread and every subagent as a thread you can open or stop. The TUI hides subagent threads completely today, so this is also how you'll reach them. The card's commit and PR rows fill in as the Git and Pull requests groups land.

| Feature | Where it is in the app | t3term today | In a terminal | Size |
|---|---|---|---|---|
| Thread-details card/popover density adaptation | Conversation side, details toggle | no | approximate; details pane/dialog based on available columns | M |
| View current branch/worktree binding | Composer context strip, thread details, sidebar branch | partial: branch text only, no worktree picker/status | port | S |
| Git status and changed files | Details Version Control / Git action popover | no | port | M |
| Open workspace in preferred external editor | Details Open in picker; Mod+O | no | port as external launcher | S |
| Run configured project action/script | Details Workspace / action button | no | port | M |
| Parent, fork, and delegated-thread navigation | Thread details Lineage | partial: CLI can include child threads; TUI filters all parented threads, including forks | port | M |
| Managed delegated-task status | Lineage running and previous agents, activity bar | partial: textual tool/event summaries, no managed task panel | port | M |
| Stop delegated work | Lineage stop action, main Stop | no for per-child controls | port | M |

### 3. Start threads

New thread in a project's main checkout, in a new worktree, or with no project. The draft screen, the background send, sending one draft to several models, and the worktree setup card with cancel, retry and work locally.

| Feature | Where it is in the app | t3term today | In a terminal | Size |
|---|---|---|---|---|
| Launch thread in a project's main checkout | New thread, command palette, draft composer workspace choice | no | port | M |
| Launch thread in a new isolated worktree | Draft workspace/branch controls | no | port | L |
| Start without selecting a project | Palette, new-thread shortcut, no-project hero | no | port | M |
| Send new thread into background | Draft composer alternate send shortcut | no | port | S |
| Send and immediately open another draft | Composer alternate action | no | port | S |
| Send one draft to multiple models in separate worktrees | Draft model picker; Shift-select/Shift+Enter adds model | no | port | L |
| Reuse existing/previous worktree | Composer workspace/previous-worktree controls | no | port | M |
| Cancel worktree preparation | Setup card Cancel | no | port | S |
| Retry failed preparation | Setup failure/queue action | no | port | S |
| Work locally instead of waiting for worktree setup | Setup card | no | port | M |
| Default new-thread workspace mode | General/Project defaults | no | port | S |
| Start worktree from origin | General/draft controls | no | port | S |

### 4. Organize the inbox

Settle, snooze, pin and reorder, mark unread, rename and regenerate titles, archive, delete, auto-settle per thread, multi-select, undo, the project filter and copying IDs and paths. This is most of the 30 commands the desktop sends and t3term doesn't.

| Feature | Where it is in the app | t3term today | In a terminal | Size |
|---|---|---|---|---|
| Rename thread inline | Header double-click/title menu, row context menu | no | port | S |
| Regenerate thread title | Thread menu, multi-select menu | no | port | S |
| Settle/unsettle | Thread menu, shortcut | partial: displays/toggles existing settled rows only | port | S |
| Snooze with presets or custom date | Thread menu | no | port | M |
| Pin/unpin | Row marker, menu, shortcut | no | port | S |
| Reorder pinned threads | Sidebar drag/drop | no | port; keyboard move action can replace drag | M |
| Reorder active inbox | Sidebar drag/drop; disabled ordering effect while Working shelf enabled | no | port | M |
| Seen/unread state synchronized across clients | Opening thread, Mark unread menu | no | port | S |
| Per-thread automatic settle override | Thread menu | no | port | S |
| Archive/unarchive thread | Thread menu; Settings Archived | no | port | M |
| Delete thread and confirmation | Thread menu; archived/settings flows | no | port | S |
| Multi-select and bulk actions | Sidebar range/select/context menu | no | port | M |
| Undo thread organization actions | Sidebar footer notice; Mod+Z outside editor | no | port | M |
| Filter thread list to project | Sidebar project scope/menu | partial: CLI `threads --project`; no TUI project filter | port | S |
| Copy thread reference, ID, branch, workspace path | Thread/header menu, shortcut | no | port; terminal clipboard support varies by host | S |
| Unpin/archive/delete confirmations | General | no | port | S |

### 5. Queue, steering and requests

Steer from the TUI, a queue panel to inspect, edit, reorder, cancel, promote and resume queued messages, restart a turn, the missing "always allow" approval that sends `acceptAlways`, dismissing a question, secret requests, implementing a proposed plan, usage-limit recovery, and restarting or handing off an agent session.

| Feature | Where it is in the app | t3term today | In a terminal | Size |
|---|---|---|---|---|
| Send text to existing thread | Composer; Send | yes | port | S |
| Queue follow-up while busy | Composer; default follow-up behavior | yes | port | S |
| Steer active turn with new message | Composer alternate send / follow-up preference | partial: CLI `--if-busy steer`; TUI normal send always queues | port | S |
| Restart active turn with message | Composer server-resolved delivery flow | no | port | S |
| Inspect held/queued messages | Composer queue control | partial: queued status label/transcript only, no queue control | port | M |
| Promote queued message to steering | Queue action; Mod+Shift+Enter | no | port | S |
| Edit latest/selected queued message | Queue editor; Alt+Up at composer start | no | port | M |
| Reorder queued messages | Queue handles, arrow keys | no | port | M |
| Cancel queued message | Queue remove action | no | port | S |
| Stop and hold all queued work | Composer Stop | partial: Ctrl+X/CLI interrupt stops active run, omits queue hold and associated cascade | port | S |
| Resume held queue | Queue control | no | port | S |
| Approve once, for session, persistently; decline/cancel | Composer pending approval banner | partial: once/session/decline/cancel; "Always allow" label sends `acceptForSession`, not `acceptAlways` | port | M |
| Inspect pending approval command/diff/output | Approval panel and transcript | partial: textual subject/body and scrolling, no graphical diff preview | port | M |
| Answer provider questions, free text and options | Composer question banner | partial: sequential text/number choice; multi-select produces one-element array | port | M |
| Attach files/images to question answers | Question banner/composer | no | port for transmission | M |
| Dismiss provider question | Question banner close control | no | port | S |
| Private secret entry and decline | Secret request timeline card | no; secret-request items are omitted | port via private prompt flow, not ordinary chat message | M |
| Implement or revise proposed plan | Plan follow-up composer banner | no | port | M |
| Copy/download/save plan to workspace | Plan card menu | no | port; terminal file destination replaces download dialog | S |
| Usage-limit auto-resume or snooze until reset | Limit recovery banner | no | port | M |
| Follow-up queue/steer preference | General | partial: CLI busy option only, no saved TUI preference | port | S |
| Provider/model context handoff | Model picker in existing conversation | partial: supported on servers advertising resolved command context; no dedicated handoff details UI | port | M |
| Restart agent session | Command palette | no | port | S |

### 6. Notifications

Desktop notifications when a thread finishes, fails, hits a limit or needs approval or input, plus a terminal bell and an in-app notice for other threads.

| Feature | Where it is in the app | t3term today | In a terminal | Size |
|---|---|---|---|---|
| OS completion/approval/input/failure/limit notifications | Background desktop notification | no | approximate; OS integration or terminal bell/toast | M |
| Pending-notification app badge and clear on focus | Desktop app badge | no | approximate; terminal status counter or host app badge integration | S |
| In-app notification with Open thread action | Toast while focused on another thread | no | port | S |
| Notification sound/mode/permission configuration | Settings General Notifications | no | approximate; sound/bell and host notification permission | M |

### 7. History, diffs and rewind

Load earlier turns, fork from a turn, merge back, parent and subagent links, turn and full-thread diffs with a file list, rewind to a turn with or without restoring files, and inline diff comments sent to the agent.

| Feature | Where it is in the app | t3term today | In a terminal | Size |
|---|---|---|---|---|
| Load earlier turns beyond bounded initial history | Timeline "Load earlier turns"; citation history lookup | no | port | M |
| Fork from a conversation turn | Timeline action | no | port | M |
| Merge conversation context back | Lineage section | no | port | M |
| Fold work groups/turns/attempts and lazy output | Timeline | partial: tool bundle folding and output hydration; no full turn/attempt controls | port | M |
| Conversation minimap and jump to turn | Timeline edge/minimap | partial: PageUp/Down, g/G and transcript scrolling only | approximate; textual turn index replaces graphical minimap | M |
| Checkpoint rewind to earlier turn | Timeline action, rewind dialog | no; checkpoint items omitted | port | M |
| Rewind and restore files | Rewind "Revert files too" | no | port | M |
| Rewind but retain current file changes | Rewind "keep changes" choice | no | port | M |
| Restore reverted prompt/attachments into composer | Rewind flow | no | port | M |
| Turn-range diff | Right panel Diff/Changes | no | port | M |
| Full-thread diff | Right panel Diff scope picker | no | port | M |
| Workspace/branch comparison diff | Diff compare-target control | no | port | L |
| Split or stacked diff layout | Diff toolbar | no | port; split needs sufficient terminal columns | M |
| Diff whitespace, wrap, refresh, fold all | Diff toolbar | no | port | S |
| Diff file tree and jump/path copy | Diff sidebar/file headers | no | port | M |
| Inline diff comments attached to agent prompt | Diff annotation editor | no | port using line/range selection | M |
| Diff colors | Appearance | no | port | S |
| Ignore diff whitespace by default | General | no | port | S |
| Collapse diffs by default | General | no | port | S |
| Default diff layout | General | no | port | S |

### 8. Machines

Running threads on other machines. In the nightly every machine runs its own T3 server, and the desktop app stays connected to all of them. You pick the machine when you start a thread, or let it pick the one with the most free CPU and memory. t3term connects to exactly one server today, the one on your Mac. So this means holding several connections at once, merging their threads into one sidebar, and pairing with remote machines over the network, SSH or T3 Connect.

| Feature | Where it is in the app | t3term today | In a terminal | Size |
|---|---|---|---|---|
| Host/environment selection for launch | Composer host control, details Workspace | no | port | L |
| Multiple server/environment connections, SSH and WSL | Settings Connections; host picker | partial: discovers/authenticates a server; no desktop environment management UI | port | L |
| T3 Connect remote relay setup | Settings Connections | no | approximate; textual setup, external sign-in/pairing where needed | L |
| Network access/Tailscale HTTPS controls | Settings Connections | no | port | L |
| Environment icons and machine identity | Connections/appearance/sidebar | no | approximate; glyph/text/color identity | M |
| Automatic host selection/load balancing | Environment settings/draft host selection | no | port | L |
| GitHub sharing / publish agent activity / offline webhook holding | Connection environment settings | no | port for controls; remote relay prerequisite | L |

### 9. Composer

Prompt history, stash, attaching files and images by path, folding a big paste into a file, `@` file mentions, `/` commands and `$` skills, model favorites, copying a message or code block, quoting a reply back to the agent, and the composer preferences.

| Feature | Where it is in the app | t3term today | In a terminal | Size |
|---|---|---|---|---|
| Persist/reuse stashed prompts | Composer stash control, Mod+S, stash menu | partial: in-memory failed-send/unsent prompt restore, not the full stash library | port | M |
| Prompt history navigation | Composer at beginning/end of input | no | port | S |
| Rich text/Markdown editing | Default composer, formatting and lists | partial: plain multiline input; transcript renders Markdown but input is not rich text | approximate; editable Markdown with textual list formatting | M |
| Send shortcut preferences | Settings General; composer | no: fixed Enter send, modified Enter/Ctrl+J newline | port | S |
| Resting single-line composer on conversation scroll | Floating composer | no | approximate; collapse to one text row, preserve draft/focus | M |
| Paste-to-focus and paste as text | Timeline/composer, Mod+Shift+V | no desktop-equivalent routing | port; host clipboard integration needed | M |
| Attach arbitrary files | Composer attach menu, drag/drop | no: sends `attachments:[]` | port | M |
| Attach/paste images | Composer image cards | no | port for sending image paths/bytes without displaying them | M |
| Image thumbnails, full-size viewing, zoom and pan | Composer/timeline; expanded image dialog | no | can't; faithful image display needs an image surface | L |
| Fold large pasted text into file attachment | Composer paste | no | port | M |
| Structured file/terminal/thread/review/skill context | Composer context chips and command menus | no | port for textual records; image/element presentation degraded | L |
| Slash commands, provider skills and mention picker | Composer trigger menu | no | port | M |
| Model/provider picker | Composer model control | yes; textual catalog picker | port | M |
| Effort and provider-specific option controls | Composer effort/options | yes, descriptor-driven boolean/select options | port | M |
| Permission/runtime modes | Composer mode control | yes | port | S |
| Model favorites and direct picker jumps | Model picker sidebar/favorite star, Mod+1..9 | no | port | M |
| Compact stale Claude history before resume / keep full history | Composer resume/history action and send flow | no | port | M |
| Copy message/code and toggle code wrapping | Timeline message controls and code header | no interactive copy action | port | S |
| Open/download message file attachment | User message attachment card | no | port | M |
| Assistant selection/citation comments and send | Selection toolbar/citation editor | no | approximate; text range selection/references, no pixel highlight overlay | M |
| Composer context display | Appearance | no | port for textual chips/details | S |
| Show skills in slash menu | General | no | port | S |
| Rich text opt-out | General | no; only plain text is implemented | approximate; plain/rich textual editor modes | M |
| Composer collapse-on-scroll preference | General | no | port | S |

### 10. Search, palette and navigation

Thread search through `orchestration.searchThreads`, a command palette, numbered jumps and back and forward.

| Feature | Where it is in the app | t3term today | In a terminal | Size |
|---|---|---|---|---|
| Thread title/content search | Sidebar search and match snippets | no | port | M |
| Previous/next thread, direct numeric jump | Shortcuts | partial: sidebar arrows/j/k, no app numeric jumps/history traversal | port | S |
| App route back/forward and reopen closed view | Shortcuts | no | port | M |
| Command palette | Mod+K | no | port | M |

### 11. Git and workspaces

Git status, commit, push, create a PR, pull, branch list and switch, worktree handoff and cleanup, opening the workspace in an editor, and the source-control settings.

| Feature | Where it is in the app | t3term today | In a terminal | Size |
|---|---|---|---|---|
| List/switch/create branch | Branch picker | no | port | M |
| Move current conversation into a new worktree, agent-owned handoff | Agent tool; workspace binding is visible in thread details. Direct renderer button unverified | no direct control | port | L |
| Commit | Git actions | no | port | M |
| Push | Git actions | no | port | M |
| Commit-and-push / commit-push-PR | Git actions | no | port | M |
| Pull / initialize repository | Branch/Git actions | no | port | M |
| Publish repository | Git publication dialog | no | port | M |
| Create PR from changes | Git actions | no | port | M |
| Resolve/checkout PR into thread workspace | PR thread dialog/branch menu | no | port | M |
| Worktree cleanup policy/location | Settings Storage | no | port | M |
| Delete worktree with threads / merged / unchanged cleanup | Settings Storage | no | port | M |
| Worktree submodule behavior | General | no | port | S |
| Auto-pull project repositories | Source Control/Project | no | port | S |
| Remove agent credits when merging | Source Control | no | port | S |
| Default PR merge method | Source Control | no | port | S |
| Source-control account discovery/configuration | Source Control | no | approximate; browser/account credential steps external | L |
| Git fetch interval and worktree branch naming | Source Control | no | port | S |
| GitHub account/token management | Source Control | no | approximate; private credential input and external auth | L |
| Bitbucket credentials | Source Control | no | port with private input | M |
| Source-control writing style/templates/writer model | Source Control writing section | no | port | M |

### 12. Pull requests

The PR inbox, PR detail with checks, linking and watching PRs from a thread, and the review, comment, merge and label actions.

| Feature | Where it is in the app | t3term today | In a terminal | Size |
|---|---|---|---|---|
| PR inbox/list, state/involvement filters, search and sort | Pull requests page, sidebar footer | no | port | L |
| PR summary/detail with branch, checks, activity and commits | Right panel Pull request / PR page | no | port | L |
| Link/unlink PR to a thread | Header/palette Link PR, details PR rows | no | port | S |
| Multiple linked PRs and native stacks | Sidebar PR badge, details linked PR rows | no | port | M |
| Watch PR and wake thread agent on changes | Linked PR controls/agent-owned watch state | no | port | M |
| PR files/diff preview and full file content | PR Changes panel | no | port | L |
| Mark PR files viewed | PR file review controls | no | port | S |
| PR discussion comments, edit and replies | PR composer/activity | no | port | M |
| Inline PR review comments and submit review/verdict | PR diff annotation/review form | no | port | L |
| Resolve/unresolve review thread | PR discussion | no | port | S |
| React to PR comments/reviews | PR activity | no | port; textual reaction names/glyphs | S |
| Request reviewers | PR details reviewer picker | no | port | M |
| Set labels | PR details label picker | no | port | M |
| Rename PR / edit PR metadata | PR detail inline editor | no | port | S |
| Merge PR with selected method | PR detail merge action | no | port | M |
| Enable/disable auto-merge, merge now | PR detail action menu | no | port | M |
| Draft/ready-for-review | PR action menu | no | port | S |
| Close/reopen PR | PR action menu; bulk close sweep | no | port | M |
| Approve PR workflows to run | PR checks/actions | no | port | S |
| Hand PR findings/check failures to agent | PR "fix findings/check" actions | no | port | M |
| Copy PR link/number/checkout command/branch | PR header/action menu | no | port | S |

### 13. Projects and files

Add, create, clone, rename and delete projects, project settings and actions, the file picker, content search, a file tree and file preview.

| Feature | Where it is in the app | t3term today | In a terminal | Size |
|---|---|---|---|---|
| List projects | Project scope/palette | yes, CLI projects list | port | S |
| Add existing local project | Add project/palette folder chooser | no | port | M |
| Create new project | Palette new-project wizard | no | port | M |
| Clone repository and optional private GitHub repository creation | Project wizard | no | port | L |
| Add project from WSL/remote environment | Project wizard/environment selector | no | port | L |
| Rename/update/delete project | Project Settings | no | port | M |
| Project default model and permission inheritance | Project Settings | no; thread selection exists, project defaults cannot be edited | port | M |
| Project icon/favicon and default workspace mode | Project Settings | no | approximate for icon artwork; glyph/color otherwise equivalent | M |
| Run project-file scripts | Project action menu | no | port | M |
| Add/edit/delete project actions and shortcuts | Project Settings/actions menu | no | port | M |
| Project file picker | Mod+P/palette | no | port | M |
| Project content search and line navigation | Mod+Shift+F/search dialog | no | port | M |
| Workspace file tree and directory navigation | Right panel Files | no | port | M |
| Text/source file preview with line reveals | Files preview | no | port | M |
| Edit and save workspace text files | File preview/editor | no | port; optional external editor can complement inline editor | L |
| Rendered Markdown/source toggle | File preview | no | approximate; terminal Markdown loses browser typography | M |
| CSV/TSV table/source toggle | File preview | no | port | M |
| PDF/HTML rendered preview | File preview browser frame | no | can't for faithful document/page rendering; text extraction is approximate | L |
| Audio/video file preview and controls | Media/file preview surfaces | no | can't for in-TUI media surface; external playback is approximate | L |
| File breadcrumb/path copy/open/save controls | File surface header | no | port | S |
| File/code word wrap | Appearance/file surfaces | partial: transcript wraps, no configurable file/code wrap preference | port | S |
| Add-project starting directory | General | no | port | S |

### 14. Settings page

One settings screen with the same sections as the desktop app, including light and dark, the built-in themes t3-chat, grove, ocean, ember and iris, and keybinding editing. Settings live in two places. Server settings are stored by the T3 server, so changing one in t3term changes it in the desktop app too. Client settings, like the theme and the send key, belong to each app, so t3term keeps its own. The Git, provider, connection and notification settings are listed in their own groups but go on this same screen. The four font rows can't work, because the terminal picks the font, not the app.

| Feature | Where it is in the app | t3term today | In a terminal | Size |
|---|---|---|---|---|
| Search settings and jump to matching control | Settings sidebar search | no | port | M |
| Color scheme system/light/dark | Appearance; palette | no; fixed dark theme | port | S |
| Built-in theme selection | Appearance/theme picker | no | port | M |
| Search/install custom themes and theme editor | Appearance/palette | no | approximate; edit terminal palette, no full CSS preview | L |
| Contrast preference | Appearance | no | port | S |
| Glass opacity | Appearance | no | approximate; opaque terminal colors cannot reproduce blur/translucent layers | S |
| Chat width | Appearance | no | port | S |
| Panel animations | Appearance | no | approximate; immediate or limited text transitions | S |
| Environment identity color/artwork | Appearance/sidebar | no | approximate; color/glyph identity, no background artwork | M |
| Interface font | Appearance | no | can't as app-controlled TUI font; terminal emulator controls font | S |
| Prompt font | Appearance | no | can't faithfully use separate prompt font within ordinary TUI | S |
| Code font | Appearance | no | can't faithfully use separate code font within ordinary TUI | S |
| Font smoothing | Appearance | no | can't; host text renderer owns smoothing | S |
| Sidebar project grouping | General/sidebar | no | port | M |
| Sidebar project ordering | General/sidebar | no | port | S |
| Automatically snooze usage-limited threads | General | no | port | S |
| Automatically resume usage-limited threads | General | no | port | S |
| Settle inactive threads after selected days | General | no | port | S |
| Settle thread when PR merges | General | no | port | S |
| Timestamp display format | General | no; fixed relative labels in sidebar | port | S |
| Response streaming behavior | General | no equivalent preference; streaming itself exists | port | S |
| Proactive panels | General/Beta | no | port for automatic text panel opening | M |
| Update checks | General | no | port | M |
| Continue interrupted threads after server restart | General | no | port | M |
| Background activity/suspension policy | General/Connections | no | port | L |
| Quit shortcut hold/double-press/confirmation behavior | General/native quit overlay | no; TUI quits directly | approximate; analogous terminal quit guard | M |
| Auxiliary text-generation model | General | no | port | S |
| Legacy Plan toggle flag | Beta settings | no preference; interaction mode itself supported | port | S |
| Legacy context meter flag | Beta settings | no | port | S |
| Legacy per-project sidebar flag | Beta settings | no | port | M |
| Working shelf beta flag | Beta settings | no | port | S |
| Keybinding editing, command/condition recording, removal | Settings Keybindings | no; fixed handler despite reading server config | port; host may reserve some chords | L |
| Provider health-check interval | Settings Providers | no | port | S |
| Privacy policy and open-source licenses | Settings links/licenses page | no | port as text/external link | S |
| Nightly mobile beta notice and companion-app links/QR | One-time toast; Settings General About | no | approximate; links/copy text replace QR popover | S |
| Rename/copy path/mute/close others/right/all tabs | Right tab context menu | no | port except media mute belongs to external browser | M |

### 15. Embedded terminal

T3's server-side shells in a pane: tabs, splits, resize, clear and restart, copy, sending output to the agent, and running a code block from a reply.

| Feature | Where it is in the app | t3term today | In a terminal | Size |
|---|---|---|---|---|
| Embedded interactive shell | Bottom drawer or right-panel Terminal tab | no; showing agent command output is a separate feature | port with terminal emulation or suspend/attach | L |
| Multiple shell tabs | Terminal drawer/tab controls | no | port | M |
| Horizontal/vertical terminal splits | Terminal controls and shortcuts | no | port | L |
| Resize drawer and terminal PTY | Drawer splitter/panel | no | port | M |
| Clear/restart/close terminal | Terminal menu/shortcuts | no | port | S |
| Terminal text selection, copy/paste, scrollback | Embedded Ghostty surface | no | port; clipboard depends on terminal host | M |
| Terminal output context attached to agent | Composer structured context | no | port | M |
| Run assistant shell code in terminal | Closed shell-code block play action | no | port | M |
| Embedded terminal font | Appearance | no | approximate; host terminal's font setting instead | S |

### 16. Providers, automations and usage

Provider setup and sign-in, scheduled tasks and webhooks, the usage page, updates and diagnostics.

| Feature | Where it is in the app | t3term today | In a terminal | Size |
|---|---|---|---|---|
| Provider instance list, add/update/configure | Settings Providers wizard/cards | partial: list models/providers/options; cannot add/configure/authenticate providers | port | L |
| Provider sign-in/logout and auth prompts | Provider authentication wizard | no; t3term server authentication is a separate flow | approximate; textual/device-code auth, browser sign-in external | L |
| Provider install/update/remove | Providers and provider-update footer pill | no | port | L |
| Custom provider model editing | Provider models/editor | no; reads configured custom models only | port | M |
| ACP registry discovery/install/auth/provider setup | Add-provider registry wizard | no | approximate; management portable, browser auth external where required | L |
| Import native/ACP agent sessions | Provider/onboarding/import flow | no | port | L |
| ChatGPT plan indicator and account usage link | Composer/model sharing control | no | port as label/external link | S |
| ChatGPT profile reconnect/import and handoff | Provider account flow | no | approximate; native/browser account steps remain external | L |
| Codex feedback submission | Composer recognized feedback command and feedback banner | no | port | S |
| Scheduled task creation/editing | Settings Scheduled tasks; per-thread Automations | no | port | L |
| Interval/fixed-time schedule controls | Scheduled task dialog | no | port | M |
| Enable/disable/run-now/delete scheduled task | Settings/task row/thread Automations | no | port | M |
| Webhook tasks, delivery history/detail, token rotation | Scheduled task webhook sections | no | port | L |
| Desktop/server updates, stable/nightly channel and restart | Settings/update footer | no | approximate; terminal binary/server upgrade workflow replaces desktop updater | L |
| Trace/process/server diagnostics | Settings Diagnostics | partial: Doctor is connectivity/config diagnosis, not trace/process UI | port | L |
| Resource telemetry/history and process signals | Diagnostics resource panels | no | port for charts/tables/process actions | L |
| Open logs/artifact directory | Diagnostics/Storage | no | port as path/external opener | S |
| Usage cost/tokens by model/time, account limits | Usage page | no | port using textual charts/tables | L |
| Usage day/week/month/quarter selection and refresh | Usage page toolbar/shortcuts | no | port | M |
| Usage provider configuration, rates/Cursor account usage | Settings Providers/Usage | no | port for configuration; external auth if required | M |

### 17. Browser, capture, devices and rich media

The embedded browser, screenshots and recordings, SnapShot, simulators, and timeline items that are HTML, Mermaid or MCP apps. Most of this can't run in a terminal; the useful part is opening it in the real browser and showing a text summary.

| Feature | Where it is in the app | t3term today | In a terminal | Size |
|---|---|---|---|---|
| Live embedded browser/page | Right panel Browser | no | can't; requires browser surface | L |
| Detect local servers and open suggested URL | Browser empty state | no | port for URL list/external open | M |
| URL entry, back/forward, refresh/stop | Browser chrome | no | approximate; command controls and external browser, no inline page | M |
| Open preview in system browser | Browser chrome | no | port as external launcher | S |
| Browser zoom/reset | Menu/shortcuts | no | approximate; operate external/native browser | S |
| Browser appearance light/dark/system | Browser menu/settings | no | approximate; external/native browser control | S |
| Responsive viewport/device toolbar and resizing | Browser menu/viewport handles | no | approximate; dimension controls without inline visual result | M |
| Inspect/select page element and attach annotation | Browser annotation mode | no | can't for faithful visual picking; DOM/text selection is approximate | L |
| Draw/erase screenshot annotations | Browser annotation overlay | no | can't; image/canvas interaction | L |
| Browser screenshot and save/attach | Browser capture control | no | approximate; capture/save/send files without image preview | L |
| Browser video recording, stop, save | Shift-click capture, recording controls | no | approximate; recording control/files without inline playback | L |
| Recording keystroke/mouse overlays and frame rate | Settings Integrations Browser | no | approximate; configure external capture, no visual overlay preview | M |
| Floating preview/mini-player / native picture-in-picture | Conversation side/overlay, browser menu | no | can't for faithful live visual window | L |
| Hard reload and DevTools | Browser more menu | no | approximate; external developer-tools launcher | M |
| Clear browser cookies/cache | Browser more menu, profile settings | no | port for management controls | M |
| Browser profiles, create/select/remove/default | Settings Integrations Browser | no | port for management, browser session remains external | L |
| Import browser profile/logins from another browser | Browser import wizard | no | approximate; wizard can be textual but still needs host browser integration | L |
| System-wide SnapShot capture shortcut | Settings SnapShots / desktop capture overlay | no | approximate; external system capture then attachment | L |
| Include app accessibility text and source details in SnapShot | SnapShot settings/attachment details | no | approximate; host accessibility integration and textual metadata | L |
| SnapShot sound, flash and capture animations | Settings SnapShots | no | approximate; audible feedback possible, window flash/animation needs GUI | M |
| Device hosts/hub setup and test | Settings Integrations / device setup | no | port for configuration | L |
| Live simulator/emulator surface | Right panel Device | no | can't; live graphical device display | L |
| Device start/open/close/shutdown | Device tab/menu | no | port for controls, live display external | M |
| Device rotation, screenshot and tools | Device control rail | no | approximate; controls portable, visual result external | M |
| Device text size/color filters/orientation | Device Tools drawer | no | port for controls | M |
| Device permissions and simulated location | Device Tools drawer | no | port | M |
| Device visual accessibility overlay and pointer/keyboard input | Device stream/tools | no | can't for faithful overlay/pointer surface; textual accessibility controls are approximate | L |
| Agent browser permission, including project override | Settings Integrations/Project | no | port for control | S |
| Agent device permission | Settings Integrations | no | port | S |
| Enable simulator/device support | Settings Integrations | no | port for control | S |
| Default browser viewport/zoom/appearance | Settings Integrations | no | approximate; controls external/native browser | M |
| Open links in chosen browser destination | Settings Integrations | no | port via external launch policy | S |
| Automatically show floating preview | Settings Integrations | no | can't for faithful floating surface | M |
| Use generated artifact template skill again | Artifact-template card "Use template" | no | port | M |
| Mermaid diagrams | Assistant Markdown | no | approximate; show Mermaid source or textual diagram, lose exact graphic | M |
| Agent-rendered HTML | Timeline HTML render row | no | can't; faithful interactive HTML needs browser | L |
| Interactive MCP application cards | Timeline MCP app | no | can't for faithful app; textual tool forms are an approximation | L |
