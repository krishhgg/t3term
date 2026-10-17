"""The t3v driver: the real t3term TUI in tmux against a fake T3 server on 127.0.0.1.

  cli.py, commands.py   the command line and the commands that start, drive, check and end a run
  build.py              which t3term a run starts, `t3v build` and the renderer's build
  runs.py               run and evidence directories, nonce checks, pids and the control route
  tmux.py               t3term in tmux: launch, keys, mouse reports, captures, waits, pipe sink
  server.py, auth.py    the fake server: HTTP, WebSocket, Effect RPC, sessions, tickets, scopes
  orchestration.py      the orchestration methods every scenario serves
  world.py              the Thread class and the label presets
  controls.py           the commands that change the world through the run's control routes
  scenarios/            each scenario's world, methods and commands
  report.py, rules.py   what `status`, `check` and `doctor` read, and the rules `check` holds
  text.py               control characters, widths, time and errors
"""
