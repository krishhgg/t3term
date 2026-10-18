# Launching a thread

`t3term launch` starts a new thread in a project, with its first message, on the server the command chose. It returns once T3 accepts the launch, before the agent starts.

```bash
t3term launch t3term "Fix the flaky build" --model claude-opus-5-5
t3term launch 4e2b9d71 --model claudeAgent/claude-opus-5-5 --worktree main --branch t3/fix-build < prompt.md
t3term --environment lab launch api "Add a health check" --model gpt-6 --mode auto-accept-edits --plan
```

The fields and behavior below follow T3 Code at nightly f570bd21: `OrchestrationV2ThreadLaunchInput` and `OrchestrationV2ThreadLaunchResult` in packages/contracts/src/orchestrationV2.ts, the handler in apps/server/src/ws.ts and apps/server/src/orchestration-v2/ThreadLaunchService.ts.

## Arguments

- `PROJECT` is a project's full id, a unique id prefix such as the 8 characters `t3term projects` prints, or its exact title. t3term looks only on the chosen server and skips projects T3 has deleted. No match exits 3 with `PROJECT_NOT_FOUND`. More than one exits 2 with `AMBIGUOUS_PROJECT`, and the message lists each match's id prefix and title.
- `PROMPT` is the first message. Leave it out and t3term reads stdin to the end. t3term sends it as typed, so an indented snippet keeps its indent and a message from stdin keeps its final newline. `send` trims its message instead. T3's Claude adapter trims the message before it passes it to Claude (`applyClaudePromptEffortPrefix` in apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts), so Claude doesn't get the whitespace at the start and end of what t3term sent. The indent of later lines reaches it. `--effort ultrathink` puts `Ultrathink:` on a line before the message, as the desktop app does, unless the trimmed message starts with a slash command or with `Ultrathink:`. Then the message goes unchanged.
- `--model` is required, in any form `send` takes. t3term doesn't pick a model, because the desktop app's choice depends on state it keeps in the browser.
- `--title` names the thread, trimmed. Without it, the title is the trimmed message cut to 50 UTF-16 code units with `...` added, as the desktop app makes it, and T3 writes a better one. The desktop app can cut an emoji in half at the 50th unit. t3term cuts between characters, so it leaves that emoji out.
- `--worktree BASE_REF` asks T3 for a new git worktree made from that branch, tag or commit. Without it the agent works in the project's folder.
- `--branch NAME` names the new worktree's branch. Without it T3 checks out a temporary branch and renames it later.
- `--from-origin` asks T3 to fetch origin first and start from origin's copy of the base ref. T3 skips this when the project has no origin.
- `--effort`, `--option`, `--mode`, `--plan` and `--no-plan` work as they do on `send`.

`--branch` and `--from-origin` need `--worktree`. Before it connects or logs in, t3term checks the model, the project argument, the message, the title, the base ref, the branch and each `--option`, and a problem with any of them exits 2. An empty message exits 2 with `PROMPT_REQUIRED`, and a missing model with `MODEL_REQUIRED`. t3term asks for no message on stdin until the model is given.

## Modes

Without `--mode`, the thread takes the project's default runtime mode from T3's settings, then the environment's default, then T3's own default, `full-access`. The desktop app picks the same way (`resolveProjectSettings` in packages/shared/src/projectSettings.ts). When the model's provider lists the modes it supports and the default isn't one of them, the launch exits 2 with `INVALID_CHOICE` and names the default, so you can pass `--mode` instead. Settings t3term can't read exit 1 with `T3_INVALID_CONFIG` rather than fall back to `full-access`, and the message doesn't repeat what they hold. A launch with `--mode` doesn't read them.

Without `--plan`, the thread starts outside plan mode. The output says which runtime mode the thread got and where it came from: `chosen` for `--mode`, `project` for the project's default and `environment` for the environment's or T3's.

## What T3 does with it

t3term logs in with the read and operate scopes and sends one `orchestration.launchThread`, with a command id, a thread id and a message id it picks once. It sends no attachments, `creationSource: "web"` as `send` does, and no `reuseExistingThread`, so T3 creates a new thread.

T3 creates the thread, sends the first message, makes a run for it and answers with the thread. It then prepares the workspace in the background, and the agent starts when that finishes. So exit 0 means T3 accepted the launch. It doesn't mean the worktree exists or the agent started.

t3term checks the answer before it prints it. The answer's thread id and the projection's thread must be the thread t3term asked for, `resumed` must be a boolean, and the projection must hold a run for the first message with an id and a status. The run's status is `preparing` while T3 sets up the workspace, or a later one if the run has already moved on.

A workspace that fails to prepare shows on the thread. `t3term wait` exits 1 with the outcome `failed`, and `t3term read` shows T3's error. In `t3term --json read`, that error item's `failure.code` is `workspace_preparation_failed`.

## Output

Without `--json`:

```text
launched 1a2b3c4d  Fix the flaky build
  environment  lab (env-lab)
  project      4e2b9d71  t3term
  model        claudeAgent/claude-opus-5-5
  mode         Full access (environment default), plan mode off
  workspace    a new worktree from main, on branch t3/fix-build
T3 accepted the launch and is preparing the workspace. The agent starts when it's ready.
Follow it with `t3term --environment lab wait 1a2b3c4d-7e6f-4a5b-9c8d-0e1f2a3b4c5d`.
```

The environment line shows the `--environment` name when the command had one, and the environment id the server proved. t3term drops control characters from the values T3 sent, as it does for other commands.

With `--json`, stdout holds one object:

