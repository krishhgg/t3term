# Environment connections

t3term can save named environments and send a command to one of them with `--environment NAME`. Each environment is a T3 home on this machine, the directory a T3 server keeps its state in, and optionally the loopback origin of that server. A saved environment is strict: t3term talks to its server and no other, and logs in only once that server proves it belongs to the T3 home.

Remote environments, pairing and T3 Connect aren't supported yet. The last two sections say what t3term refuses and why, and what T3 Connect would take.

## Saving environments

```bash
t3term environment add work --t3-home ~/.t3
t3term environment add lab --t3-home ~/t3-lab --origin http://127.0.0.1:4000
t3term environment default work
t3term environment list
t3term --environment lab threads
t3term environment rename lab scratch
t3term environment default --clear
t3term environment remove scratch
```

- `--t3-home` is the directory the server was started with as `--base-dir` or `T3CODE_HOME`, such as `~/.t3`. A relative path is saved from the current directory. t3term reads `userdata/environment-id` and `userdata/server-runtime.json` there.
- `--origin` is where the server listens, such as `http://127.0.0.1:3773`. Leave it out and t3term reads the origin from `server-runtime.json` each time it connects, so a server that restarts on a new port is still found. Give it when you start the server on a fixed port. It must be http, which is how a local T3 server listens. t3term is built without TLS, so it refuses an https origin with `ENVIRONMENT_TLS_UNSUPPORTED`.
- A name is 1 to 32 lowercase letters, digits, hyphens and underscores, and starts with a letter or digit.
- `--default` on `add`, or `environment default NAME`, makes an environment the default for commands that name none. `environment default --clear` goes back to discovery.
- `remove` refuses the default. Choose another default or clear it first.
- `rename` moves the default with the environment.

These commands read and write `~/.config/t3term/environments.json` and nothing else. They reach no server and run no `t3`, so they work with every server stopped. `--json` prints what they changed. `environment list --json` prints each environment's name, kind, T3 home, origin and whether it is the default, and no other key the file holds.

## Which server a command uses

t3term chooses once per command, before it reaches any server:

1. The environment `--environment NAME` names. `T3TERM_ORIGIN`, `T3CODE_HOME` and the saved default don't apply.
2. Otherwise, `T3TERM_ORIGIN` when it is set and not empty. It works as it did before environments: when its server doesn't answer, t3term tries the servers `T3CODE_HOME` (or `~/.t3`) records.
3. Otherwise, the saved default environment.
4. Otherwise, discovery: the server `T3CODE_HOME` (or `~/.t3`) records in `userdata/server-runtime.json`, then in `dev/server-runtime.json`.

A saved environment, from the flag or the default, is the only server t3term tries. When it fails a check below, the command stops with an error and t3term doesn't try `T3TERM_ORIGIN`, `T3CODE_HOME` or another environment instead.

Two problems with `environments.json` stop a command, because t3term can't tell which server you chose and doesn't guess. They don't have the same way past:

| Problem | A command that names no environment | With `--environment NAME` | With `T3TERM_ORIGIN` and no `--environment` | To fix it |
| --- | --- | --- | --- | --- |
| The default names an environment that isn't saved. | Stops with `ENVIRONMENT_NOT_FOUND`, exit 3. | Runs, since the file still reads. | Runs, and the file isn't read. | `t3term environment default --clear`, or `environment default` with a saved name. |
| `environments.json` doesn't parse or can't be read. | Stops with `ENVIRONMENTS_UNREADABLE`, exit 1. | Stops the same way, since t3term looks the name up in the file. `T3TERM_ORIGIN` doesn't help, because the flag comes first. | Runs, and the file isn't read. | Repair the file, or move it away to start over. The `environment` commands can't change it until then. |

## What t3term checks before it logs in

For a saved environment, in this order:

