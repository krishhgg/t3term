# Proposed next steps: one Rust client with a TUI and CLI

Updated 7 October 2026. This is an implementation proposal, not a completed client. Keep the separate sys1rust process running as requested.

A V2 CLI already exists: [MajesteitBart/t3code-cli](https://github.com/MajesteitBart/t3code-cli). Version 0.3 targets V2. It can discover/read/message threads, wait for replies, change model controls, handle approvals/questions, and manage queued work. It is useful immediately and as a protocol reference.

An existing TUI is [StevenMatchett/t3code on tui-main](https://github.com/StevenMatchett/t3code/tree/tui-main). Use it as the current TypeScript baseline and workflow reference. [Pajn/tria](https://github.com/Pajn/tria) supplies Rust terminal and transport ideas, but its inspected source still needs a V2 port.

The proposed native product is one executable, tentatively `t3term`. Default invocation opens the interactive TUI. Subcommands provide a CLI. Both call the same Rust connection, state and command library. Keep the official T3 server responsible for providers, orchestration, storage, Git and PTYs.

## Milestone 1: shared V2 connection and a small read-only CLI

Pin one known server build and record the V2 contract revision. Implement discovery, pairing/auth, version detection, authenticated HTTP/WebSocket RPC and bounded stream handling.

Proposed commands, not commands that exist yet:

```text
t3term doctor
t3term projects list --json
t3term threads list --json
t3term threads read <id>
```

Use replayed synthetic fixtures and a disposable server environment. Never open a second writable server against the user's live database. Acceptance: connect to the pinned V2 server, list projects/threads and read a transcript; unsupported protocols produce a clear error; secrets never appear in output.

## Milestone 2: complete one interaction through the same core

Implement prompt submission, streamed state updates, turn status, interruption, approvals and questions. Add sequence/resume, reconnect and snapshot recovery before treating streaming as complete. CLI output gets human-readable text, JSON results and line-delimited JSON for event streams, plus stable exit codes.

Acceptance: one synthetic turn can start, stream, pause for approval, finish and survive a connection drop without duplicate messages. Compare its final projection with T3's TypeScript reducer. Keep state transitions when visual frames are coalesced.

## Milestone 3: build the first usable TUI

Rust, Ratatui and Crossterm. Show the project/thread sidebar, conversation, tool groups and multiline composer. Include model/reasoning selection, approvals, questions and keyboard/mouse navigation. Both CLI and TUI must use Milestones 1 and 2 rather than implement their own RPC/state logic.

Acceptance: open an existing thread, submit a prompt and respond to an approval through the TUI. Stable scroll anchoring, cached completed Markdown, visible-row rendering and no periodic idle frames are part of this milestone.

## Milestone 4: desktop-independent operation and power integration

Attach to or explicitly manage a compatible headless official T3 server so normal use can work with Electron closed. Preserve existing state through supported environment/startup behavior. Report power state from the server's own host. Send client focus and visible-scope activity leases. Respect auth scopes and distinguish client battery state from remote-host battery state.

Acceptance: projects and threads stay consistent between clients; a headless environment uses T3's battery policy with fresh host state; hidden panes do not subscribe to preview discovery or continuous diagnostics.

## Milestone 5: complete high-value T3 workflows

Add diff/checkpoint review, worktrees, agent visibility, queued prompts, attachments and embedded terminals. Use supported terminal image protocols with fallbacks. Open interactive web previews in an external browser. Prioritize everyday workflow parity before uncommon panels.

## Milestone 6: prove resource savings and package

Compare release builds of Electron, optimized OpenTUI and the Rust client against the same backend and deterministic workload. Include terminal-emulator cost, server and provider processes. Use alternating paired runs with fixed display and power settings, and report variance. Measure idle frames/wakeups, CPU time, physical footprint, typing latency and whole-machine energy per completed task.

The first engineering targets are no periodic idle frames, less than 0.5% of one CPU core over five minutes, at most one client-attributable idle wakeup per second, default streaming presentation at most 30 fps and p95 typing echo below 50 ms. These are proposed targets, not observed results.

Do not claim a battery-saving percentage until repeated whole-system measurements support it. Package a macOS release first, then verify Linux and Windows independently.

## First deliverable

Start with a small Rust V2 client that can list threads, send and stream a prompt, handle an approval and reconnect. Expose those operations through CLI commands and a simple TUI on the same core. This is the smallest complete slice that tests the protocol, the user experience and the efficiency goal together.

The existing TypeScript CLI means the CLI-only use case does not have to wait for this implementation. Use its behavior and fake-server fixtures as references rather than re-discovering every protocol detail.
