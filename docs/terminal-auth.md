# Terminal sessions

T3 checks each terminal RPC against `terminal:read` or `terminal:operate`, never against the orchestration scopes (apps/server/src/auth/RpcAuthorization.ts:172-181 at T3 f570bd21). The saved login holds `orchestration:read` and `orchestration:operate` alone and stays that way, since it lasts 30 days in the Keychain. So the terminal gets a session and a WebSocket of its own.

`Client::terminal_rpc` makes both. Nothing in the CLI or TUI calls it yet, so no t3term command issues a terminal session today.

## The session

- It carries `terminal:read` and `terminal:operate` and nothing else, and lasts 12 hours.
- T3 lists it as `t3term terminal`, with the subject `t3term`.
- t3term issues it with the server's own `t3 auth session issue --base-dir`, against the T3 home of the environment the client connected to.
- It is never saved. Like any session that lasts one run, it is revoked when the client drops and at exit, including after Ctrl+C, `kill` or a closed terminal window.
- A login can't carry a terminal scope. `Session::login` and `Session::issue` refuse one with `TERMINAL_SCOPE_NOT_ALLOWED`.

## When t3term issues it

Connecting a client issues nothing, and neither does `Client::rpc`, which keeps the login's scopes. The first `terminal_rpc` call does this, in order:

1. It checks the server at the client's origin again. The server must still report the client's environment id and orchestration protocol 2. A named environment's server must also report the id its T3 home holds, as when the client connected. t3term asks that origin alone, even when the home now records another one. Otherwise the call stops before t3term issues a session or sends a token, with `T3_ENVIRONMENT_MISMATCH`, `T3_ENVIRONMENT_CHANGED`, `T3_SERVER_UNAVAILABLE` or the error discovery reports.
2. It issues the session.
3. It asks `/api/auth/session` about the session and reads `permissions`. The older `scopes` field leaves out `terminal:read` (packages/contracts/src/auth.ts:131-149 at T3 f570bd21), so t3term doesn't read it.
4. It gets a WebSocket ticket with the session's token and opens `/ws` with it.

What t3term does with the answer in step 3:

| `/api/auth/session` answers | t3term |
| --- | --- |
| `authenticated: true`, and `permissions` lists `terminal:read` and `terminal:operate` alone | Opens the socket. |
| `authenticated: true` with other `permissions`, or none | Revokes the session and fails with `TERMINAL_ACCESS_REFUSED`. The client issues no other terminal session. |
| `authenticated: false` for the session it just issued | Revokes it and fails with `TERMINAL_SESSION_REJECTED`. |
| `authenticated: false` for a session issued earlier | Revokes it and issues one in its place, once. |
| A redirect, an error status, an answer it can't read, or none | Fails with that error. It follows no redirect and keeps the session, which T3 may still accept. |

## Sharing and reconnecting

- Calls that come at once wait for one connect and share the session and the socket it makes.
- Once the socket drops, the next call checks the server and the session again and opens a new socket with the same session.
- A ticket request or socket that fails keeps the session for the next call.
- A connect runs on a task of its own. A caller that stops waiting can't drop a session between issuing and tracking it, and a client that drops mid-connect revokes the session once the connect ends.
- A client starts at most one issue a minute. A call within the minute fails with `TERMINAL_SESSION_LIMITED` and says why the last issue failed, if it did. Nothing retries in the background.

## Errors

| Code | Exit | Meaning |
| --- | --- | --- |
| `TERMINAL_SCOPE_NOT_ALLOWED` | 1 | A login or a one-run session asked for a terminal scope. |
| `TERMINAL_ACCESS_REFUSED` | 4 | T3 lists other permissions for the terminal session. Lasts until t3term starts again. |
| `TERMINAL_SESSION_REJECTED` | 4 | T3 doesn't accept the terminal session it just issued. |
| `TERMINAL_SESSION_LIMITED` | 5 | The client tried to issue a terminal session less than a minute ago. |
| `T3_ENVIRONMENT_CHANGED` | 4 | The server at the client's origin is another environment now. Start t3term again. |
| `TERMINAL_CONNECT_FAILED` | 1 | The connect task stopped before it opened the socket. |

## Known limits

- A signal that arrives while `t3 auth session issue` runs kills it. T3 may have made a session t3term never saw, and it expires within 12 hours. The saved login has the same gap.
- t3term doesn't close a terminal connection a caller still holds when it revokes the session.
- `TERMINAL_ACCESS_REFUSED` and `T3_ENVIRONMENT_CHANGED` last until t3term starts again.
- The terminal itself, opening a shell and the TUI's terminal panel aren't built yet.