1. The T3 home is an absolute path, and the origin, saved or recorded, is an http origin on this machine: 127.0.0.0/8, `::1` or `localhost`. t3term logs in by running the server's own `t3` against the T3 home, and it never sends that login off the machine. An https origin on this machine stops the command with `ENVIRONMENT_TLS_UNSUPPORTED`. A program that builds a target with the t3term library gets the same check when it connects, before t3term reads the home.
2. The T3 home holds `userdata/environment-id`. t3term reaches no server until it has read it.
3. The server answers `/.well-known/t3/environment` at the origin itself. An answer that redirects stops the command with `T3_REDIRECTED`. An error status, or a body that isn't a whole descriptor, stops it with `T3_DESCRIPTOR_INVALID`. Nothing listening, or no answer within 2.5 seconds, stops it with `T3_SERVER_UNAVAILABLE`.
4. The `environmentId` the server reports is the id the T3 home holds. A server that belongs to another home, such as one that took over the port, gets no login.
5. The server speaks orchestration protocol 2.
6. t3term runs `t3 auth session issue --base-dir <T3 home>`. It finds `t3` from the pid in `server-runtime.json`, and uses that pid only when the record names the same origin t3term connected to. `T3TERM_T3_COMMAND` replaces the program, and t3term still passes the environment's T3 home as `--base-dir`.

T3 writes `environment-id` in the state directory and reports the same id in its descriptor (apps/server/src/environment/ServerEnvironment.ts:89-177 at T3 f570bd21). A server started with `--base-dir` or `T3CODE_HOME` keeps its state in `userdata` under that home, even as a dev server (apps/server/src/cli/config.ts:319-332 and apps/server/src/config.ts:139-142).

t3term reads `environment-id` and `server-runtime.json` once per command, on a blocking thread, so Ctrl-C still works while a slow disk holds the read up. It reads each only as a regular file of at most 16 KiB. A directory, a FIFO or a larger file counts as unreadable, so t3term never waits on a pipe or fills memory. Waiting can't cancel a read that never finishes. That read keeps its thread, and t3term exits without it.

## Redirects and proxies

t3term sends each HTTP request straight to the server it chose. It follows no redirect. When a server answers the descriptor or any later request with a redirect, the saved login check included, the command stops with `T3_REDIRECTED`, and the request, its token and its body never reach the server the redirect names. At T3 f570bd21 no route t3term calls redirects. t3term uses no proxy either, so `HTTP_PROXY`, `HTTPS_PROXY` and `ALL_PROXY` don't apply, and a request for a server on this machine stays on it. The WebSocket already worked this way.

Discovery without a saved environment follows the same rule. A server that `T3TERM_ORIGIN` names or `T3CODE_HOME` records and that answers with a redirect is passed over, as one that doesn't answer is, and t3term tries the next one.

## Errors

Every error uses an existing exit code. With `--json` it prints `{"ok": false, "error": {"code": ..., "message": ...}}`.

| Code | Exit | When |
| --- | --- | --- |
| `ENVIRONMENT_NAME_INVALID` | 2 | A name breaks the naming rule. |
| `ENVIRONMENT_HOME_INVALID` | 2 | `--t3-home` is empty or isn't UTF-8, or a target a program built has a relative T3 home. |
| `ENVIRONMENT_ORIGIN_INVALID` | 2 | An origin isn't an http or https URL, or has a user, a path, a query or a fragment. |
| `ENVIRONMENT_INVALID` | 2 | A saved environment has no kind, a relative `t3Home` or an origin that isn't text. |
| `ENVIRONMENT_NOT_FOUND` | 3 | No environment of that name is saved, or the default names one that isn't. |
| `ENVIRONMENT_EXISTS` | 4 | `add` or `rename` to a name already saved. |
| `ENVIRONMENT_IS_DEFAULT` | 4 | `remove` of the default. |
| `ENVIRONMENT_NOT_LOCAL` | 4 | The saved or recorded origin is off this machine, or the recorded one isn't an origin. |
| `ENVIRONMENT_TLS_UNSUPPORTED` | 4 | The origin, given to `add`, saved or recorded, is on this machine but uses https. |
| `ENVIRONMENT_UNSUPPORTED` | 4 | A saved environment's kind isn't `local`. |
| `T3_ENVIRONMENT_UNVERIFIED` | 4 | The T3 home holds no `userdata/environment-id`. |
| `T3_ENVIRONMENT_MISMATCH` | 4 | The server reports another environment id. |
| `T3_DESCRIPTOR_INVALID` | 4 | A saved environment's server answered the descriptor with an error status, or with a body that isn't a whole descriptor: one that doesn't parse, lacks a field or stops partway. |
| `T3_REDIRECTED` | 4 | A saved environment's server answered the descriptor with a redirect, or a server answered a later request with one, the saved login check included. |
| `T3_PROTOCOL_UNSUPPORTED` | 4 | The server isn't on orchestration protocol 2. |
| `T3_SERVER_UNAVAILABLE` | 5 | Nothing listens at the origin, the server closed the connection without an answer or didn't answer within 2.5 seconds, no origin is saved or recorded, or `server-runtime.json` can't be read. |
| `T3_AUTH_UNAVAILABLE` | 5 | No recorded pid to find `t3` from, and no `T3TERM_T3_COMMAND`. |
| `ENVIRONMENTS_UNREADABLE` | 1 | `environments.json` doesn't read or parse. |
| `ENVIRONMENTS_NOT_SAVED` | 1 | A change couldn't be written. |