| Field | Value |
|---|---|
| `ok` | `true` |
| `environmentId` | The id of the environment t3term checked and launched on. |
| `projectId`, `threadId`, `commandId`, `messageId` | The full ids t3term sent. |
| `runId`, `runStatus` | The run T3 made for the first message, and its status when T3 answered. |
| `resumed` | Whether T3 had accepted this command id before. The CLI sends each launch once, with new ids, so it expects `false`. |
| `projectTitle` | The project's title as T3 sent it. |
| `title`, `generateTitle` | The title sent, and whether T3 will write a better one. |
| `modelSelection` | The model and options sent. |
| `runtimeMode`, `runtimeModeSource` | The runtime mode sent, and `chosen`, `project` or `environment`. |
| `interactionMode` | `default` or `plan`. |
| `workspaceStrategy` | `{"type": "root"}`, or `{"type": "worktree", "baseRef": ...}` with `branch` and `startFromOrigin: true` when given. |

## Errors

An error before the launch goes out keeps the code any command gets, such as `T3_SERVER_UNAVAILABLE` or a login failure, because T3 never saw the launch. After it goes out, `--json` prints the error with the launch's ids beside it, an `outcome`, and a `read` command that finds the thread. The command keeps `--environment NAME` when the launch had it. The plain message ends with the same command.

| Code | Exit | `outcome` | When |
|---|---|---|---|
| `THREAD_LAUNCH_UNCONFIRMED` | 5 | `unknown` | The connection dropped, or T3 didn't answer within 60 seconds. T3 may have launched the thread, or may still. |
| `T3_INVALID_LAUNCH` | 1 | `unknown` | T3 answered, but not with the thread and run t3term asked for. |
| `THREAD_LAUNCH_REJECTED` | 4 | `rejected` | T3 ran the launch and it failed with T3's own launch error, `OrchestrationV2ThreadLaunchError`. |
| `T3_RPC_FAILED` | 4 | `rejected` | Any other failure T3 tags, such as `EnvironmentAuthorizationError` for a session without the operate scope. |
| `T3_RPC_FAILED` | 4 | `unknown` | A failure without a tag t3term can read, such as a defect from a handler that died. It doesn't say how far the launch got, so T3 may have created the thread. |
| `OUTPUT_FAILED` | 1 | `accepted` | T3 accepted the launch, but t3term couldn't write the result to stdout, for example because the program reading it exited. The thread exists, so launching again would start a second one. |

```json
{"ok": false, "error": {"code": "THREAD_LAUNCH_UNCONFIRMED", "message": "..."},
 "environmentId": "env-lab", "projectId": "...", "threadId": "...", "commandId": "...",
 "messageId": "...", "outcome": "unknown",
 "read": "t3term --environment lab read 1a2b3c4d-7e6f-4a5b-9c8d-0e1f2a3b4c5d"}
```

### A launch t3term can't confirm

t3term never sends a launch again on its own, with the same ids or new ones. T3 holds a launch that arrives while its server starts until startup finishes (apps/server/src/serverRuntimeStartup.ts:123-133), so a launch t3term gave up on can still create its thread later. A thread missing from `read` right after doesn't prove T3 dropped the launch. Running `launch` again picks new ids and can start a second thread, so look for the first one with the `read` command the error prints.

A failure without a tag gets the same `unknown` outcome and the same advice. t3term matches failures by tag alone and doesn't parse T3's message, so it can't tell a defect after T3 created the thread from one before. A T3 server without `orchestration.launchThread` answers with a defect too, so its launch is reported as unknown although nothing was created.

### A launch t3term can't print

`OUTPUT_FAILED` comes from the CLI after T3 has answered, so t3term doesn't send the launch again or call it unconfirmed. With `--json` the error goes to the same stdout that failed, so it usually doesn't arrive and only the exit code shows. Without `--json` it goes to stderr with the thread's id. Either way t3term revokes its temporary session and waits up to three seconds for the revoke to finish before it exits, as it does after any other error.

### A rejected launch

T3 creates the thread before it sends the first message. A launch that fails after that leaves the thread behind under the id t3term printed, possibly without its first message. The `read` command shows whether it did. t3term reports T3's message as it came, and doesn't parse it or the cause T3 attaches.

## The library

`t3term::launch` holds the same launch for other Rust code:

- `LaunchRequest::new` takes a connected `Client`, a project id, the message, an optional title, a `Plan` from `models::plan_new_thread` and a `Workspace`. It checks them as the CLI does and picks the three ids.
- A request is bound to the environment id of the client that built it. `Client::launch_thread` refuses a request built for another environment with `LAUNCH_WRONG_ENVIRONMENT`, exit 2, before it connects. The request T3 gets names no environment.
- Sending the same request again, through a client for the same environment, is the only safe retry. T3 treats a command id it has accepted as the same command and answers with the thread that launch made, with `resumed` set to `true`. The request's fields are private, so its ids and content can't change between attempts.
- `Client::launch_thread` returns a `LaunchReceipt` with the thread id, the run's id and status, `resumed` and the thread T3 returned. Its errors are the ones above except `OUTPUT_FAILED`, which only the CLI returns, with the ids and `outcome` in `T3Error::details`.
- `Debug` for a request prints its ids, the model's instance and model ids, how many options it has, the modes, the lengths of the message and the title, and the workspace's kind, the lengths of its base ref and branch and `start_from_origin`. It leaves out the message, the title, the options' values, any other field of the model selection and the ref and branch names. `Debug` for a receipt leaves out the thread.

## Not supported

- Launching into an existing worktree (`existing_worktree`), reusing a thread (`reuseExistingThread`), attachments and context.
- A new-thread action in the TUI.
- The project's `newWorktreesStartFromOrigin` setting. Pass `--from-origin` instead.
- Retrying or cancelling a workspace that failed to prepare.
