# Terminal sessions

T3 checks each terminal RPC against `terminal:read` or `terminal:operate`, never against the orchestration scopes (apps/server/src/auth/RpcAuthorization.ts:172-181 at T3 f570bd21). The saved login holds `orchestration:read` and `orchestration:operate` alone and stays that way, since it lasts 30 days in the Keychain. So the terminal gets a session and a WebSocket of its own.

`Client::terminal_rpc` makes both. Nothing in the CLI or TUI calls it yet, so no t3term command issues a terminal session today.

## The session

- It carries `terminal:read` and `terminal:operate` and nothing else, and lasts 12 hours.
- T3 lists it as `t3term terminal`, with the subject `t3term`.
- t3term issues it with the server's own `t3 auth session issue --base-dir`, against the T3 home of the environment the client connected to.
- It is never saved. Like any session that lasts one run, it is revoked when the client drops and at exit, including after Ctrl+C, `kill` or a closed terminal window.
- Revoking it doesn't close the socket it opened, which keeps the scopes it opened with (apps/server/src/ws.ts:3155-3213 and apps/server/src/auth/SessionStore.ts:995-1012 at T3 f570bd21). So when the client drops, t3term closes the terminal's connection first and then starts the revoke, as [Closing](#closing) says.
- A login can't carry a terminal scope. `Session::login` and `Session::issue` refuse one with `TERMINAL_SCOPE_NOT_ALLOWED`.

## When t3term issues it

Connecting a client issues nothing, and neither does `Client::rpc`, which keeps the login's scopes. The first `terminal_rpc` call does this, in order:

1. It checks the server at the client's origin again. The server must still report the client's environment id and orchestration protocol 2. A named environment's server must also report the id its T3 home holds, as when the client connected. t3term asks that origin alone, even when the home now records another one. Otherwise the call stops before t3term issues a session or sends a token, with `T3_ENVIRONMENT_MISMATCH`, `T3_ENVIRONMENT_CHANGED`, `T3_SERVER_UNAVAILABLE` or the error discovery reports.
2. It issues the session.
3. It asks `/api/auth/session` about the session and reads `permissions`. The older `scopes` field leaves out `terminal:read` (packages/contracts/src/auth.ts:131-149 at T3 f570bd21), so t3term doesn't read it.
4. It gets a WebSocket ticket with the session's token and opens `/ws` with it. The ticket and the WebSocket handshake get 30 seconds together. Past that the call fails with `TERMINAL_CONNECT_TIMED_OUT` and keeps the session for the next call.

What t3term does with the answer in step 3:

| `/api/auth/session` answers | t3term |
| --- | --- |
| `authenticated: true`, and `permissions` lists `terminal:read` and `terminal:operate` alone | Opens the socket. |
| `authenticated: true` with other `permissions`, or none | Revokes the session and fails with `TERMINAL_ACCESS_REFUSED`. The client issues no other terminal session. |
| `authenticated: false` for the session it just issued | Revokes it and fails with `TERMINAL_SESSION_REJECTED`. |
| `authenticated: false` for a session issued earlier | Revokes it and issues one in its place, once. |
| A redirect, an error status, an answer it can't read, or none | Fails with that error. It follows no redirect and keeps the session, which T3 may still accept. |

## Sharing and reconnecting

- A client runs one connect at a time. Every call that comes while it runs waits for it and gets what it ends with: the socket, or the same error with the same code, exit code and message. Each call gets its own copy of the error, without the causes behind it, which t3term doesn't print.
- A call that comes after a connect ends starts a new one. So the next call after a failure tries again, and t3term keeps no failure.
- Once the socket drops, the next call checks the server and the session again and opens a new socket with the same session.
- A ticket request, handshake or socket that fails or runs out of time keeps the session for the next call.
- A connect runs on a task of its own. A caller that stops waiting can't drop a session between issuing and tracking it, and it starts no other connect. A client that drops mid-connect revokes the session once the connect ends. Every step of a connect has a time limit, so it does end.
- A client starts at most one issue a minute. A call within the minute fails with `TERMINAL_SESSION_LIMITED` and says why the last issue failed, if it did. Nothing retries in the background.

## Closing

When the client drops, t3term closes the terminal's connection and then starts revoking the session. Closing reaches every copy of the connection a caller still holds, in this order:

1. Before the close returns, every copy is marked closed. A call made from then on fails at once and sends nothing, and a request still queued never goes out.
2. The connection's task stops any write T3 hasn't taken, even one T3 stopped reading partway through. Each call or stream still waiting on T3 ends with `T3_DISCONNECTED`.
3. The task gives a WebSocket Close frame one second to go out, then drops the socket whether it went or not. A T3 that stopped reading never takes the frame, so its socket drops at that second.

The revoke starts once step 1 is done. It doesn't wait for steps 2 and 3, so T3 may see the revoke before the socket drops.

t3term sends T3 no request when it closes, so it doesn't close, kill or restart a terminal. T3 sees a client that went away. The client's other socket, the login's, stays open.

The session lasts 12 hours, and nothing renews it. At T3 f570bd21, a socket's session is checked when the socket opens, and nothing in `ws.ts` ends an open socket when its session expires. So an open terminal socket may outlive its session. Nobody has tried this against a running T3. A call after the socket drops and the session expires finds the session rejected, revokes it and issues one in its place.

## Errors

| Code | Exit | Meaning |
| --- | --- | --- |
| `TERMINAL_SCOPE_NOT_ALLOWED` | 1 | A login or a one-run session asked for a terminal scope. |
| `TERMINAL_ACCESS_REFUSED` | 4 | T3 lists other permissions for the terminal session. Lasts until t3term starts again. |
| `TERMINAL_SESSION_REJECTED` | 4 | T3 doesn't accept the terminal session it just issued. |
| `TERMINAL_SESSION_LIMITED` | 5 | The client tried to issue a terminal session less than a minute ago. |
| `TERMINAL_CONNECT_TIMED_OUT` | 5 | The ticket and WebSocket handshake took more than 30 seconds. The session stays for the next call. |
| `T3_ENVIRONMENT_CHANGED` | 4 | The server at the client's origin is another environment now. The next call checks again, and connects if the client's environment is back. |
| `TERMINAL_CONNECT_FAILED` | 1 | The connect's task stopped without an outcome, as at a panic. The next call starts another connect. |

## Known limits

- A signal that arrives while `t3 auth session issue` runs kills it. T3 may have made a session t3term never saw, and it expires within 12 hours. The saved login has the same gap.
- `TERMINAL_ACCESS_REFUSED` lasts until t3term starts again. After any other error, the next call checks the server and the session again.
- While the client lives, a write T3 doesn't read waits on the open connection, and the 30-second silence check doesn't run until it ends. The login's connection works the same way. Dropping the client ends the wait, as [Closing](#closing) says.
- An open terminal socket may outlive its 12-hour session, as [Closing](#closing) says.
- The terminal itself, opening a shell and the TUI's terminal panel aren't built yet.