`doctor` adds an `environment` check with the name and T3 home when it checks a saved environment, then runs its other checks against that environment's server.

## Logins

The saved login in the Keychain is keyed by the environment id the server reports, as before. A saved environment and discovery share one login when they reach the same server, and renaming or removing an environment leaves its login alone.

Before it reuses a saved login, t3term asks the server at `/api/auth/session` whether it still accepts it. Only an answer of `authenticated: false`, which T3 gives for an expired or revoked login, makes t3term issue a new login and revoke the old one. T3's own client renews on that answer alone (apps/server/src/auth/EnvironmentAuth.ts:770-789 and packages/client-runtime/src/state/session.ts:65-66 at T3 f570bd21). Any other result stops the command with its error before t3term issues, saves, deletes or revokes a login: a redirect, an error status, an answer without `authenticated`, or no answer. The saved login stays as it was.

`t3term --environment NAME logout` revokes that environment's saved login and removes it from the Keychain. When the environment's server is stopped, t3term can't revoke the login, so it removes that environment's login alone, found by the id its T3 home holds. Stopped means one of these:

- The system refused the connection at the environment's origin. A record left behind by a server that crashed still counts when it names that origin.
- No origin is saved and the home records no server. T3 removes `server-runtime.json` when its server stops (apps/server/src/serverRuntimeState.ts:72-113).

Anything else keeps the login and reports the error: a server that answers with an error status, a body that isn't a descriptor, a redirect or another environment's id; a server that took the connection and didn't answer, which may be busy rather than stopped; a home whose record can't be read or names another origin; and a target t3term refuses.

The removal holds the lock a login holds while it checks, issues and saves this environment's login. When another t3term holds it, logout removes nothing and fails with `T3_AUTH_FAILED`, since that process may be saving a login for a server that started after logout checked. Logout doesn't wait for the lock. Run it again once the other command finishes, and it checks the server again. The lock can't catch a login that starts and finishes between logout's check and its lock. When t3term can't open or lock the lock file, logout also fails and keeps the login.

With a saved default, `t3term logout` acts on the default the same way. Without either, it works as before: with no server running, it removes every t3term login.

With `T3TERM_NO_SAVED_LOGIN=1`, t3term keeps no login. It reads, saves and deletes nothing in the Keychain, so `logout` finds no saved login. Each command uses a session that lasts one run, revoked against the same T3 home it was issued from.

## The file

```json
{
  "default": "work",
  "environments": {
    "lab": {
      "kind": "local",
      "t3Home": "/Users/you/t3-lab",
      "origin": "http://127.0.0.1:4000"
    },
    "work": {
      "kind": "local",
      "t3Home": "/Users/you/.t3"
    }
  }
}
```

It holds names, paths and origins, and never a login or a token. A change follows `settings.json`: it holds a lock on `environments.json.lock` from its read to its write, writes `environments.json.tmp` and renames it into place, and replaces the file a symlink names rather than the link. A change that fails writes nothing. Keys t3term doesn't know, at the top or in an environment, are written back as they were read.

## Not supported yet

