# Environment connections

t3term can save named environments and send a command to one of them with `--environment NAME`. Each environment is a T3 home on this machine, the directory a T3 server keeps its state in, and optionally the loopback origin of that server. A saved environment is strict: t3term talks to its server and no other, and logs in only once that server proves it belongs to the T3 home.

Remote environments, pairing and T3 Connect aren't supported yet. The last section says what t3term refuses and why.

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
- `--origin` is where the server listens, such as `http://127.0.0.1:3773`. Leave it out and t3term reads the origin from `server-runtime.json` each time it connects, so a server that restarts on a new port is still found. Give it when you start the server on a fixed port.
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

A default that names an environment no longer saved stops every command that names none, with `ENVIRONMENT_NOT_FOUND`. So does an `environments.json` that doesn't parse, with `ENVIRONMENTS_UNREADABLE`. In both cases t3term can't tell which server you chose, so it doesn't guess. Setting `T3TERM_ORIGIN` or passing `--environment` gets past either one.

## What t3term checks before it logs in

For a saved environment, in this order:

1. The origin, saved or recorded, is on this machine: 127.0.0.0/8, `::1` or `localhost`. t3term logs in by running the server's own `t3` against the T3 home, and it never sends that login off the machine.
2. The T3 home holds `userdata/environment-id`. t3term reaches no server until it has read it.
3. The server answers `/.well-known/t3/environment` at the origin.
4. The `environmentId` the server reports is the id the T3 home holds. A server that belongs to another home, such as one that took over the port, gets no login.
5. The server speaks orchestration protocol 2.
6. t3term runs `t3 auth session issue --base-dir <T3 home>`. It finds `t3` from the pid in `server-runtime.json`, and uses that pid only when the record names the same origin t3term connected to. `T3TERM_T3_COMMAND` replaces the program, and t3term still passes the environment's T3 home as `--base-dir`.

T3 writes `environment-id` in the state directory and reports the same id in its descriptor (apps/server/src/environment/ServerEnvironment.ts:89-177 at T3 f570bd21). A server started with `--base-dir` or `T3CODE_HOME` keeps its state in `userdata` under that home, even as a dev server (apps/server/src/cli/config.ts:319-332 and apps/server/src/config.ts:139-142).

## Errors

Every error uses an existing exit code. With `--json` it prints `{"ok": false, "error": {"code": ..., "message": ...}}`.

| Code | Exit | When |
| --- | --- | --- |
| `ENVIRONMENT_NAME_INVALID` | 2 | A name breaks the naming rule. |
| `ENVIRONMENT_HOME_INVALID` | 2 | `--t3-home` is empty or isn't UTF-8. |
| `ENVIRONMENT_ORIGIN_INVALID` | 2 | An origin isn't http or https, or has a user, a path, a query or a fragment. |
| `ENVIRONMENT_INVALID` | 2 | A saved environment has no kind, a relative `t3Home` or an origin that isn't text. |
| `ENVIRONMENT_NOT_FOUND` | 3 | No environment of that name is saved, or the default names one that isn't. |
| `ENVIRONMENT_EXISTS` | 4 | `add` or `rename` to a name already saved. |
| `ENVIRONMENT_IS_DEFAULT` | 4 | `remove` of the default. |
| `ENVIRONMENT_NOT_LOCAL` | 4 | The saved or recorded origin is off this machine. |
| `ENVIRONMENT_UNSUPPORTED` | 4 | A saved environment's kind isn't `local`. |
| `T3_ENVIRONMENT_UNVERIFIED` | 4 | The T3 home holds no `userdata/environment-id`. |
| `T3_ENVIRONMENT_MISMATCH` | 4 | The server reports another environment id. |
| `T3_PROTOCOL_UNSUPPORTED` | 4 | The server isn't on orchestration protocol 2. |
| `T3_SERVER_UNAVAILABLE` | 5 | No origin is saved or recorded, or the server didn't answer. |
| `T3_AUTH_UNAVAILABLE` | 5 | No recorded pid to find `t3` from, and no `T3TERM_T3_COMMAND`. |
| `ENVIRONMENTS_UNREADABLE` | 1 | `environments.json` doesn't read or parse. |
| `ENVIRONMENTS_NOT_SAVED` | 1 | A change couldn't be written. |

`doctor` adds an `environment` check with the name and T3 home when it checks a saved environment, then runs its other checks against that environment's server.

## Logins

The saved login in the Keychain is keyed by the environment id the server reports, as before. A saved environment and discovery share one login when they reach the same server, and renaming or removing an environment leaves its login alone.

`t3term --environment NAME logout` revokes that environment's saved login and removes it from the Keychain. When the environment's server isn't running, t3term removes that environment's login only, using the id its T3 home holds, and can't revoke it. When the check fails any other way, such as a server that reports another environment id, logout removes nothing and reports the error. With a saved default, `t3term logout` acts on the default the same way. Without either, it works as before: with no server running, it removes every t3term login.

A session that lasts one run, with `T3TERM_NO_SAVED_LOGIN=1`, is revoked against the same T3 home it was issued from.

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

- Remote environments. t3term refuses an origin off this machine, and a saved kind other than `local`, before it reaches any server. T3 pairs a remote client by exchanging a one-time `t3 pair` credential at the server's `/oauth/token` for a bearer token, and t3term has no secret store for that token yet.
- T3 Connect. Signing in to T3's cloud and reaching an environment through its relay need a client the relay accepts, and t3term isn't one yet.
- A picker in the TUI. The TUI opens on the environment the command chose, and the sidebar names it as before.
- A dev server started with `--dev-url` and no base dir keeps its state in `~/.t3/dev`, which a saved environment doesn't read. Discovery still finds it.
- `remove` doesn't revoke the environment's login. Run `t3term --environment NAME logout` first.