- https. A local T3 server listens on plain http, and t3term is built without TLS, so it refuses an https origin on this machine when you add it, when a saved one loads and when a T3 home records one, all before it reaches a server. `T3TERM_ORIGIN` isn't checked this way: discovery passes an https origin over as one that didn't answer.
- Remote environments. t3term refuses an origin off this machine, and a saved kind other than `local`, before it reaches any server. T3 pairs a remote client by exchanging a one-time `t3 pair` credential at the server's `/oauth/token` for a bearer token. t3term's HTTP client is built without TLS, and its saved logins don't hold a remote token yet.
- T3 Connect. The next section says what T3's clients do and what t3term lacks.
- A picker in the TUI. The TUI opens on the environment the command chose, and the sidebar names it as before.
- A dev server started with `--dev-url` and no base dir keeps its state in `~/.t3/dev`, which a saved environment doesn't read. Discovery still finds it.
- `remove` doesn't revoke the environment's login. Run `t3term --environment NAME logout` first.

## T3 Connect

T3 Connect reaches an environment through T3's relay. t3term doesn't do it yet. This section reads the source at T3 f570bd21 for what T3's own clients do and what stands in t3term's way. No part of it has been tried against T3's cloud.

### How T3's clients connect

1. The client signs in to T3's Clerk instance and asks Clerk for a token from the `t3-relay` JWT template, whose audience is `t3-code-relay` (docs/operations/connect-setup.md:50-61). The web app signs in in the browser. The desktop app signs in through Clerk's Electron integration, which returns to `t3code://app/`, and the Android app through `clerk://<applicationId>.callback`. T3's Clerk administrator puts each of those redirects on an allowlist (connect-setup.md:63-88).
2. It trades that token at the relay's `POST /v1/client/dpop-token` for an access token bound to a DPoP key it holds, good for 30 minutes. The request names a `client_id`, `t3-web` from the web app or `t3-mobile` from the mobile app, which decides the scopes the token may carry (packages/contracts/src/relay.ts:747-770, infra/relay/src/auth/RelayTokens.ts:63-81, apps/web/src/cloud/managedRelayLayer.ts:80).
3. With that token it asks the relay for one environment, `POST /v1/environments/:environmentId/connect`, and gets the environment's endpoint and a one-time bootstrap credential.
4. It reads the endpoint's descriptor and checks the environment id, then exchanges the bootstrap credential at the environment's `/oauth/token` for an access token bound to the same key (packages/client-runtime/src/authorization/service.ts:278-360).

T3's CLI signs in another way. `https://app.t3.codes/connect` signs the user in, then sends the browser to Clerk's authorize page for T3's public CLI OAuth app, which returns the code to the CLI's listener at `http://127.0.0.1:34338/callback` (connect-setup.md:37-48, apps/web/src/cloud/connectCliAuth.ts:12-46). The CLI gets a Clerk OAuth access token, not a template token.

### What t3term lacks

- A token the exchange takes. The relay's account routes, such as listing and linking environments, take a template token or a Clerk OAuth access token (infra/relay/src/http/Api.ts:1656-1698). The DPoP exchange in step 2 takes only a Clerk session token with the relay's audience (Api.ts:1020-1025). So an OAuth app, T3's or one registered for t3term, could list environments but not connect to one. Connecting takes a Clerk session in T3's instance, or a relay that accepts an OAuth token at the exchange.
- A sign-in of its own. T3's Clerk administrator registers each way in: the CLI's OAuth app and its loopback redirect, and the desktop and Android redirects. t3term won't use T3's CLI client id or present itself as T3's web app. An OAuth app or a sign-in redirect for t3term is T3's to register, and only a redirect gives t3term a session.
- A client id. `client_id` is `t3-web` or `t3-mobile` and nothing else. It is neither a secret nor a registration, since it only picks scopes, but t3term would send neither, so an id for t3term is a change to the relay's contract and its scope table.
- Its own parts. DPoP needs an ES256 key and a signed proof for each request. The relay and a remote endpoint need TLS, which t3term's HTTP client and WebSocket are built without. The tokens need the Keychain.

A paired remote environment needs none of T3's cloud. `t3 pair` issues a one-time credential, and the environment's `/oauth/token` exchanges it for a bearer token (packages/client-runtime/src/authorization/remote.ts:115-140). For that t3term needs TLS, a saved token and the descriptor check above, and no change to T3.
