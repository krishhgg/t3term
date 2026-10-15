#!/usr/bin/env python3
"""Run the real t3term TUI and its terminal drawer against a fake T3 server on 127.0.0.1.

Review tooling for the terminal drawer, PR 41 (https://github.com/krishhgg/t3term/pull/41).
docs/terminal-pane.md says how GPT-6.1 Sol runs it. Claude Opus 5.5 adapted it from the fixture
of the Lineage check (docs/lineage-verification.md), which stayed outside the repository, and
never ran or parsed it, so Sol's first run is its first. What is new is T3's terminal:
terminal.attach, terminal.write and terminal.resize behind the terminal scopes, T3's output
window, a fake shell that answers what the drawer types, and `term` controls that change a
terminal the way T3 or the desktop would.

The rules it keeps:

- Fakes only. Every project, thread, terminal, session and line of output is invented. The
  server listens on 127.0.0.1 alone, runs no command and no shell, and refuses every
  orchestration command. Nothing reaches a real T3 server.
- The Python standard library only, 3.9 or newer, and tmux.
- No real HOME, Keychain or credential. t3term runs under `env -i` with HOME and T3CODE_HOME
  inside the fixture directory and T3TERM_NO_SAVED_LOGIN=1, so it never sees your ~/.t3 or your
  Keychain. tmux runs on the run's own socket with `-f /dev/null`, so it reads no tmux.conf.
  The helper reads and writes nothing outside the fixture directory but the link below, and it
  never opens a .env file.
- No secret in the log or on the screen. Session tokens, tickets and the control key never
  reach logs/events.jsonl or anything the helper prints. A write logs its length and a hash,
  never what was typed.
- Cleanup stays inside the run. Everything a run makes lives in one directory,
  /tmp/t3term-pr41-fixture-XXXXXXXX, which mkdtemp makes, with a marker file holding a random
  nonce that the directory's fixture.json repeats. Each command checks the directory's name,
  its parent and its nonce before it acts on a run. `stop` signals only the pids the run
  recorded, each only while it still runs the command the run started, and stops only the tmux
  server on the run's socket. `cleanup` stops the run the same way and then deletes only its
  directory. The link /tmp/t3term-pr41-fixture-current names the newest run. A start replaces
  it only when it links to a fixture directory, and `cleanup` removes it only when it links to
  the run it deletes.

What it starts, all in the run's fixture directory:

- A fake T3 server on 127.0.0.1 (a random port). It serves the environment descriptor, the auth
  routes t3term calls, the orchestration HTTP routes and Effect RPC over /ws: the shell and
  thread subscriptions, `subscribeWorktreeSetup` (always null), `server.getConfig`,
  `subscribeServerConfig` (one ready Codex), and terminal.attach, terminal.write and
  terminal.resize.
- A `t3` stand-in for `t3 auth session issue|revoke`, found through T3TERM_T3_COMMAND. It asks the
  fixture server for a session over a loopback control route guarded by a random key in the
  fixture directory.
- t3term itself, in a detached tmux session on the run's socket (`tmux -S DIR/tmux.sock`), with
  NO_PROXY set for loopback.

What the fixture follows from T3 at f570bd2 (packages/contracts/src/terminal.ts,
apps/server/src/terminal/Manager.ts and OutputProtocol.ts, apps/server/src/auth/RpcAuthorization.ts):

- Scopes. A socket carries the scopes of the session whose ticket opened it. terminal.attach,
  write, resize, open, clear, restart and close need terminal:operate, terminal.observe and the
  two terminal subscriptions need terminal:read, and the orchestration and config RPCs need
  orchestration:read or orchestration:operate. A request without its scope fails with
  EnvironmentAuthorizationError and counts in scopeViolations.
- /api/auth/session answers {authenticated, scopes, permissions}. `auth permissions` changes what
  `permissions` says for terminal sessions.
- terminal.attach. Without cwd, a missing terminal fails with TerminalSessionLookupError
  {threadId, terminalId}. With cwd, a missing terminal starts: the session counts its events from
  0, its started event is 1, the stream's snapshot is 1 with the history a close kept, and the
  banner and prompt follow as output. An existing terminal ignores the launch. The snapshot
  comes first on the stream, then each event.
- The window. On each attach stream the server sends while fewer than 8 chunks and 64 KiB (the
  compact JSON of each Chunk message) wait for an Ack, and each Ack frees the oldest.
- Output goes in events of at most 4 KiB, at most 4 events to a chunk. History keeps the last
  5,000 lines and 8 MiB, counting bytes in UTF-8 as Buffer.byteLength counts them, and trims as
  BoundedTerminalHistory trims: whole lines from the front, then whole code points. The DSR, CPR
  and DA queries T3 strips never reach it (a simpler match than T3's).
- terminal.write: a missing terminal fails with TerminalSessionLookupError, an exited one succeeds
  and does nothing, and any other that isn't running fails with TerminalNotRunningError.
  terminal.resize does nothing for a missing terminal or one that isn't running.
- The label is "Terminal 1" while the shell is idle, and the running child's command while there
  is one, cut at 128 UTF-16 code units the way T3 cuts it.
- closed takes the event count plus one and removes the terminal, and attach streams stay open.
  restart clears the history and counts on, or counts from 0 when the terminal was gone. A
  started event reaches attach streams as a snapshot.

What isn't T3, and says so where it appears: the stranger, duplicate, old, malformed,
ignore-window and oversize controls, a project with a blank workspace root, `--starting` (T3
shows starting only while the PTY spawns), and the fake shell.

The world. Project proj-term, Terminal Desk, at /fixture/terminal-desk, and proj-rootless,
Rootless Desk, whose workspaceRoot is blank (not T3).

  thr-running   Running: a shell with history     term-1 running, colored history
  thr-fresh     Fresh: no terminal yet            worktree /fixture/terminal-desk/.t3/worktrees/fresh
  thr-local     Local: no worktree                no worktree, no term-1
  thr-exited    Exited: the shell exited with 1   term-1 exited with code 1
  thr-failed    Failed: the shell never started   term-1 in status error, no history
  thr-busy      Busy: npm run dev                 term-1 running a dev server until Ctrl+C
  thr-huge      Huge: 8 MiB of history            4,990 lines, about 27 MiB as JSON
  thr-rootless  Rootless: no workspace root       in proj-rootless, no term-1

The fake shell. Commands: help, ls, colors, echo, env, seq N, flood MIB, clear, size, showkeys,
cursorkeys, fullscreen [--mouse], dsr, sync [--hang], paste on|off, hostile, sleep N, dev,
exit [N] and kill [SIGNAL]. At the prompt, a key it has no use for (Tab, an arrow, Esc, a
function key, Ctrl+X) prints a line such as `[key ^I  09]`, so you can see what reached it.
Ctrl+C prints ^C and Ctrl+D on an empty line exits. Its prompt turns bracketed paste on, as zsh's
does, and a bracketed paste prints `[paste N characters, LF n, CR n, sha256 XXXXXXXXXXXX]`.

Checks. `F check` exits 1 and names each rule the run has broken so far, and exits 0 when all
held. The rules hold at every step, so it can run after any of them: no request without its
scope, no attach with a field the drawer never sends, no launch that differs from the thread's,
no terminal RPC the drawer never sends, at most 8 chunks unacked, no Ack for a chunk or stream
that isn't there, no write before the last one's answer, no write over 65,536 UTF-16 units, none
of the hostile controls or their hidden text in what t3term wrote or on screen, and the
duplicate event at most once per `term duplicate`. What each step should show beyond those
needs a reader and the captures. The walkthrough below says what to look for, and those
predictions aren't results until someone sees them.

Walkthrough. F is `python3 scripts/terminal-pane-fixture.py`, run from the worktree root.
`F term list` shows each terminal and the header the drawer should show for it, and a failed
shell's last row. `F status` sums up the log.

 1. Nothing before the drawer. `F start --bin target/debug/t3term`. Open Running with
    `F click --text "Running: a shell"`. `F status`: terminalSessionsIssued 0 and no terminal RPC.
 2. Show it. `F keys F4`. t3term issues one session with terminal:read and terminal:operate,
    checks it at /api/auth/session, opens a second socket and sends one terminal.attach with
    threadId and terminalId alone. Once the snapshot comes, one terminal.resize brings the size
    the drawer drew. The header reads ` Terminal · Terminal 1 · running` with `F4 hide` at its
    right, and the screen shows the history in color with the cursor at the prompt.
    `F capture running`.
 3. Type. `F type "echo hello"` prints hello. `F keys Tab BTab Escape C-c C-x`: each reaches the
    shell. `F type showkeys`, `F keys Left F5 M-x C-c Home`, then `F keys C-d`. `F type
    cursorkeys` and `F keys Up` shows `^[OA`. `F keys F6` moves the keyboard off the drawer, and
    Tab then moves the TUI's focus again.
 4. Hide and show. With the keyboard off the drawer, `F type --no-enter "a draft"` in the
    composer, then `F keys F6` back to the drawer and `F keys F4`: one
    terminal Interrupt, and `F term list` shows the shell still running. `F keys F4` again: a new
    attach by identity, a new snapshot, and the draft is still there.
 5. Start. Open Fresh and `F keys F4`: `This thread has no terminal yet.` and Start terminal.
    `F keys Enter`: one attach with cwd /fixture/terminal-desk/.t3/worktrees/fresh, its
    worktreePath, and T3CODE_PROJECT_ROOT and T3CODE_WORKTREE_PATH. `F type env` shows them, and
    launchMismatches stays empty. Open Local and `F click --text "Start terminal"`: cwd is the
    workspace root with no worktreePath and T3CODE_PROJECT_ROOT alone. Open Rootless and press
    Enter: "T3 hasn't said where this thread's project is…", and no attach with a launch.
 6. A start T3 refuses. `F term close --thread thr-fresh --delete-history`, `F term missing-cwd
    on`, F4 twice on Fresh and Enter: stopped with "The shell's directory doesn't exist.". Enter
    attaches again by identity and Start terminal comes back. `F term missing-cwd off`.
 7. Read-only. Exited reads `exited with 1, read-only`. Typing there notes "The shell exited, so
    this terminal is read-only." and the write count holds. Failed reads `failed, read-only`
    over an empty screen, since its snapshot carries no message.
 8. Busy reads ` Terminal · npm run dev · running`. C-c prints ^C and the title goes back to
    Terminal 1.
 9. Live changes on Running, one at a time, with a capture after each:
    `F term activity --thread thr-running --label "cargo test"`, then `--preset long` (the title
    is cut and the state stays), `--preset hostile` (cleaned and cut to 64 columns) and
    `--preset idle`; `F term clear --thread thr-running`;
    `F term exit --thread thr-running --code 3`; `F term restart --thread thr-running`;
    `F term error --thread thr-running --message "pty broke"`, after which the header reads
    `failed, read-only`, the screen's last row reads ` pty broke` with blanks after it, whatever
    the screen had there, and a notice reads
    "The shell failed: pty broke"; `F term restart` again;
    `F term close --thread thr-running` (closed, read-only, and the stream stays open); then
    `F term start --thread thr-running`, the desktop opening it again from 0: the drawer takes
    the new shell, with the history the close kept, and takes input.
10. Events to skip. `F term stranger --thread thr-running`, `F term old --thread thr-running` and
    `F term duplicate --thread thr-running`. duplicateOnScreen is 1, and screenLeaks stays 0.
    Then `F term activity --preset split`: T3's cut at 128 UTF-16 units leaves half an emoji,
    which JSON writes as a lone \\ud83d escape. serde_json refuses that, and src/rpc.rs:221 skips
    any frame it can't parse. If the header keeps its old title and `F term list` shows one
    chunk unacked from then on, the drawer lost that event and T3 lost a window slot.
    `--preset idle` after shows whether later events still arrive. docs/terminal-pane.md lists
    this as a known limit until the RPC client's fix lands.
11. Malformed. `F term malformed --thread thr-running`: the drawer stops, naming the event and
    field, and T3 gets one Interrupt. Enter attaches again.
12. Limits. `F term ignore-window --thread thr-running --chunks 20`, and after Enter,
    `F term oversize --thread thr-running`: each stops the drawer, and no attach follows until
    Enter.
13. The window. `F type "flood 16"`: maxUnacked stays at most 8, acks catch up with chunks once
    it ends, and the screen ends at `flood: done`. Then `F type "flood 64"` and, while it runs,
    `F keys C-c`: the key goes out between two batches, so ^C shows and the flood stops with no
    `flood: done`. Only the output T3 had queued, at most 32 chunks, comes after it.
14. Huge. Open Huge and F4: the last lines show. `F keys S-PPage` scrolls and the header's right
    shows `↑ N rows`, or `↑ 1 row` for one. `F click --text "huge 04980" --button wheel-up`
    scrolls too.
15. Paste. `F paste --text "hello paste"`: the shell prints `[paste 11 characters ...]` with the
    sha the helper printed. `F paste --bytes 262132` goes whole, as 262,144 bytes with the
    brackets. `F paste --bytes 262133` is refused whole with no write and the notice "t3term
    pasted nothing: the paste is 257 KiB, and the terminal takes at most 256 KiB at once."
    `F type "paste off"`, then `F paste --text "echo one"`: it lands on the prompt line with no
    [paste] report, as typed text does, and `F keys Enter` runs it. `F type "paste on"` turns it
    back.
16. Replies. `F type dsr`: two replies show. `F term close --thread thr-local`,
    `F term start --thread thr-local --starting 8`, F4 on Local: `starting`, keys refused, and
    `F term output --thread thr-local --escapes "\\x1b[6n"` gets no write. Once running, typing works.
17. Order. `F term slow 800`, then `F keys a b c d e f --gap 0`: writesOverlapped 0, and the keys
    go in fewer writes than keys. `F term slow 0`.
18. Input failure. `F term fail-writes on`, then a key: `input stopped` and a notice. F4 twice
    attaches again. `F term fail-writes off`.
19. Resize. `F resize 120 36`: one terminal.resize about 100 ms after the frame that drew the new
    size. Several resizes in a row: only the last size goes. `F type size` says what the shell
    got. `F term fail-resize on` and a resize: a notice, and that size isn't sent again.
20. Full screen. `F type fullscreen`: arrows move the dot, a wheel turn over the drawer sends
    arrows, q quits. `F type "fullscreen --mouse"`: the wheel sends nothing.
21. Hostile. `F mark before-hostile`, `F type hostile`, `F status`: controlLeaksSinceMark all 0
    and screenLeaks all 0.
22. Sync. `F type sync` shows its lines once the update ends. `F type "sync --hang"` shows them
    at the drawer's deadline, and nothing keeps waking after.
23. Reconnect. `F drop --hold 3`: `reconnecting, try N of 8`, then running with a new snapshot
    and one terminal.resize with the size drawn. The write count doesn't jump: no key goes
    again. `F drop --hold 60`: stopped after 8 tries, about 36 s. Enter attaches again.
24. Permissions. `F auth permissions extra`, `F restart`, open Running, F4: stopped with
    "T3 at … lists the terminal session's permissions as …". Enter: refused again, and no new
    session is issued. `F auth permissions normal`, `F restart`.
25. Slow attach. `F term hold-attach 5`, then F4: `attaching` and "Connecting to the thread's
    terminal…", and a key notes "The terminal is still attaching.".
26. Dialogs. With the drawer open, `F keys F6` until the keyboard is off it, then `F keys M-b`
    (the drawer would send it to the shell): the details take F4, F6, keys and clicks until
    Escape.
27. Sizes. `F resize 80 24`, `F resize 40 12`, `F resize 16 8`, with a capture at each. Where
    the drawer shows, its header keeps the state: a narrow row drops `F4 hide`, then cuts the
    title or leaves it out, then gives the state its short form.
28. Quit. `F check`, then `F stop` sends C-q, and t3term revokes both sessions
    (sessionsRevoked: login 1, terminal 1).

Lifecycle, from the worktree root:

  cargo build                                         # by Sol
  python3 scripts/terminal-pane-fixture.py start --bin target/debug/t3term
  env -u TMUX tmux -S /tmp/t3term-pr41-fixture-current/tmux.sock attach -r -t t3term-pr41
  python3 scripts/terminal-pane-fixture.py status
  python3 scripts/terminal-pane-fixture.py check    # exits 1 when the run broke a rule
  python3 scripts/terminal-pane-fixture.py stop     # or cleanup, which deletes the directory

The server stops itself after --max-seconds (default 7200), quitting t3term first.
"""

from __future__ import annotations

import argparse
import base64
import codecs
import collections
import copy
import hashlib
import hmac
import http.server
import itertools
import json
import os
import platform
import queue
import re
import secrets
import shlex
import shutil
import signal
import socket
import socketserver
import struct
import subprocess
import sys
import tempfile
import threading
import time
import unicodedata
import urllib.error
import urllib.parse
import urllib.request
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path

SCRIPT = Path(os.path.realpath(__file__))
PREFIX = 't3term-pr41-fixture-'
MARKER = '.t3term-pr41-fixture'
CURRENT = Path('/tmp/t3term-pr41-fixture-current')
SESSION = 't3term-pr41'
WS_GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11'
ENVIRONMENT_ID = 'env-fixture-pr41'
SERVER_VERSION = '0.0.46-fixture.pr41'
FIRST_SEQUENCE = 100
MAX_FRAME = 8 * 1024 * 1024
NAME = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,80}$')
SECOND, MINUTE, HOUR, DAY = 1000, 60 * 1000, 3600 * 1000, 86400 * 1000

INSTANCE, MODEL = 'codex', 'gpt-6'
EFFORT = {'id': 'effort', 'label': 'Effort', 'type': 'select',
          'options': [{'id': 'low', 'label': 'Low'},
                      {'id': 'medium', 'label': 'Medium', 'isDefault': True},
                      {'id': 'high', 'label': 'High'}]}
# ORCHESTRATION_V2_PROJECTION_SCHEMA_VERSION in the pinned nightly, which the shell snapshot carries.
SCHEMA_VERSION = 2

DESK_ROOT = '/fixture/terminal-desk'
TERMINAL_ID = 'term-1'
TERMINAL_SCOPES = ('terminal:read', 'terminal:operate')
# T3's output window on terminal.attach and subscribeTerminalEvents (OutputProtocol.ts).
WINDOW_CHUNKS = 8
WINDOW_BYTES = 64 * 1024
# BoundedTerminalHistory's limits and a new shell's size (Manager.ts:97-106). T3 counts the bytes
# as Buffer.byteLength does, in UTF-8, and adds text in chunks of at most 16 Ki UTF-16 units,
# where History adds pieces of at most 16 Ki characters.
HISTORY_LINES = 5000
HISTORY_BYTES = 8 * 1024 * 1024
HISTORY_PIECE = 16 * 1024
DEFAULT_COLS, DEFAULT_ROWS = 120, 30
# TerminalWriteInput's longest data, and the longest label, in UTF-16 code units.
WRITE_LIMIT = 65536
LABEL_LIMIT = 128
# How the fixture cuts output: a PTY read is at most this long here, and a chunk holds at most
# this many events.
OUTPUT_PIECE = 4096
EVENTS_PER_CHUNK = 4
# t3term's drawer limits (src/tui/terminal.rs), for what the helpers print.
DRAWER_CHUNKS = 16
DRAWER_BYTES = 56 << 20
PASTE_BYTES = 256 << 10
PASTE_START, PASTE_END = '\x1b[200~', '\x1b[201~'
PASTE_BRACKETS = len(PASTE_START) + len(PASTE_END)
# The longest line the fake shell keeps at its prompt.
LINE_LIMIT = 2000
# The fake shell's pids, and the sequence the seeded terminals have counted to.
FIRST_PID = 41001
SEEDED_SEQUENCE = 120

# The scope each RPC needs (RpcAuthorization.ts). A tag missing here needs none in the fixture.
SCOPE_FOR = {
    'orchestration.subscribeShell': 'orchestration:read',
    'orchestration.subscribeThread': 'orchestration:read',
    'orchestration.getTurnItem': 'orchestration:read',
    'orchestration.dispatchCommand': 'orchestration:operate',
    'server.getConfig': 'orchestration:read',
    'subscribeServerConfig': 'orchestration:read',
    'subscribeWorktreeSetup': 'orchestration:read',
    'terminal.open': 'terminal:operate',
    'terminal.attach': 'terminal:operate',
    'terminal.observe': 'terminal:read',
    'terminal.write': 'terminal:operate',
    'terminal.resize': 'terminal:operate',
    'terminal.clear': 'terminal:operate',
    'terminal.restart': 'terminal:operate',
    'terminal.close': 'terminal:operate',
    'subscribeTerminalEvents': 'terminal:read',
    'subscribeTerminalMetadata': 'terminal:read',
}
# What the drawer never sends. Each one counts in unexpectedTerminalRpcs.
UNEXPECTED_TERMINAL = ('terminal.open', 'terminal.observe', 'terminal.clear', 'terminal.restart',
                       'terminal.close', 'subscribeTerminalEvents', 'subscribeTerminalMetadata')
PERMISSION_MODES = ('normal', 'extra', 'missing', 'absent')

# The queries T3 keeps out of history (shouldStripCsiSequence in Manager.ts), simplified.
QUERY = re.compile(r'\x1b\[[0-9;?]*n|\x1b\[[0-9;?]*R|\x1b\[[>0-9;?]*c')

CLIPBOARD = 'PR41-CLIPBOARD'
HOSTILE_OUTPUT = (
    '\x1b]52;c;' + base64.b64encode(CLIPBOARD.encode()).decode() + '\x07'
    '\x1b]0;PR41-WINDOW-TITLE\x07'
    '\x1b]2;PR41-WINDOW-TITLE\x1b\\'
    'hostile: a \x1b]8;;https://pr41-link.invalid/\x07hyperlink\x1b]8;;\x07 in a line\r\n'
    '\x1bPPR41-DCS\x1b\\'
    '\x1b_PR41-APC\x1b\\'
    '\x07'
    '\x9b0m'
    'hostile: done. Nothing above should reach your terminal, its title or your clipboard.\r\n')
HOSTILE_LABEL = ('\t\x1b[31mPR41-LABEL\x1b[0m \x07bel \x9b2J c1 \x1b]0;osc\x07 '
                 + 'L' * 300)
LABEL_PRESETS = {
    'cargo': 'cargo test',
    'long': 'node ' + 'x' * 200,
    'hostile': HOSTILE_LABEL,
    # 127 units and an emoji, so T3's cut at 128 units splits the emoji's surrogate pair.
    'split': 'a' * 127 + '\U0001f600',
}
# Byte strings that must never reach capture/raw.ansi: the controls t3term never writes, and the
# text the hostile command hides inside them.
LEAK_MARKERS = {
    'osc': b'\x1b]',
    'bel': b'\x07',
    'dcs': b'\x1bP',
    'apc': b'\x1b_',
    'c1Csi': b'\xc2\x9b',
    'clipboardText': CLIPBOARD.encode(),
    'clipboardBase64': base64.b64encode(CLIPBOARD.encode()),
    'windowTitle': b'PR41-WINDOW-TITLE',
    'linkTarget': b'pr41-link.invalid',
    'dcsText': b'PR41-DCS',
    'apcText': b'PR41-APC',
}
# Text that must never show on screen: the hostile command's hidden text, and the output of
# events the drawer must skip.
SCREEN_MARKERS = ('PR41-CLIPBOARD', 'PR41-WINDOW-TITLE', 'pr41-link.invalid', 'PR41-DCS',
                  'PR41-APC', 'STRANGER-TERM-2', 'STRANGER-THREAD', 'PR41-FUTURE', 'OLD-EVENT')
# The one marker that must show exactly once after `term duplicate`.
DUPLICATE = 'DUPLICATE-CHECK'
SIGNALS = {'HUP': 1, 'INT': 2, 'QUIT': 3, 'KILL': 9, 'TERM': 15}
THREADS = (
    # thread id, title, worktree name or None, project id
    ('thr-running', 'Running: a shell with history', 'running', 'proj-term'),
    ('thr-fresh', 'Fresh: no terminal yet', 'fresh', 'proj-term'),
    ('thr-local', 'Local: no worktree', None, 'proj-term'),
    ('thr-exited', 'Exited: the shell exited with 1', 'exited', 'proj-term'),
    ('thr-failed', 'Failed: the shell never started', 'failed', 'proj-term'),
    ('thr-busy', 'Busy: npm run dev', 'busy', 'proj-term'),
    ('thr-huge', 'Huge: 8 MiB of history', 'huge', 'proj-term'),
    ('thr-rootless', 'Rootless: no workspace root', None, 'proj-rootless'),
)


def now_ms() -> int:
    return int(time.time() * 1000)


def iso(ms: int) -> str:
    """The timestamp form T3 writes: 2026-10-08T00:06:52.769Z."""
    seconds, millis = divmod(int(ms), 1000)
    stamp = datetime.fromtimestamp(seconds, tz=timezone.utc).strftime('%Y-%m-%dT%H:%M:%S')
    return f'{stamp}.{millis:03d}Z'


def die(message: str, code: int = 1):
    print(f'fixture: {message}', file=sys.stderr)
    sys.exit(code)


def escape_controls(text: str):
    """`text` with each control character but a newline written as an escape such as \\x1b, and
    how many there were, so a control character never reaches this terminal."""
    out, count = [], 0
    for c in text:
        if c != '\n' and unicodedata.category(c) in ('Cc', 'Cs'):
            out.append(f'\\x{ord(c):02x}' if ord(c) < 0x100 else f'\\u{ord(c):04x}')
            count += 1
        else:
            out.append(c)
    return ''.join(out), count


def safe(text, limit: int = 80) -> str:
    """Any value as one printable row of at most `limit` characters."""
    shown, _ = escape_controls(str(text).replace('\n', ' '))
    return shown if len(shown) <= limit else shown[:limit - 1] + '…'


def zero_width(c: str) -> bool:
    return unicodedata.combining(c) > 0 or unicodedata.category(c) in ('Mn', 'Me', 'Cf')


def cells(text: str) -> int:
    """Columns `text` takes, counting a wide character as two and a combining one as none."""
    return sum(0 if zero_width(c) else 2 if unicodedata.east_asian_width(c) in ('W', 'F') else 1
               for c in text)


def shown(text, width: int) -> str:
    """`text` as label::cut in src/tui/label.rs shows it in `width` columns: `\\r\\n` and each
    tab, line break, VT, FF or NEL as a space, every other control dropped, and the rest cut
    with `…` when it takes more columns. The columns are cells' count, which is close to the
    drawer's but doesn't group clusters as it does."""
    clean = []
    for c in str(text).replace('\r\n', '\r'):
        if c in '\t\n\x0b\x0c\r\x85':
            clean.append(' ')
        elif unicodedata.category(c) not in ('Cc', 'Cs'):
            clean.append(c)
    if cells(''.join(clean)) <= width:
        return ''.join(clean)
    kept, used = [], 0
    for c in clean:
        if used + cells(c) > width - 1:
            break
        kept.append(c)
        used += cells(c)
    return ''.join(kept) + '…'


def utf16_len(text: str) -> int:
    return len(text.encode('utf-16-le', 'surrogatepass')) // 2


def utf8_len(text: str) -> int:
    """Buffer.byteLength's count: the UTF-8 bytes, with a lone surrogate as 3 bytes."""
    return len(text.encode('utf-8', 'surrogatepass'))


def utf16_cut(text: str, limit: int) -> str:
    """The first `limit` UTF-16 code units of `text`, as JavaScript's slice takes them, which can
    split a surrogate pair."""
    units = text.encode('utf-16-le', 'surrogatepass')
    if len(units) <= limit * 2:
        return text
    return units[:limit * 2].decode('utf-16-le', 'surrogatepass')


def sha(text: str) -> str:
    return hashlib.sha256(text.encode('utf-8', 'surrogatepass')).hexdigest()


def encode(value) -> bytes:
    """`value` as compact JSON in UTF-8, as JSON.stringify writes it. A lone surrogate, which
    JSON.stringify escapes, makes the whole value ASCII with escapes."""
    try:
        return json.dumps(value, separators=(',', ':'), ensure_ascii=False).encode('utf-8')
    except UnicodeEncodeError:
        return json.dumps(value, separators=(',', ':')).encode('ascii')


def caret(text: str) -> str:
    """`text` with each control character in caret notation (^[ for Esc, ^? for DEL) and each C1
    control as \\x9b, so the shell never echoes a control."""
    out = []
    for c in text:
        code = ord(c)
        if code < 0x20:
            out.append('^' + chr(code + 64))
        elif code == 0x7f:
            out.append('^?')
        elif 0x80 <= code < 0xa0 or 0xd800 <= code < 0xe000:
            out.append(f'\\x{code:02x}')
        else:
            out.append(c)
    return ''.join(out)


def hexes(text: str, limit: int = 48) -> str:
    data = text.encode('utf-8', 'surrogatepass')
    shown = ' '.join(f'{b:02x}' for b in data[:limit])
    return shown + (f' … ({len(data)} bytes)' if len(data) > limit else '')


def printable(c: str) -> bool:
    code = ord(c)
    return code >= 0x20 and code != 0x7f and not 0x80 <= code < 0xa0 and not 0xd800 <= code < 0xe000


def provider_entity(stamp: str) -> dict:
    """One ready, signed-in Codex, so the provider banner stays out of the captures."""
    return {'instanceId': INSTANCE, 'driver': 'codex', 'displayName': 'Codex', 'enabled': True, 'installed': True,
            'version': '1.2.0', 'status': 'ready', 'auth': {'status': 'authenticated'}, 'message': None,
            'checkedAt': stamp, 'setup': {'canAuthenticate': False, 'canInstall': False},
            'compatibilityAdvisory': None, 'supportedRuntimeModes': None,
            'models': [{'slug': MODEL, 'name': 'GPT-6', 'shortName': 'GPT-6', 'isCustom': False,
                        'capabilities': {'optionDescriptors': [copy.deepcopy(EFFORT)]}}]}


def unescape(text: str) -> str:
    """`text` with \\xHH, \\uHHHH, \\e, \\a, \\r, \\n, \\t and \\\\ turned into what they name."""
    named = {'\\e': '\x1b', '\\a': '\x07', '\\r': '\r', '\\n': '\n', '\\t': '\t', '\\\\': '\\'}

    def one(match) -> str:
        found = match.group(0)
        return chr(int(found[2:], 16)) if found[1] in 'xu' else named[found]

    return re.sub(r'\\x[0-9a-fA-F]{2}|\\u[0-9a-fA-F]{4}|\\[earnt\\]', one, text)


def expected_launch(thread: 'Thread') -> dict:
    """The launch t3term sends to start `thread`'s term-1, as Launch::for_thread builds it from
    what the desktop sends: the worktree or the workspace root as cwd, the worktree, and
    T3CODE_PROJECT_ROOT, with T3CODE_WORKTREE_PATH when there is a worktree."""
    root, worktree = thread.project['workspaceRoot'], thread.record['worktreePath']
    launch = {'cwd': worktree or root, 'env': {'T3CODE_PROJECT_ROOT': root}}
    if worktree:
        launch['worktreePath'] = worktree
        launch['env']['T3CODE_WORKTREE_PATH'] = worktree
    return launch


def launch_mismatches(thread: 'Thread', payload: dict) -> list:
    """The launch fields of an attach that differ from `expected_launch`, by name."""
    expected = expected_launch(thread)
    wrong = []
    for field in ('cwd', 'worktreePath', 'env'):
        if payload.get(field) != expected.get(field):
            wrong.append(field)
    return wrong


# ---- the invented world ----


class Thread:
    """One invented thread in the nightly's V2 shapes, with a few finished turns. Its record is
    the app thread and the shell thread in one, as the PR 33 fixture serves them."""

    def __init__(self, tid: str, project: dict, title: str, created: str, worktree, visited: str):
        self.tid, self.short, self.project = tid, tid[len('thr-'):], project
        self.runs, self.messages, self.items = [], [], []
        self.record = {
            'createdBy': 'user', 'creationSource': 'web',
            'id': tid, 'projectId': project['id'], 'title': title,
            'branch': f't3/{self.short}' if worktree else None,
            'worktreePath': worktree, 'providerInstanceId': INSTANCE,
            'modelSelection': {'instanceId': INSTANCE, 'model': MODEL},
            'runtimeMode': 'full-access', 'interactionMode': 'default',
            'lineage': {'parentThreadId': None, 'relationshipToParent': None, 'rootThreadId': tid},
            'forkedFrom': None, 'activeProviderThreadId': f'pthr-{self.short}',
            'latestRunId': None, 'activeRunId': None, 'status': 'idle', 'activityRunStatus': None,
            'pendingRuntimeRequest': None, 'pendingBackgroundTasks': [], 'latestVisibleMessage': None,
            'latestUserMessageAt': None, 'latestUserAuthoredMessageAt': None, 'latestRunRequestedAt': None,
            'latestRunStartedAt': None, 'latestRunCompletedAt': None, 'activityRunStartedAt': None,
            'lastError': None, 'lastErrorClass': None, 'hasActionableProposedPlan': False,
            'createdAt': created, 'updatedAt': created, 'archivedAt': None, 'pinnedAt': None,
            'pinOrderKey': None, 'activeOrderKey': None, 'settledOverride': None, 'settledAt': None,
            'unsettledAt': None, 'snoozedAt': None, 'snoozedUntil': None, 'lastVisitedAt': visited,
            'deletedAt': None,
        }

    def shell(self) -> dict:
        """The shell's copy, with the item counts a ThreadShell carries."""
        return dict(copy.deepcopy(self.record), itemCount=len(self.items), visibleItemCount=len(self.items))

    def turns(self, at, count: int, last: int, step: int = 20 * MINUTE) -> 'Thread':
        """`count` finished turns, the last asked at offset `last`, one every `step`."""
        for n in range(1, count + 1):
            offset = last - (count - n) * step
            asked, done = at(offset), at(offset + 40 * SECOND)
            run_id, root, message_id = f'run-{self.short}-{n}', f'node-{self.short}-{n}-root', f'msg-{self.short}-{n}'
            text = f'Fixture prompt {n} on {self.tid}. Nothing here came from a real thread.'
            reply = (f"Fixture reply {n} on {self.tid}. F4 shows this thread's terminal in a drawer under the "
                     f'composer. Nothing here came from a real thread.')
            self.runs.append({'id': run_id, 'threadId': self.tid, 'ordinal': n, 'providerInstanceId': INSTANCE,
                              'modelSelection': copy.deepcopy(self.record['modelSelection']),
                              'providerThreadId': None, 'userMessageId': message_id, 'rootNodeId': root,
                              'activeAttemptId': None, 'status': 'completed', 'requestedAt': asked,
                              'startedAt': asked, 'completedAt': done, 'checkpointId': None,
                              'contextHandoffId': None})
            self.messages.append({'id': message_id, 'role': 'user', 'runId': run_id, 'text': text,
                                  'createdAt': asked})
            for key, stamp, fields in (
                    (f'prompt-{n}', asked, {'type': 'user_message', 'text': text}),
                    (f'reply-{n}', done, {'type': 'assistant_message', 'messageId': f'msg-{self.short}-reply-{n}',
                                          'streaming': False, 'text': reply})):
                item = {'id': f'item-{self.short}-{key}', 'threadId': self.tid, 'runId': run_id, 'nodeId': root,
                        'providerThreadId': None, 'providerTurnId': None, 'nativeItemRef': None,
                        'parentItemId': None, 'status': 'completed', 'title': None, 'startedAt': stamp,
                        'completedAt': stamp, 'updatedAt': stamp, 'ordinal': len(self.items)}
                item.update(fields)
                self.items.append(item)
        latest = self.runs[-1]
        self.record.update(
            latestRunId=latest['id'], status='completed', latestUserMessageAt=latest['requestedAt'],
            latestUserAuthoredMessageAt=latest['requestedAt'], latestRunRequestedAt=latest['requestedAt'],
            latestRunStartedAt=latest['startedAt'], latestRunCompletedAt=latest['completedAt'],
            activityRunStartedAt=latest['startedAt'], updatedAt=latest['completedAt'])
        return self

    def projection(self) -> dict:
        own = copy.deepcopy(self.items)
        visible = [{'position': i, 'visibility': 'local', 'sourceThreadId': self.tid, 'sourceItemId': item['id'],
                    'item': item} for i, item in enumerate(own)]
        return {'thread': copy.deepcopy(self.record), 'runs': copy.deepcopy(self.runs),
                'messages': copy.deepcopy(self.messages), 'turnItems': own, 'visibleTurnItems': visible,
                'providerSessions': [], 'runtimeRequests': [], 'plans': [], 'attempts': [], 'nodes': [],
                'subagents': [], 'providerThreads': [], 'providerTurns': [], 'checkpointScopes': [],
                'checkpoints': [], 'contextHandoffs': [], 'contextTransfers': [],
                'updatedAt': self.record['updatedAt']}


def build_world(t0: int) -> dict:
    """The two projects and their threads. Every time is an offset from t0, when the world was
    built."""
    def at(offset: int) -> str:
        return iso(t0 + offset)

    projects = {
        'proj-term': {'id': 'proj-term', 'title': 'Terminal Desk', 'workspaceRoot': DESK_ROOT,
                      'repositoryIdentity': None, 'createdAt': at(-20 * DAY), 'updatedAt': at(-DAY),
                      'deletedAt': None},
        # Not T3: a project with a blank workspace root, where t3term must not start a shell.
        'proj-rootless': {'id': 'proj-rootless', 'title': 'Rootless Desk', 'workspaceRoot': '',
                          'repositoryIdentity': None, 'createdAt': at(-20 * DAY), 'updatedAt': at(-DAY),
                          'deletedAt': None},
    }
    threads, order = {}, []
    for k, (tid, title, worktree, project) in enumerate(THREADS):
        path = f'{DESK_ROOT}/.t3/worktrees/{worktree}' if worktree else None
        thread = Thread(tid, projects[project], title, at(-(3 + k) * DAY), path, at(-k * MINUTE))
        thread.turns(at, 1 + k % 2, -(k + 1) * HOUR)
        threads[tid] = thread
        order.append(tid)
    return {'projects': list(projects.values()), 'threads': threads, 'order': order}


# ---- T3's terminals ----


class History:
    """A terminal's history, as BoundedTerminalHistory keeps it (Manager.ts): at most
    HISTORY_LINES lines and HISTORY_BYTES bytes of UTF-8, less the queries T3 strips. Text after
    the last newline counts as a line, as in T3. Like T3, it adds text in pieces of at most
    HISTORY_PIECE characters and trims after each piece: whole lines from the front until the
    lines fit, then whole code points from the front until the bytes fit. A trim copies at most
    one piece, so a flood never copies the history."""

    def __init__(self, text: str = ''):
        self.pieces = collections.deque()  # [text, its UTF-8 bytes, its newlines]
        self.bytes = self.newlines = 0
        if text:
            self.add(text)

    @property
    def lines(self) -> int:
        if not self.pieces:
            return 0
        return self.newlines + (0 if self.pieces[-1][0].endswith('\n') else 1)

    def add(self, text: str):
        text = QUERY.sub('', text)
        for start in range(0, len(text), HISTORY_PIECE):
            piece = text[start:start + HISTORY_PIECE]
            size, newlines = utf8_len(piece), piece.count('\n')
            self.pieces.append([piece, size, newlines])
            self.bytes += size
            self.newlines += newlines
            self.trim()

    def drop(self, count: int):
        """Drops the first `count` characters of the oldest piece, and the piece once it is empty."""
        first = self.pieces[0]
        if count >= len(first[0]):
            self.pieces.popleft()
            self.bytes -= first[1]
            self.newlines -= first[2]
            return
        gone = first[0][:count]
        size, newlines = utf8_len(gone), gone.count('\n')
        first[0] = first[0][count:]
        first[1] -= size
        first[2] -= newlines
        self.bytes -= size
        self.newlines -= newlines

    def trim(self):
        over = self.lines - HISTORY_LINES
        while over > 0:
            text, _, newlines = self.pieces[0]
            if newlines < over:
                over -= newlines
                self.drop(len(text))
                continue
            end = 0
            for _ in range(over):
                end = text.index('\n', end) + 1
            self.drop(end)
            over = 0
        while self.bytes > HISTORY_BYTES:
            text, size, _ = self.pieces[0]
            excess = self.bytes - HISTORY_BYTES
            if size <= excess:
                self.drop(len(text))
                continue
            data = text.encode('utf-8', 'surrogatepass')
            # T3 drops whole code points until it has dropped `excess` bytes or more, so the
            # first one kept starts at the first lead byte from `excess` on.
            while excess < len(data) and data[excess] & 0xC0 == 0x80:
                excess += 1
            self.drop(len(data[:excess].decode('utf-8', 'surrogatepass')))

    def text(self) -> str:
        return ''.join(text for text, _, _ in self.pieces)


class Session:
    """One terminal T3 holds: term-1 of a thread, its shell, its history and its event count."""

    def __init__(self, fixture: 'Fixture', tid: str, terminal: str, launch: dict, history: str = '',
                 sequence: int = 0):
        self.fixture = fixture
        self.tid, self.terminal = tid, terminal
        self.cwd, self.worktree = launch['cwd'], launch.get('worktreePath')
        self.env = dict(launch.get('env') or {})
        self.status, self.pid = 'running', next(fixture.pids)
        self.exit_code = self.exit_signal = None
        self.message = None  # an error event's message
        self.label, self.busy = 'Terminal 1', False
        self.sequence = sequence
        self.history = History(history)
        self.cols, self.rows = DEFAULT_COLS, DEFAULT_ROWS
        self.sizes = collections.deque(maxlen=20)
        self.updated = iso(now_ms())
        self.shell = FakeShell(fixture, self)

    @property
    def key(self) -> tuple:
        return self.tid, self.terminal

    def snapshot(self) -> dict:
        return {'threadId': self.tid, 'terminalId': self.terminal, 'cwd': self.cwd,
                'worktreePath': self.worktree, 'status': self.status, 'pid': self.pid,
                'history': self.history.text(), 'exitCode': self.exit_code, 'exitSignal': self.exit_signal,
                'label': self.label, 'updatedAt': self.updated, 'sequence': self.sequence}

    def event(self, kind: str, **fields) -> dict:
        """An event of this terminal with the next sequence."""
        self.sequence += 1
        self.updated = iso(now_ms())
        event = {'type': kind, 'threadId': self.tid, 'terminalId': self.terminal, 'sequence': self.sequence}
        event.update(fields)
        return event

    def header(self) -> str:
        """The left of the header the drawer should show while it is attached to this terminal,
        on a row with room for all of it: the name, the label and the state in full, with
        `F4 hide` at the right. A narrower row drops `F4 hide`, then cuts the label or leaves it
        out, then gives the state its short form, such as `exited` for `exited with 1,
        read-only`, as header_row in src/tui/terminal.rs lays it out."""
        if self.status == 'starting':
            state = 'starting'
        elif self.status == 'running':
            state = 'running'
        elif self.status == 'exited':
            how = (f'exited on signal {self.exit_signal}' if self.exit_signal is not None
                   else f'exited with {self.exit_code}' if self.exit_code is not None else 'exited')
            state = f'{how}, read-only'
        else:
            # The message shows on the screen's last row and in a notice, as last_row says.
            state = 'failed, read-only'
        return f' Terminal · {shown(self.label, 64)} · {state}'

    def last_row(self):
        """What a failed shell's screen shows on its last row: T3's message from the error
        event, with its controls cleaned out and cut as a drawer of 120 columns cuts it. None for
        any other shell. A snapshot carries no message, so once the drawer attaches again the
        last row is the screen's own."""
        if self.status != 'error' or not self.message:
            return None
        return shown(f' {self.message}', 120)


class AttachStream:
    """One terminal.attach stream and T3's output window on it (OutputProtocol.ts). The server
    sends while fewer than WINDOW_CHUNKS chunks and WINDOW_BYTES bytes wait for an Ack, counting
    each Chunk message's compact JSON, and each Ack frees the oldest."""

    def __init__(self, fixture: 'Fixture', connection: 'Connection', request_id: str, tid: str, terminal: str):
        self.fixture, self.connection = fixture, connection
        self.request_id, self.tid, self.terminal = request_id, tid, terminal
        self.window = collections.deque()  # bytes of each chunk that waits for an Ack
        self.waiting = collections.deque()  # encoded chunks the window holds back
        self.sent = self.acked = self.excess = self.bytes = 0
        self.open = True

    @property
    def key(self) -> tuple:
        return self.tid, self.terminal

    def full(self) -> bool:
        return len(self.window) >= WINDOW_CHUNKS or sum(self.window) >= WINDOW_BYTES

    def push(self, values: list):
        self.waiting.append(encode({'_tag': 'Chunk', 'requestId': self.request_id, 'values': values}))
        self.flush()

    def flush(self):
        fixture = self.fixture
        while self.open and self.waiting and not self.full():
            data = self.waiting.popleft()
            if not self.connection.send_frame(0x1, data):
                self.open = False
                return
            self.window.append(len(data))
            self.sent += 1
            self.bytes += len(data)
            fixture.counts['window.chunks'] += 1
            fixture.counts['window.bytes'] += len(data)
            fixture.most('window.maxUnacked', len(self.window))
            fixture.most('window.maxChunkBytes', len(data))
        fixture.most('window.maxWaiting', len(self.waiting))

    def ack(self):
        if self.window:
            self.window.popleft()
            self.acked += 1
            self.fixture.counts['window.acks'] += 1
        else:
            self.excess += 1
            self.fixture.counts['window.excessAcks'] += 1
        self.flush()

    def force(self, chunks: list):
        """Not T3: sends `chunks` in one frame as one JSON array, past the window."""
        encoded = [encode({'_tag': 'Chunk', 'requestId': self.request_id, 'values': values}) for values in chunks]
        if self.connection.send_frame(0x1, b'[' + b','.join(encoded) + b']'):
            self.window.extend(len(data) for data in encoded)
            self.sent += len(encoded)
            self.fixture.counts['window.forced'] += len(encoded)


# ---- the fake shell ----


HELP = ('Fixture commands. Nothing here runs a program or reads a file.\r\n'
        '  help  ls  colors  echo TEXT  env  seq N  flood MIB  clear  size\r\n'
        '  showkeys        each write in caret notation and hex, until Ctrl+D\r\n'
        '  cursorkeys      application cursor keys on or off\r\n'
        '  fullscreen [--mouse]   the alternate screen: arrows move the @, q quits\r\n'
        '  dsr             asks for the cursor position and device attributes\r\n'
        '  sync [--hang]   a synchronized update, ended or not\r\n'
        '  paste on|off    whether the prompt asks for bracketed paste\r\n'
        '  hostile         OSC, DCS, APC, BEL and C1 controls that must go nowhere\r\n'
        '  sleep N  dev    a busy shell until it ends or Ctrl+C\r\n'
        '  exit [N]  kill [SIGNAL]\r\n')
LS = ('\x1b[1;34mdocs\x1b[0m  \x1b[1;34msrc\x1b[0m  \x1b[1;34mtests\x1b[0m  Cargo.lock  Cargo.toml  README.md  '
      '\x1b[1;32mbuild.sh\x1b[0m  \x1b[36mlatest -> README.md\x1b[0m\r\n')
DEV = ('\r\n> terminal-desk@0.0.0 dev\r\n> vite\r\n\r\n'
       '  \x1b[1;32mVITE\x1b[0m v7.0.0  ready in \x1b[1m312\x1b[0m ms\r\n\r\n'
       '  \x1b[32m➜\x1b[0m  \x1b[1mLocal\x1b[0m:   http://127.0.0.1:5173/ (invented, nothing listens)\r\n'
       '  \x1b[2m➜  press Ctrl+C to stop\x1b[0m\r\n')
FLOOD_ROW = 'abcdefghijklmnopqrstuvwxyz 0123456789 the fixture floods this terminal\r\n'


def colors() -> str:
    rows = ['16 colors   ' + ''.join(f'\x1b[{30 + i}m{i:>3}\x1b[0m' for i in range(8))
            + ''.join(f'\x1b[{90 + i}m{8 + i:>3}\x1b[0m' for i in range(8)),
            'backgrounds ' + ''.join(f'\x1b[{40 + i}m   \x1b[0m' for i in range(8))
            + ''.join(f'\x1b[{100 + i}m   \x1b[0m' for i in range(8))]
    for start in range(16, 256, 36):
        rows.append('256 colors  ' + ''.join(f'\x1b[48;5;{c}m \x1b[0m' for c in range(start, min(start + 36, 256))))
    rows.append('truecolor   ' + ''.join(f'\x1b[48;2;{255 * i // 47};96;{255 - 255 * i // 47}m \x1b[0m'
                                         for i in range(48)))
    rows.append('styles      \x1b[1mbold\x1b[0m \x1b[2mdim\x1b[0m \x1b[3mitalic\x1b[0m \x1b[4munderline\x1b[0m '
                '\x1b[7mreverse\x1b[0m \x1b[9mstrike\x1b[0m')
    rows.append('wide        漢字 かな 🙂   combining  cafe\u0301 n\u0303')
    return '\r\n'.join(rows) + '\r\n'


def number(args: list, default: int, low: int, high: int):
    """The first word as a whole number from `low` to `high`, `default` without one, or None."""
    if not args:
        return default
    word = args[0]
    if not word.isdigit() or not low <= int(word) <= high:
        return None
    return int(word)


COMMANDS = {name: f'do_{name}' for name in (
    'help', 'ls', 'colors', 'echo', 'env', 'seq', 'flood', 'clear', 'size', 'showkeys', 'cursorkeys',
    'fullscreen', 'dsr', 'sync', 'paste', 'hostile', 'sleep', 'dev', 'exit', 'kill')}


class FakeShell:
    """What answers term-1's input: a line editor and the commands in HELP, all invented. It runs
    no program and reads no file. All it prints comes from this file, and what it shows of its
    input is printable characters, caret notation and hex. Callers hold the fixture's lock."""

    def __init__(self, fixture: 'Fixture', session: Session):
        self.fixture, self.session = fixture, session
        self.mode = 'prompt'  # prompt, wait, dsr, showkeys, fullscreen or stopped
        self.line = ''
        self.bracketed = True
        self.paste = None  # the open bracketed paste: its hash, its counts and its first characters
        self.pending = ''  # the start of an end bracket, which the next write may finish
        self.epoch = 0  # moved by Ctrl+C, an end and a stop, so a timer from before does nothing
        self.cursor_keys = self.mouse = False
        self.dot = [0, 0]
        self.last_key, self.arrows, self.replies = 'none yet', 0, 0
        self.directory = safe(session.cwd.rstrip('/').rsplit('/', 1)[-1] or '/', 40)

    def out(self, text: str):
        self.fixture.output(self.session, text)

    def prompt(self) -> str:
        on = '\x1b[?2004h' if self.bracketed else ''
        return f'{on}\x1b[1;32mfixture\x1b[0m:\x1b[1;34m{self.directory}\x1b[0m$ '

    def banner(self) -> str:
        return (f'\x1b[1mfixture shell\x1b[0m on {self.session.tid}, in {safe(self.session.cwd, 120)}\r\n'
                'The PR 41 fixture invented this shell, and it runs nothing. Type help.\r\n')

    def start(self):
        self.out(self.banner() + self.prompt())

    def stop(self):
        self.epoch += 1
        self.mode = 'stopped'

    def done(self, text: str = ''):
        """Prints `text` and the prompt, and takes keys at the prompt again."""
        self.mode = 'prompt'
        self.out(text + self.prompt())

    # -- input --

    def feed(self, data: str):
        """Takes one terminal.write."""
        data, self.pending = self.pending + data, ''
        i = 0
        while i < len(data):
            if self.mode == 'stopped':
                return
            if self.mode == 'showkeys':
                self.showkey(data[i:])
                return
            if self.paste is not None:
                end = data.find(PASTE_END, i)
                if end < 0:
                    keep = next((k for k in range(min(len(PASTE_END) - 1, len(data) - i), 0, -1)
                                 if data.endswith(PASTE_END[:k])), 0)
                    self.paste_add(data[i:len(data) - keep])
                    self.pending = data[len(data) - keep:]
                    return
                self.paste_add(data[i:end])
                i = end + len(PASTE_END)
                self.paste_done()
            elif data.startswith(PASTE_START, i):
                self.paste = {'hash': hashlib.sha256(), 'chars': 0, 'lf': 0, 'cr': 0, 'head': ''}
                i += len(PASTE_START)
            else:
                token = self.token(data, i)
                i += len(token)
                self.key(token)

    @staticmethod
    def token(data: str, i: int) -> str:
        """The key at `i`: a CSI sequence to its final byte, an SS3 key, Alt and a character, Esc
        alone at the end of a write, or one character."""
        if data[i] != '\x1b' or i + 1 == len(data):
            return data[i]
        if data[i + 1] == '[':
            j = i + 2
            while j < len(data) and '\x20' <= data[j] <= '\x3f':
                j += 1
            return data[i:j + 1] if j < len(data) and '\x40' <= data[j] <= '\x7e' else data[i:j]
        if data[i + 1] == 'O':
            return data[i:i + 3]
        return data[i:i + 2]

    def key(self, token: str):
        self.fixture.counts['shell.keys'] += 1
        if self.mode == 'fullscreen':
            self.fullscreen_key(token)
        elif self.mode == 'dsr':
            if token == '\x03':
                self.dsr_end(self.epoch)
            else:
                self.replies += 1
                self.fixture.counts['shell.dsrReplies'] += 1
                self.out(f'[reply] {caret(token)}\r\n')
        elif self.mode == 'wait':
            if token == '\x03':
                self.interrupt()
            elif len(token) == 1 and printable(token):
                self.out(token)
        else:
            self.prompt_key(token)

    def prompt_key(self, token: str):
        if len(token) == 1 and printable(token):
            if len(self.line) < LINE_LIMIT:
                self.line += token
                self.out(token)
        elif token in ('\r', '\n'):
            self.run()
        elif token in ('\x7f', '\x08'):
            if self.line:
                gone, self.line = self.line[-1], self.line[:-1]
                self.out('\b \b' * max(1, cells(gone)))
        elif token == '\x03':
            self.line = ''
            self.out('^C\r\n' + self.prompt())
        elif token == '\x04':
            if not self.line:
                self.out('exit\r\n')
                self.fixture.exit_session(self.session, 0, None, 'Ctrl+D')
        elif token == '\x15':
            self.line = ''
            self.out('\r\x1b[K' + self.prompt())
        elif token == '\x0c':
            self.out('\x1b[H\x1b[2J' + self.prompt() + self.line)
        else:
            self.fixture.counts['shell.otherKeys'] += 1
            self.out(f'\r\n[key {caret(token)}  {hexes(token)}]\r\n' + self.prompt() + self.line)

    def paste_add(self, text: str):
        if not text:
            return
        paste = self.paste
        paste['hash'].update(text.encode('utf-8', 'surrogatepass'))
        paste['chars'] += len(text)
        paste['lf'] += text.count('\n')
        paste['cr'] += text.count('\r')
        if len(paste['head']) <= 200:
            paste['head'] += text[:201 - len(paste['head'])]

    def paste_done(self):
        paste, self.paste = self.paste, None
        digest = paste['hash'].hexdigest()[:12]
        self.fixture.counts['shell.pastes'] += 1
        self.fixture.log('shell.paste', threadId=self.session.tid, chars=paste['chars'], lf=paste['lf'],
                         cr=paste['cr'], sha=digest)
        if self.mode != 'prompt':
            return
        head = paste['head']
        if len(head) <= 200 and all(printable(c) for c in head):
            self.line = (self.line + head)[:LINE_LIMIT]
        self.out(f'\r\n[paste {paste["chars"]} characters, LF {paste["lf"]}, CR {paste["cr"]}, sha256 {digest}]\r\n'
                 + self.prompt() + self.line)

    def resized(self):
        if self.mode == 'fullscreen':
            self.out(self.draw())

    # -- commands --

    def run(self):
        line, self.line = self.line.strip(), ''
        self.out('\r\n' + ('\x1b[?2004l' if self.bracketed else ''))
        words = line.split()
        if not words:
            self.out(self.prompt())
            return
        name, rest = words[0], words[1:]
        method = COMMANDS.get(name)
        self.fixture.counts[f'shell.{name if method else "unknown"}'] += 1
        self.fixture.log('shell.command', threadId=self.session.tid, command=name if method else None,
                         words=len(words))
        if method is None:
            self.done(f'fixture: {safe(name, 40)}: not a fixture command. Type help.\r\n')
            return
        getattr(self, method)(rest)

    def do_help(self, args):
        self.done(HELP)

    def do_ls(self, args):
        self.done(LS)

    def do_colors(self, args):
        self.done(colors())

    def do_echo(self, args):
        self.done(' '.join(args) + '\r\n')

    def do_env(self, args):
        rows = [f'PWD={self.session.cwd}', f'(T3 worktreePath: {self.session.worktree})']
        rows += [f'{k}={v}' for k, v in sorted(self.session.env.items())]
        self.done(''.join(caret(row) + '\r\n' for row in rows))

    def do_seq(self, args):
        count = number(args, 10, 1, 100000)
        if count is None:
            self.done('seq: takes a count from 1 to 100000\r\n')
        else:
            self.done(''.join(f'{n}\r\n' for n in range(1, count + 1)))

    def do_flood(self, args):
        mib = number(args, 16, 1, 64)
        if mib is None:
            self.done('flood: takes 1 to 64 MiB\r\n')
            return
        self.mode = 'wait'
        self.fixture.activity(self.session, f'flood {mib}', True)
        threading.Thread(target=self.flood, args=(self.epoch, mib << 20), daemon=True).start()

    def flood(self, epoch: int, total: int):
        """Prints `total` characters in rows, 64 KiB at a time while every attach stream of the
        terminal holds back fewer than 32 chunks. Stops when no stream took a chunk for 10 s."""
        fixture, sent, row, idle = self.fixture, 0, 0, 0.0
        while True:
            with fixture.lock:
                if self.epoch != epoch:
                    return
                if sent >= total:
                    self.epoch += 1
                    fixture.activity(self.session, 'Terminal 1', False)
                    self.done(f'flood: done, {total >> 20} MiB in {row} rows\r\n')
                    return
                if fixture.backlog(self.session) < 32:
                    rows, size = [], 0
                    while size < 65536 and sent + size < total:
                        text = f'flood {row:08d} {FLOOD_ROW}'
                        rows.append(text)
                        size += len(text)
                        row += 1
                    self.out(''.join(rows))
                    sent += size
                    idle = 0.0
                    continue
            time.sleep(0.01)
            idle += 0.01
            if idle >= 10:
                with fixture.lock:
                    if self.epoch == epoch:
                        self.epoch += 1
                        fixture.activity(self.session, 'Terminal 1', False)
                        self.done(f'flood: stopped after {sent >> 20} MiB: no stream took output for 10 s\r\n')
                return

    def do_clear(self, args):
        self.done('\x1b[H\x1b[2J\x1b[3J')

    def do_size(self, args):
        session = self.session
        if session.sizes:
            self.done(f'size: T3 last resized this shell to {session.cols}x{session.rows}\r\n')
        else:
            self.done(f"size: no resize yet, so T3's default {session.cols}x{session.rows}\r\n")

    def do_showkeys(self, args):
        self.mode = 'showkeys'
        self.out('showkeys: each write shows on its own line in caret notation and hex. Ctrl+D alone ends it.\r\n')

    def showkey(self, data: str):
        if data == '\x04':
            self.done('showkeys: done\r\n')
            return
        self.fixture.counts['shell.showkeysWrites'] += 1
        shown = caret(data)
        shown = shown if len(shown) <= 120 else shown[:119] + '…'
        self.out(f'[write] {shown}  {hexes(data)}\r\n')

    def do_cursorkeys(self, args):
        self.cursor_keys = not self.cursor_keys
        if self.cursor_keys:
            self.done('\x1b[?1hcursorkeys: application cursor keys on, so Up comes as ^[OA. '
                      'Run cursorkeys again to turn them off.\r\n')
        else:
            self.done('\x1b[?1lcursorkeys: application cursor keys off, so Up comes as ^[[A.\r\n')

    def do_fullscreen(self, args):
        self.mode = 'fullscreen'
        self.mouse = '--mouse' in args
        self.dot = [self.session.cols // 2, self.session.rows // 2]
        self.last_key, self.arrows = 'none yet', 0
        modes = '\x1b[?1049h\x1b[?1007h' + ('\x1b[?1000h\x1b[?1006h' if self.mouse else '') + '\x1b[?25l'
        self.out(modes + self.draw())

    def draw(self) -> str:
        cols, rows = max(1, self.session.cols), max(1, self.session.rows)
        x = min(max(self.dot[0], 0), cols - 1)
        y = min(max(self.dot[1], 1), max(1, rows - 2))
        self.dot = [x, y]
        title = (f'fullscreen {cols}x{rows}: arrows move the @, q quits, '
                 + ('mouse reports on' if self.mouse else 'the wheel sends arrows'))
        footer = f'arrows {self.arrows}, last key {self.last_key}'
        return ('\x1b[H\x1b[2J\x1b[7m' + title[:cols].ljust(cols) + '\x1b[0m'
                + f'\x1b[{y + 1};{x + 1}H\x1b[1;33m@\x1b[0m'
                + f'\x1b[{rows};1H' + footer[:cols])

    def fullscreen_key(self, token: str):
        moves = {'A': (0, -1), 'B': (0, 1), 'C': (1, 0), 'D': (-1, 0)}
        if token == 'q':
            modes = '\x1b[?1007l' + ('\x1b[?1006l\x1b[?1000l' if self.mouse else '') + '\x1b[?25h\x1b[?1049l'
            self.done(modes + f'fullscreen: {self.arrows} arrow keys\r\n')
            return
        if len(token) == 3 and token[:2] in ('\x1b[', '\x1bO') and token[2] in moves:
            dx, dy = moves[token[2]]
            self.dot = [self.dot[0] + dx, self.dot[1] + dy]
            self.arrows += 1
            self.fixture.counts['shell.fullscreenArrows'] += 1
        self.last_key = caret(token)[:40]
        self.out(self.draw())

    def do_dsr(self, args):
        self.mode, self.replies = 'dsr', 0
        epoch = self.epoch
        self.out('dsr: asks for the cursor position (CSI 6n) and the device attributes (CSI c). '
                 'Replies show for 2 s.\r\n\x1b[6n\x1b[c')
        self.fixture.later(2.0, lambda: self.dsr_end(epoch))

    def dsr_end(self, epoch: int):
        with self.fixture.lock:
            if self.epoch != epoch or self.mode != 'dsr':
                return
            self.epoch += 1
            self.done(f'dsr: {self.replies} replies\r\n')

    def do_sync(self, args):
        hang = '--hang' in args
        self.mode = 'wait'
        epoch = self.epoch
        self.out('\x1b[?2026h' + ''.join(f'sync: row {k} of 12, which shows once the update ends\r\n'
                                         for k in range(1, 13)))

        def end():
            with self.fixture.lock:
                if self.epoch != epoch:
                    return
                self.epoch += 1
                if hang:
                    self.done('sync: the update never ended, so it shows at the drawer\'s deadline\r\n')
                else:
                    self.done('\x1b[?2026lsync: done\r\n')

        self.fixture.later(1.0, end)

    def do_paste(self, args):
        if args[:1] == ['on']:
            self.bracketed = True
            self.done('paste: the prompt asks for bracketed paste\r\n')
        elif args[:1] == ['off']:
            self.bracketed = False
            self.done('paste: the prompt no longer asks for bracketed paste\r\n')
        else:
            self.done('paste: on or off\r\n')

    def do_hostile(self, args):
        self.done(HOSTILE_OUTPUT)

    def do_sleep(self, args):
        seconds = number(args, 5, 1, 60)
        if seconds is None:
            self.done('sleep: takes 1 to 60 seconds\r\n')
            return
        self.mode = 'wait'
        epoch = self.epoch
        self.fixture.activity(self.session, f'sleep {seconds}', True)

        def end():
            with self.fixture.lock:
                if self.epoch != epoch:
                    return
                self.epoch += 1
                self.fixture.activity(self.session, 'Terminal 1', False)
                self.done('sleep: done\r\n')

        self.fixture.later(seconds, end)

    def do_dev(self, args):
        self.mode = 'wait'
        self.fixture.activity(self.session, 'npm run dev', True)
        self.out(DEV)

    def interrupt(self):
        self.epoch += 1
        if self.session.busy:
            self.fixture.activity(self.session, 'Terminal 1', False)
        self.done('^C\r\n')

    def do_exit(self, args):
        code = number(args, 0, 0, 255)
        self.out('exit\r\n')
        self.fixture.exit_session(self.session, 0 if code is None else code, None, 'exit')

    def do_kill(self, args):
        name = (args[0] if args else 'KILL').upper().lstrip('-')
        name = name[3:] if name.startswith('SIG') else name
        signum = SIGNALS.get(name)
        if signum is None and name.isdigit() and 0 < int(name) < 65:
            signum = int(name)
        if signum is None:
            self.done('kill: takes HUP, INT, QUIT, KILL, TERM or a number from 1 to 64\r\n')
            return
        self.out(f'kill: the shell dies of signal {signum}\r\n')
        self.fixture.exit_session(self.session, None, signum, 'kill')


# ---- the server's state ----


def schema_error(message: str) -> dict:
    """Not T3's shape: T3 fails a request its schema refuses with a parse error, which t3term
    reports the same way as any other refusal."""
    return {'_tag': 'FixtureSchemaError', 'message': message}


def lookup_error(tid: str, terminal: str) -> dict:
    # T3's terminal errors carry their fields and no message (terminal.ts).
    return {'_tag': 'TerminalSessionLookupError', 'threadId': tid, 'terminalId': terminal}


def named(value) -> bool:
    return isinstance(value, str) and bool(value.strip())


def whole(value, high: int) -> bool:
    return isinstance(value, int) and not isinstance(value, bool) and 1 <= value <= high


class Fixture:
    """State shared by every connection: sessions, tickets, the world, T3's terminals, their
    attach streams and the event log. Every change holds `lock`."""

    def __init__(self, root: Path, options: dict):
        self.root = root
        self.options = options
        self.lock = threading.RLock()
        self.control_key = (root / 'control.key').read_text().strip()
        self.secret_values = {self.control_key}
        self.sessions = {}  # token -> {'id', 'scopes', 'kind', 'revoked'}
        self.tickets = {}  # ticket -> token, single use
        self.counts = Counter()
        self.maxima = Counter()
        self.rpc_tags = Counter()  # 'login TAG' or 'terminal TAG' -> requests
        self.connections = set()
        self.connection_ids = itertools.count(1)
        self.config_streams = []  # (connection, request id)
        self.ws_hold_until = 0  # every /ws answers 503 until this wall-clock millisecond
        self.terminal_ws_hold_until = 0  # a terminal session's /ws answers 503 until then
        self.permissions = 'normal'
        self.sequence = FIRST_SEQUENCE
        self.t0 = None
        self.port = None
        self.projects = self.threads = self.order = None
        self.terminals = {}  # (thread id, terminal id) -> Session
        self.persisted = {}  # (thread id, terminal id) -> the history a close kept
        self.closed = {}  # terminal a close removed -> its last label, until something starts it again
        self.streams = []  # open AttachStreams
        self.held = []  # (connection, request id, payload) of attaches `hold-attach` holds
        self.busy_writes = Counter()  # terminal -> writes T3 hasn't answered yet
        self.busy_resizes = Counter()
        self.hold_attach_until = 0
        self.slow_ms = 0
        self.fail_writes = self.fail_resize = self.missing_cwd = False
        self.pids = itertools.count(FIRST_PID)
        self.timers = []
        self.log_path = root / 'logs' / 'events.jsonl'

    # -- log --

    def scrub(self, text: str) -> str:
        for value in self.secret_values:
            if value and value in text:
                text = text.replace(value, '<redacted>')
        return text

    def log(self, event: str, **fields):
        stamp = now_ms()
        record = {'wall': iso(stamp), 'wallMs': stamp, 'event': event}
        if self.t0 is not None:
            record['sinceWorldMs'] = stamp - self.t0
        record.update({k: v for k, v in fields.items() if v is not None})
        with self.lock:
            line = self.scrub(json.dumps(record))
            fd = os.open(self.log_path, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
            try:
                os.write(fd, (line + '\n').encode())
            finally:
                os.close(fd)
            print(line, file=sys.stderr, flush=True)

    def most(self, name: str, value: int):
        if value > self.maxima[name]:
            self.maxima[name] = value

    def later(self, seconds: float, action):
        timer = threading.Timer(max(0.0, seconds), action)
        timer.daemon = True
        timer.start()
        with self.lock:
            self.timers = [t for t in self.timers if t.is_alive()]
            self.timers.append(timer)

    # -- world --

    def ensure_world(self):
        with self.lock:
            if self.threads is not None:
                return
            self.t0 = now_ms()
            world = build_world(self.t0)
            self.projects, self.threads, self.order = world['projects'], world['threads'], world['order']
            self.seed()
            write_json(self.root / 'world.json', {'t0Ms': self.t0, 't0': iso(self.t0)})
            self.log('world.built', threads=len(self.order), terminals=sorted(tid for tid, _ in self.terminals))

    def seed(self):
        """The terminals T3 holds when the fixture starts. Each has counted to SEEDED_SEQUENCE."""
        def make(tid: str):
            session = Session(self, tid, TERMINAL_ID, expected_launch(self.threads[tid]), sequence=SEEDED_SEQUENCE)
            self.terminals[session.key] = session
            return session, session.shell

        session, shell = make('thr-running')
        prompt = shell.prompt()
        session.history.add(shell.banner() + prompt + 'ls\r\n\x1b[?2004l' + LS + prompt + 'colors\r\n\x1b[?2004l'
                            + colors() + prompt + 'seq 60\r\n\x1b[?2004l' + ''.join(f'{n}\r\n' for n in range(1, 61))
                            + prompt)

        session, shell = make('thr-exited')
        session.history.add(shell.banner() + shell.prompt() + 'exit 1\r\n\x1b[?2004lexit\r\n')
        session.status, session.pid, session.exit_code = 'exited', None, 1
        shell.stop()

        session, shell = make('thr-failed')
        session.status, session.pid = 'error', None
        shell.stop()

        session, shell = make('thr-busy')
        session.history.add(shell.banner() + shell.prompt() + 'npm run dev\r\n\x1b[?2004l' + DEV)
        session.label, session.busy, shell.mode = 'npm run dev', True, 'wait'

        session, shell = make('thr-huge')
        rows = ''.join(f'\x1b[3{1 + n % 6}mhuge {n:05d}\x1b[0m ' + '\x1b7\x1b8' * 400 + 'end\r\n'
                       for n in range(1, 4991))
        session.history.add(rows + shell.prompt())

    def shell_snapshot(self) -> dict:
        with self.lock:
            self.ensure_world()
            return {'schemaVersion': SCHEMA_VERSION, 'snapshotSequence': self.sequence,
                    'projects': copy.deepcopy(self.projects),
                    'threads': [self.threads[tid].shell() for tid in self.order], 'archivedThreads': []}

    def thread_snapshot(self, tid: str):
        with self.lock:
            self.ensure_world()
            thread = self.threads.get(tid)
            if thread is None:
                return None
            return {'snapshotSequence': self.sequence, 'projection': thread.projection()}

    def config_value(self) -> dict:
        return {'environment': self.descriptor(), 'providers': [provider_entity(iso(now_ms()))], 'settings': {}}

    def descriptor(self) -> dict:
        return {
            'environmentId': ENVIRONMENT_ID,
            'label': 'PR 41 fixture',
            'platform': {'os': sys.platform, 'arch': platform.machine()},
            'serverVersion': SERVER_VERSION,
            'orchestrationProtocolVersion': 2,
            'capabilities': {'threadSettlement': True, 'threadSnooze': True, 'serverResolvedCommandContext': True},
        }

    # -- auth --

    def issue_session(self, request: dict) -> dict:
        with self.lock:
            scopes = [s for s in request.get('scopes') or [] if isinstance(s, str)]
            kind = 'terminal' if any(s.startswith('terminal:') for s in scopes) else 'login'
            session_id = f'sess-fixture-{len(self.sessions) + 1:02d}-{kind}'
            token = 'fixture-' + secrets.token_urlsafe(32)
            self.secret_values.add(token)
            self.sessions[token] = {'id': session_id, 'scopes': scopes, 'kind': kind, 'revoked': False}
            self.counts[f'auth.issue.{kind}'] += 1
            self.log('auth.issue', sessionId=session_id, kind=kind, scopes=scopes, ttl=str(request.get('ttl')),
                     label=str(request.get('label')), subject=str(request.get('subject')))
            return {'sessionId': session_id, 'token': token}

    def revoke_session(self, session_id: str) -> bool:
        with self.lock:
            for session in self.sessions.values():
                if session['id'] == session_id:
                    already = session['revoked']
                    session['revoked'] = True
                    self.counts[f'auth.revoke.{session["kind"]}'] += 1
                    self.log('auth.revoke', sessionId=session_id, kind=session['kind'], alreadyRevoked=already,
                             openSockets=sum(1 for c in self.connections if c.session is session))
                    return True
            self.log('auth.revoke', sessionId=session_id, known=False)
            return False

    def session_state(self, session) -> dict:
        """What /api/auth/session says of `session`. `scopes` is the older field, which leaves out
        terminal:read (packages/contracts/src/auth.ts:131-149). `permissions` lists the session's
        scopes, or what `auth permissions` has it say for a terminal session."""
        with self.lock:
            if session is None:
                return {'authenticated': False}
            scopes = list(session['scopes'])
            answer = {'authenticated': True, 'scopes': [s for s in scopes if s != 'terminal:read']}
            mode = self.permissions if session['kind'] == 'terminal' else 'normal'
            if mode == 'extra':
                answer['permissions'] = scopes + ['orchestration:read']
            elif mode == 'missing':
                answer['permissions'] = [s for s in scopes if s != 'terminal:operate']
            elif mode != 'absent':
                answer['permissions'] = scopes
            self.counts[f'auth.check.{session["kind"]}'] += 1
            self.log('auth.check', sessionId=session['id'], kind=session['kind'], permissions=mode)
            return answer

    def set_permissions(self, mode: str):
        with self.lock:
            if mode not in PERMISSION_MODES:
                return None, f'permissions takes {", ".join(PERMISSION_MODES)}'
            self.permissions = mode
            self.log('auth.permissions', mode=mode)
            return {'note': f'/api/auth/session now answers a terminal session with the {mode} permissions. '
                            't3term checks them each time it connects the terminal; `restart` gives it a '
                            'session to check.'}, None

    def issue_ticket(self, token: str) -> str:
        ticket = secrets.token_urlsafe(24)
        with self.lock:
            self.tickets[ticket] = token
            self.secret_values.add(ticket)
        return ticket

    def take_ticket(self, ticket: str):
        with self.lock:
            token = self.tickets.pop(ticket, None)
            session = self.sessions.get(token) if token else None
            return session if session and not session['revoked'] else None

    def drop(self, hold: float, everything: bool):
        """Closes the terminal sockets, or every socket, and has /ws answer 503 for `hold` s."""
        with self.lock:
            until = now_ms() + int(hold * 1000)
            self.terminal_ws_hold_until = until
            if everything:
                self.ws_hold_until = until
            targets = [c for c in self.connections if everything or c.kind == 'terminal']
            self.counts['drops'] += 1
            self.log('drop', holdSeconds=hold, everything=everything, sockets=len(targets))
        for connection in targets:
            connection.close()
        whose = 'every session' if everything else 'a terminal session'
        return {'note': f'closed {len(targets)} socket(s); /ws answers 503 to {whose} for {hold:g} s'}, None

    # -- T3's terminals --

    def current(self, session: Session) -> bool:
        return self.terminals.get(session.key) is session

    def broadcast(self, key: tuple, values: list):
        for stream in list(self.streams):
            if stream.key == key and stream.open:
                stream.push(values)

    def backlog(self, session: Session) -> int:
        return max((len(s.waiting) for s in self.streams if s.key == session.key and s.open), default=0)

    def output(self, session: Session, text: str):
        """Output from the shell, as T3 reads it from the PTY: events of at most OUTPUT_PIECE
        characters, each with its sequence, into the history, and to the attach streams at most
        EVENTS_PER_CHUNK events to a chunk."""
        if not self.current(session) or not text:
            return
        events = []
        for start in range(0, len(text), OUTPUT_PIECE):
            piece = text[start:start + OUTPUT_PIECE]
            session.history.add(piece)
            events.append(session.event('output', data=piece))
        for start in range(0, len(events), EVENTS_PER_CHUNK):
            self.broadcast(session.key, events[start:start + EVENTS_PER_CHUNK])
        self.counts['term.outputEvents'] += len(events)

    def activity(self, session: Session, label: str, busy: bool):
        """An activity event, with the label cut at 128 UTF-16 code units as T3 cuts it."""
        if not self.current(session):
            return
        session.label, session.busy = utf16_cut(label, LABEL_LIMIT), busy
        self.broadcast(session.key, [session.event('activity', hasRunningSubprocess=busy, label=session.label)])

    def exit_session(self, session: Session, code, signum, why: str):
        if not self.current(session) or session.status in ('exited', 'error'):
            return
        session.shell.stop()
        session.status, session.pid = 'exited', None
        session.exit_code, session.exit_signal = code, signum
        if session.busy:
            session.label, session.busy = 'Terminal 1', False
        self.broadcast(session.key, [session.event('exited', exitCode=code, exitSignal=signum)])
        self.counts['term.exited'] += 1
        self.log('term.exited', threadId=session.tid, code=code, signal=signum, why=why)

    def create(self, tid: str, terminal: str, launch: dict, starting: float = 0.0) -> Session:
        """A new terminal, as openWithWorkspaceLease starts one: with the history a close kept,
        counting from 0, and its started event as a snapshot on the attach streams already open.
        The caller prints the banner once the stream that asked has its snapshot. With
        `starting`, the shell shows starting that many seconds first (not T3)."""
        key = (tid, terminal)
        session = Session(self, tid, terminal, launch, self.persisted.pop(key, ''), sequence=0)
        self.terminals[key] = session
        self.closed.pop(key, None)
        self.counts['term.created'] += 1
        self.log('term.created', threadId=tid, terminalId=terminal, starting=starting or None)
        if starting > 0:
            session.status, session.pid = 'starting', None
            self.broadcast(key, [{'type': 'snapshot', 'snapshot': session.snapshot()}])
            self.later(starting, lambda: self.started(session, True))
        else:
            self.started(session, False)
        return session

    def started(self, session: Session, banner: bool):
        with self.lock:
            if not self.current(session) or session.status not in ('starting', 'running'):
                return
            session.status = 'running'
            if session.pid is None:
                session.pid = next(self.pids)
            session.sequence += 1
            session.updated = iso(now_ms())
            self.broadcast(session.key, [{'type': 'snapshot', 'snapshot': session.snapshot()}])
            if banner:
                session.shell.start()

    def find(self, connection, request_id: str):
        return next((s for s in self.streams if s.connection is connection and s.request_id == request_id), None)

    def attach(self, connection, request_id: str, payload: dict):
        tid, terminal = payload.get('threadId'), payload.get('terminalId')
        launch = 'cwd' in payload
        violations = [name for name in ('cols', 'rows', 'restartIfNotRunning', 'providerInstanceId')
                      if name in payload]
        with self.lock:
            self.ensure_world()
            self.counts[f'attach.{"launch" if launch else "identity"}'] += 1
            for name in violations:
                self.counts[f'attach.violation.{name}'] += 1
            self.log('term.attach', conn=connection.id, requestId=request_id, threadId=safe(tid, 60),
                     terminalId=safe(terminal, 40), launch=launch, violations=violations or None)
            if not (named(tid) and named(terminal)) or (launch and not named(payload.get('cwd'))):
                self.counts['attach.badInput'] += 1
                connection.fail(request_id, 'terminal.attach',
                                schema_error('threadId, terminalId and any cwd must be non-empty strings'))
                return
            connection.streams[request_id] = 'terminal.attach'
            wait = (self.hold_attach_until - now_ms()) / 1000
            if wait > 0:
                self.held.append((connection, request_id, payload))
                self.counts['attach.held'] += 1
                self.later(wait, lambda: self.release(connection, request_id))
                return
            self.answer_attach(connection, request_id, payload)

    def release(self, connection, request_id: str):
        with self.lock:
            entry = next((h for h in self.held if h[0] is connection and h[1] == request_id), None)
            if entry is None:
                # Interrupted, or its socket closed, during the hold.
                return
            wait = (self.hold_attach_until - now_ms()) / 1000
            if wait > 0:
                self.later(wait, lambda: self.release(connection, request_id))
                return
            self.held.remove(entry)
            self.answer_attach(connection, request_id, entry[2])

    def answer_attach(self, connection, request_id: str, payload: dict):
        """Starts the terminal for a launch when there is none, then registers the stream and
        sends its snapshot under one lock hold, so no event lands between them."""
        tid, terminal = payload['threadId'], payload['terminalId']
        key = (tid, terminal)
        session = self.terminals.get(key)
        fresh = False
        if session is None:
            refusal = None
            if 'cwd' not in payload:
                self.counts['attach.lookupRefused'] += 1
                refusal = lookup_error(tid, terminal)
            elif self.missing_cwd:
                self.counts['attach.cwdRefused'] += 1
                refusal = {'_tag': 'TerminalCwdNotFoundError', 'cwd': str(payload['cwd'])}
            elif tid not in self.threads:
                self.counts['attach.unknownThread'] += 1
                refusal = lookup_error(tid, terminal)
            if refusal is not None:
                connection.streams.pop(request_id, None)
                connection.fail(request_id, 'terminal.attach', refusal)
                return
            wrong = launch_mismatches(self.threads[tid], payload)
            for field in wrong:
                self.counts[f'attach.launchMismatch.{field}'] += 1
            env = payload.get('env') if isinstance(payload.get('env'), dict) else {}
            worktree = payload.get('worktreePath')
            self.log('term.launch', threadId=tid, cwd=safe(payload['cwd'], 160),
                     worktreePath=safe(worktree, 160) if worktree is not None else None,
                     envKeys=sorted(str(k) for k in env), mismatches=wrong or None)
            session = self.create(tid, terminal, {'cwd': payload['cwd'],
                                                  'worktreePath': worktree if named(worktree) else None,
                                                  'env': env})
            fresh = True
        elif 'cwd' in payload:
            self.counts['attach.launchIgnored'] += 1
        stream = AttachStream(self, connection, request_id, tid, terminal)
        self.streams.append(stream)
        stream.push([{'type': 'snapshot', 'snapshot': session.snapshot()}])
        self.counts['attach.answered'] += 1
        if fresh:
            session.shell.start()

    def ack(self, connection, request_id: str):
        with self.lock:
            stream = self.find(connection, request_id)
            if stream is None:
                # t3term acks each chunk of the login socket's other streams on receipt.
                self.counts[f'rpc.ackOther.{connection.kind}'] += 1
            else:
                stream.ack()

    def interrupt(self, connection, request_id: str):
        with self.lock:
            stream = self.find(connection, request_id)
            if stream is not None:
                stream.open = False
                self.streams.remove(stream)
                self.counts['attach.interrupted'] += 1
            held = [h for h in self.held if h[0] is connection and h[1] == request_id]
            for entry in held:
                self.held.remove(entry)
                self.counts['attach.interruptedWhileHeld'] += 1
            self.config_streams = [(c, r) for c, r in self.config_streams if not (c is connection and r == request_id)]

    def drop_streams(self, connection):
        """Forgets a connection's streams after its socket closed."""
        with self.lock:
            for stream in [s for s in self.streams if s.connection is connection]:
                stream.open = False
                self.streams.remove(stream)
                self.counts['attach.socketClosed'] += 1
            self.held = [h for h in self.held if h[0] is not connection]
            self.config_streams = [(c, r) for c, r in self.config_streams if c is not connection]

    def write(self, connection, request_id: str, payload: dict):
        tid, terminal, data = payload.get('threadId'), payload.get('terminalId'), payload.get('data')
        with self.lock:
            self.ensure_world()
            self.counts['write.requests'] += 1
            if isinstance(data, str):
                # Counted before the checks below, so a write over the limit shows in largestWrite.
                self.most('write.largest', utf16_len(data))
            if not (named(tid) and named(terminal)) or not isinstance(data, str) or not data \
                    or utf16_len(data) > WRITE_LIMIT:
                self.counts['write.badInput'] += 1
                connection.fail(request_id, 'terminal.write', schema_error(
                    'data must be a non-empty string of at most 65536 UTF-16 code units'))
                return
            key = (tid, terminal)
            units = utf16_len(data)
            self.counts['write.units'] += units
            if self.busy_writes[key]:
                self.counts['write.overlapped'] += 1
            # The log keeps the length and a hash of what was typed, never the text.
            self.log('term.write', conn=connection.id, requestId=request_id, threadId=safe(tid, 60),
                     units=units, sha=sha(data)[:12])
            session = self.terminals.get(key)
            if session is None:
                self.counts['write.lookupRefused'] += 1
                connection.fail(request_id, 'terminal.write', lookup_error(tid, terminal))
            elif session.status == 'exited':
                self.counts['write.toExited'] += 1
                connection.exit_success(request_id, None)
            elif session.status != 'running':
                self.counts['write.notRunning'] += 1
                connection.fail(request_id, 'terminal.write',
                                {'_tag': 'TerminalNotRunningError', 'threadId': tid, 'terminalId': terminal})
            elif self.fail_writes:
                self.counts['write.failed'] += 1
                connection.fail(request_id, 'terminal.write', {
                    '_tag': 'TerminalWriteError', 'threadId': tid, 'terminalId': terminal, 'terminalPid': session.pid,
                    'cause': {'name': 'Error', 'message': 'EIO: the fixture failed this write on purpose'}})
            elif self.slow_ms:
                self.busy_writes[key] += 1
                self.later(self.slow_ms / 1000, lambda: self.finish_write(connection, request_id, session, data, True))
            else:
                self.finish_write(connection, request_id, session, data, False)

    def finish_write(self, connection, request_id: str, session: Session, data: str, slow: bool):
        with self.lock:
            if slow:
                self.busy_writes[session.key] -= 1
            if self.current(session) and session.status == 'running':
                session.shell.feed(data)
                self.counts['write.fed'] += 1
            connection.exit_success(request_id, None)

    def resize(self, connection, request_id: str, payload: dict):
        tid, terminal = payload.get('threadId'), payload.get('terminalId')
        cols, rows = payload.get('cols'), payload.get('rows')
        with self.lock:
            self.ensure_world()
            self.counts['resize.requests'] += 1
            if not (named(tid) and named(terminal)) or not (whole(cols, 1000) and whole(rows, 500)):
                self.counts['resize.badInput'] += 1
                connection.fail(request_id, 'terminal.resize',
                                schema_error('cols must be a whole number from 1 to 1000, rows from 1 to 500'))
                return
            key = (tid, terminal)
            if self.busy_resizes[key]:
                self.counts['resize.overlapped'] += 1
            self.log('term.resize', conn=connection.id, requestId=request_id, threadId=safe(tid, 60),
                     cols=cols, rows=rows)
            session = self.terminals.get(key)
            if session is None or session.status != 'running':
                # T3 resizes nothing for a terminal it doesn't have or that isn't running.
                self.counts['resize.ignored'] += 1
                connection.exit_success(request_id, None)
            elif self.fail_resize:
                self.counts['resize.failed'] += 1
                connection.fail(request_id, 'terminal.resize', {
                    '_tag': 'TerminalResizeError', 'threadId': tid, 'terminalId': terminal,
                    'terminalPid': session.pid, 'cols': cols, 'rows': rows,
                    'cause': {'name': 'Error', 'message': 'EBADF: the fixture failed this resize on purpose'}})
            elif self.slow_ms:
                self.busy_resizes[key] += 1
                self.later(self.slow_ms / 1000,
                           lambda: self.finish_resize(connection, request_id, session, cols, rows, True))
            else:
                self.finish_resize(connection, request_id, session, cols, rows, False)

    def finish_resize(self, connection, request_id: str, session: Session, cols: int, rows: int, slow: bool):
        with self.lock:
            if slow:
                self.busy_resizes[session.key] -= 1
            if self.current(session) and session.status == 'running':
                session.cols, session.rows = cols, rows
                session.sizes.append(f'{cols}x{rows}')
                self.counts['resize.applied'] += 1
                session.shell.resized()
            connection.exit_success(request_id, None)

    # -- `term` controls --

    def listing(self) -> list:
        rows = []
        for tid in self.order:
            key = (tid, TERMINAL_ID)
            session = self.terminals.get(key)
            streams = [s for s in self.streams if s.key == key and s.open]
            row = {'threadId': tid, 'title': self.threads[tid].record['title'], 'streams': len(streams),
                   'waiting': sum(len(s.waiting) for s in streams), 'unacked': sum(len(s.window) for s in streams),
                   'expectedLaunch': expected_launch(self.threads[tid])}
            if session is None:
                closed = key in self.closed
                label = f' · {shown(self.closed[key], 64)}' if closed else ''
                row.update(status='closed' if closed else 'missing',
                           keptHistory=utf8_len(self.persisted.get(key, '')),
                           header=(f' Terminal{label} · closed, read-only' if closed and streams
                                   else ' Terminal · not started'))
            else:
                row.update(status=session.status, label=safe(session.label, 80), labelUnits=utf16_len(session.label),
                           busy=session.busy, sequence=session.sequence, pid=session.pid,
                           cwd=safe(session.cwd, 120), worktreePath=session.worktree,
                           historyBytes=session.history.bytes, historyLines=session.history.lines,
                           size=f'{session.cols}x{session.rows}', sizes=list(session.sizes),
                           shell=session.shell.mode, header=session.header(), lastRow=session.last_row())
            rows.append(row)
        return rows

    def setting(self, action: str, value):
        if action == 'hold-attach':
            try:
                seconds = float(value)
            except (TypeError, ValueError):
                seconds = -1
            if not 0 <= seconds <= 120:
                return None, 'hold-attach takes 0 to 120 seconds; 0 ends a hold'
            self.hold_attach_until = now_ms() + int(seconds * 1000)
            note = (f'T3 answers each terminal.attach after {seconds:g} s until the hold ends' if seconds
                    else 'attaches are answered at once again; held ones go when their wait ends')
        elif action == 'slow':
            if not isinstance(value, int) or isinstance(value, bool) or not 0 <= value <= 10000:
                return None, 'slow takes 0 to 10000 milliseconds'
            self.slow_ms = value
            note = (f'T3 feeds each write and applies each resize {value} ms after it comes, then answers'
                    if value else 'writes and resizes are answered at once again')
        else:
            if value not in ('on', 'off'):
                return None, f'{action} takes on or off'
            attribute = {'fail-writes': 'fail_writes', 'fail-resize': 'fail_resize', 'missing-cwd': 'missing_cwd'}[action]
            setattr(self, attribute, value == 'on')
            note = {'fail-writes': 'each terminal.write fails with TerminalWriteError',
                    'fail-resize': 'each terminal.resize fails with TerminalResizeError',
                    'missing-cwd': 'an attach that starts a terminal fails with TerminalCwdNotFoundError'}[action]
            note = note if value == 'on' else f'{action} is off'
        self.log('term.setting', action=action, value=value)
        return {'note': note}, None

    def term_action(self, request: dict):
        """One `term` control: (answer, None), or (None, why not)."""
        action = request.get('action')
        with self.lock:
            self.ensure_world()
            if action == 'list':
                return {'terminals': self.listing()}, None
            if action in ('hold-attach', 'slow', 'fail-writes', 'fail-resize', 'missing-cwd'):
                return self.setting(action, request.get('value'))
            tid = request.get('threadId')
            thread = self.threads.get(tid) if isinstance(tid, str) else None
            if thread is None:
                return None, f'no fixture thread {safe(tid, 60)}; `term list` lists them'
            key = (tid, TERMINAL_ID)
            session = self.terminals.get(key)
            self.counts[f'control.{action}'] += 1
            self.log('term.control', action=str(action), threadId=tid)
            if action == 'start':
                return self.term_start(thread, session, request)
            if action == 'restart':
                return self.term_restart(thread, session)
            if session is None:
                return None, f'{tid} has no term-1 now; `term start --thread {tid}` starts one'
            method = {'exit': self.term_exit, 'error': self.term_error, 'close': self.term_close,
                      'clear': self.term_clear, 'activity': self.term_activity, 'output': self.term_output,
                      'malformed': self.term_malformed, 'stranger': self.term_stranger,
                      'duplicate': self.term_duplicate, 'old': self.term_old,
                      'ignore-window': self.term_ignore_window, 'oversize': self.term_oversize}.get(action)
            if method is None:
                return None, f'no term action {safe(action, 40)}'
            return method(session, request)

    def term_start(self, thread: Thread, session, request: dict):
        if session is not None:
            return None, f"{thread.tid}'s term-1 is open already, and T3 ignores another open of the same launch"
        launch = expected_launch(thread)
        if not launch['cwd']:
            return None, f'{thread.tid} has no workspace root, and T3 refuses a blank cwd'
        try:
            starting = float(request.get('starting') or 0)
        except (TypeError, ValueError):
            starting = -1
        if not 0 <= starting <= 60:
            return None, '--starting takes 0 to 60 seconds'
        session = self.create(thread.tid, TERMINAL_ID, launch, starting)
        if not starting:
            session.shell.start()
        how = f', starting for {starting:g} s first (not T3)' if starting else ''
        return {'note': f'started term-1 of {thread.tid} as the desktop would{how}: counting from 0, with '
                        f'{session.history.bytes} bytes of kept history'}, None

    def term_restart(self, thread: Thread, session):
        """restartResolved: the history goes, a terminal T3 has counts on and a missing one counts
        from 0, and the attach streams get restarted with the new snapshot."""
        launch = expected_launch(thread)
        if not launch['cwd']:
            return None, f'{thread.tid} has no workspace root, and T3 refuses a blank cwd'
        key = (thread.tid, TERMINAL_ID)
        if session is None:
            self.persisted.pop(key, None)
            session = Session(self, thread.tid, TERMINAL_ID, launch, '', sequence=0)
            self.terminals[key] = session
            self.closed.pop(key, None)
        else:
            session.shell.stop()
            session.shell = FakeShell(self, session)
            session.history = History()
            session.status, session.pid = 'running', next(self.pids)
            session.exit_code = session.exit_signal = session.message = None
            session.label, session.busy = 'Terminal 1', False
        session.sequence += 1
        session.updated = iso(now_ms())
        self.broadcast(key, [{'type': 'restarted', 'threadId': thread.tid, 'terminalId': TERMINAL_ID,
                              'sequence': session.sequence, 'snapshot': session.snapshot()}])
        session.shell.start()
        return {'note': f'restarted term-1 of {thread.tid} at sequence {session.sequence}'}, None

    def term_exit(self, session: Session, request: dict):
        if session.status not in ('running', 'starting'):
            return None, f'term-1 of {session.tid} is {session.status} already'
        code, signum = request.get('code'), request.get('signal')
        if signum is not None:
            if not whole(signum, 64):
                return None, '--signal takes 1 to 64'
            self.exit_session(session, None, signum, 'control')
        else:
            code = 0 if code is None else code
            if not isinstance(code, int) or isinstance(code, bool) or not -255 <= code <= 255:
                return None, '--code takes -255 to 255'
            self.exit_session(session, code, None, 'control')
        return {'note': f'term-1 of {session.tid}: {session.header().strip()}'}, None

    def term_error(self, session: Session, request: dict):
        message = str(request.get('message') or 'The fixture failed this shell on purpose.')[:300]
        if not message.strip():
            return None, 'an error event needs a message'
        session.shell.stop()
        session.status, session.pid, session.busy, session.message = 'error', None, False, message
        self.broadcast(session.key, [session.event('error', message=message)])
        return {'note': f'term-1 of {session.tid} failed: "{safe(message, 120)}"'}, None

    def term_close(self, session: Session, request: dict):
        """closeSession: closed with the event count plus one, the history kept unless deleted,
        and the terminal gone. Attach streams stay open."""
        key = session.key
        session.shell.stop()
        self.broadcast(key, [{'type': 'closed', 'threadId': session.tid, 'terminalId': session.terminal,
                              'sequence': session.sequence + 1}])
        if request.get('deleteHistory'):
            self.persisted.pop(key, None)
        else:
            self.persisted[key] = session.history.text()
        del self.terminals[key]
        self.closed[key] = session.label
        kept = utf8_len(self.persisted.get(key, ''))
        return {'note': f'closed term-1 of {session.tid}; {kept} bytes of history kept for its next start'}, None

    def term_clear(self, session: Session, request: dict):
        session.history = History()
        self.broadcast(session.key, [session.event('cleared')])
        return {'note': f'cleared term-1 of {session.tid}'}, None

    def term_activity(self, session: Session, request: dict):
        preset, label = request.get('preset'), request.get('label')
        if preset == 'idle':
            self.activity(session, 'Terminal 1', False)
        elif preset in LABEL_PRESETS:
            self.activity(session, LABEL_PRESETS[preset], True)
        elif isinstance(label, str) and label:
            self.activity(session, label, True)
        else:
            return None, f'activity takes --label TEXT or --preset {"|".join(sorted(LABEL_PRESETS))}|idle'
        split = any(0xd800 <= ord(c) < 0xe000 for c in session.label)
        note = (f'term-1 of {session.tid}: label "{safe(session.label, 120)}", {utf16_len(session.label)} UTF-16 '
                f'units{", ending in a lone surrogate as T3 cuts it" if split else ""}')
        return {'note': note}, None

    def term_output(self, session: Session, request: dict):
        text = request.get('text')
        if not isinstance(text, str) or not text:
            return None, 'output needs text'
        if request.get('escapes'):
            text = unescape(text)
        if len(text) > 1 << 20:
            return None, 'output takes at most 1 MiB'
        if request.get('noSequence'):
            session.history.add(text)
            self.broadcast(session.key, [{'type': 'output', 'threadId': session.tid, 'terminalId': session.terminal,
                                          'data': text}])
        else:
            self.output(session, text)
        return {'note': f'{len(text)} characters of output on term-1 of {session.tid}'}, None

    def redraw(self, session: Session):
        if session.shell.mode == 'prompt':
            self.output(session, session.shell.prompt() + session.shell.line)

    def term_malformed(self, session: Session, request: dict):
        # Not T3: an output event whose data isn't a string.
        self.broadcast(session.key, [session.event('output', data=42)])
        return {'note': f'sent term-1 of {session.tid} an output event with data 42'}, None

    def term_stranger(self, session: Session, request: dict):
        # Not T3: events for another terminal and another thread, and a type T3 doesn't have.
        self.broadcast(session.key, [
            {'type': 'output', 'threadId': session.tid, 'terminalId': 'term-2', 'sequence': session.sequence + 1,
             'data': '\r\nSTRANGER-TERM-2\r\n'},
            {'type': 'output', 'threadId': 'thr-somewhere-else', 'terminalId': session.terminal,
             'sequence': 10 ** 6, 'data': '\r\nSTRANGER-THREAD\r\n'},
            {'type': 'future', 'threadId': session.tid, 'terminalId': session.terminal, 'data': '\r\nPR41-FUTURE\r\n'},
        ])
        return {'note': 'sent three events the drawer must skip; none of their text should show'}, None

    def term_duplicate(self, session: Session, request: dict):
        # Not T3: the same event twice.
        event = session.event('output', data=f'\r\n{DUPLICATE}\r\n')
        session.history.add(event['data'])
        self.broadcast(session.key, [event])
        self.broadcast(session.key, [event])
        self.redraw(session)
        return {'note': f'sent {DUPLICATE} twice at sequence {event["sequence"]}; it should show once'}, None

    def term_old(self, session: Session, request: dict):
        # Not T3: an event from before the last one.
        sequence = max(0, session.sequence - 3)
        self.broadcast(session.key, [{'type': 'output', 'threadId': session.tid, 'terminalId': session.terminal,
                                      'sequence': sequence, 'data': '\r\nOLD-EVENT\r\n'}])
        return {'note': f'sent OLD-EVENT at sequence {sequence}, after {session.sequence}'}, None

    def term_ignore_window(self, session: Session, request: dict):
        # Not T3: chunks past the window, in one frame.
        count = request.get('chunks') or 20
        if not whole(count, 64):
            return None, '--chunks takes 1 to 64'
        chunks = []
        for k in range(1, count + 1):
            data = f'window probe {k} of {count}\r\n'
            session.history.add(data)
            chunks.append([session.event('output', data=data)])
        streams = [s for s in self.streams if s.key == session.key and s.open]
        for stream in streams:
            stream.force(chunks)
        return {'note': f'sent {count} chunks in one frame past the window to {len(streams)} stream(s)'}, None

    def term_oversize(self, session: Session, request: dict):
        # Not T3: one output event of about 57.4 MiB as JSON, past the drawer's 56 MiB. It stays out
        # of the history.
        event = session.event('output', data='\x1b7\x1b8' * 4_300_000)
        streams = [s for s in self.streams if s.key == session.key and s.open]
        for stream in streams:
            stream.push([event])
        return {'note': f'sent one output chunk of about 57.4 MiB to {len(streams)} stream(s)'}, None

    def state(self) -> dict:
        with self.lock:
            self.ensure_world()
            return {
                'counts': dict(self.counts),
                'maxima': dict(self.maxima),
                'rpcRequestsByTag': dict(self.rpc_tags),
                'permissions': self.permissions,
                'settings': {'holdAttachSecondsLeft': max(0, round((self.hold_attach_until - now_ms()) / 1000, 1)),
                             'slowMs': self.slow_ms, 'failWrites': self.fail_writes,
                             'failResize': self.fail_resize, 'missingCwd': self.missing_cwd},
                'sockets': dict(Counter(c.kind for c in self.connections)),
                'attachStreams': len(self.streams),
                'sessions': [{'id': s['id'], 'kind': s['kind'], 'scopes': s['scopes'], 'revoked': s['revoked']}
                             for s in self.sessions.values()],
                'terminals': self.listing(),
            }


# ---- the WebSocket and HTTP server ----


def unmask(data: bytes, mask: bytes) -> bytes:
    if not data:
        return data
    key = (mask * (len(data) // 4 + 1))[:len(data)]
    return (int.from_bytes(data, 'big') ^ int.from_bytes(key, 'big')).to_bytes(len(data), 'big')


class Connection:
    """One WebSocket carrying Effect RPC, the way src/rpc.rs speaks it. A writer thread sends
    what `send_frame` queues, in order, so nothing that holds the fixture's lock ever waits on a
    socket that t3term isn't reading."""

    def __init__(self, fixture: Fixture, handler, session: dict):
        self.fixture = fixture
        self.rfile, self.wfile, self.sock = handler.rfile, handler.wfile, handler.connection
        self.session = session
        self.id = next(fixture.connection_ids)
        self.open = True
        self.streams = {}  # request id -> tag, for the streams still open
        self.outbox = queue.Queue()
        self.writer = threading.Thread(target=self.write_loop, daemon=True)

    @property
    def kind(self) -> str:
        return self.session['kind']

    def send_frame(self, opcode: int, payload: bytes) -> bool:
        """Queues one frame. False once the socket closed."""
        header = bytearray([0x80 | opcode])
        size = len(payload)
        if size < 126:
            header.append(size)
        elif size < 65536:
            header.append(126)
            header += struct.pack('!H', size)
        else:
            header.append(127)
            header += struct.pack('!Q', size)
        if not self.open:
            return False
        self.outbox.put((bytes(header), payload))
        return True

    def write_loop(self):
        while True:
            frame = self.outbox.get()
            if frame is None:
                return
            try:
                self.wfile.write(frame[0])
                self.wfile.write(frame[1])
            except OSError:
                self.open = False
                return

    def send_json(self, value) -> bool:
        return self.send_frame(0x1, encode(value))

    def exit_success(self, request_id: str, value) -> bool:
        self.streams.pop(request_id, None)
        return self.send_json({'_tag': 'Exit', 'requestId': request_id, 'exit': {'_tag': 'Success', 'value': value}})

    def fail(self, request_id: str, tag: str, error: dict) -> bool:
        """Ends a request with T3's error `error`, as Effect RPC sends a failure."""
        self.streams.pop(request_id, None)
        delivered = self.send_json({'_tag': 'Exit', 'requestId': request_id, 'exit': {'_tag': 'Failure', 'cause': [
            {'_tag': 'Fail', 'error': error}]}})
        self.fixture.log('rpc.refused', conn=self.id, kind=self.kind, requestId=request_id, tag=tag,
                         errorTag=error.get('_tag'), delivered=delivered)
        return delivered

    def read_exact(self, size: int) -> bytes:
        parts, need = [], size
        while need:
            part = self.rfile.read(need)
            if not part:
                raise EOFError('socket ended')
            parts.append(part)
            need -= len(part)
        return b''.join(parts)

    def receive(self):
        """The next data message as (kind, bytes). Answers pings itself; kind 'close' ends."""
        parts, first = [], None
        while True:
            b1, b2 = self.read_exact(2)
            fin, opcode = b1 & 0x80, b1 & 0x0F
            size = b2 & 0x7F
            if size == 126:
                size = struct.unpack('!H', self.read_exact(2))[0]
            elif size == 127:
                size = struct.unpack('!Q', self.read_exact(8))[0]
            if size > MAX_FRAME:
                raise ValueError(f'frame of {size} bytes')
            mask = self.read_exact(4) if b2 & 0x80 else b''
            data = self.read_exact(size) if size else b''
            if mask:
                data = unmask(data, mask)
            if opcode == 0x8:
                return 'close', data
            if opcode == 0x9:
                self.send_frame(0xA, data)
                continue
            if opcode >= 0x8:
                continue
            if opcode:
                first, parts = opcode, [data]
            else:
                parts.append(data)
            if fin:
                return ('text' if first == 0x1 else 'binary'), b''.join(parts)

    def serve(self):
        fixture = self.fixture
        self.writer.start()
        with fixture.lock:
            fixture.connections.add(self)
            fixture.counts[f'ws.open.{self.kind}'] += 1
        fixture.log('ws.open', conn=self.id, kind=self.kind, sessionId=self.session['id'])
        reason = 'closed'
        try:
            while True:
                kind, data = self.receive()
                if kind == 'close':
                    self.send_frame(0x8, data[:2])
                    reason = 'client closed'
                    break
                try:
                    decoded = json.loads(data.decode('utf-8'))
                except ValueError:
                    fixture.counts['rpc.unreadable'] += 1
                    fixture.log('rpc.unreadable', conn=self.id, bytes=len(data))
                    continue
                for message in decoded if isinstance(decoded, list) else [decoded]:
                    if isinstance(message, dict):
                        self.handle(message)
        except (EOFError, OSError, ValueError) as error:
            reason = f'{type(error).__name__}: {error}'
        finally:
            self.open = False
            self.outbox.put(None)
            fixture.drop_streams(self)
            with fixture.lock:
                fixture.connections.discard(self)
                fixture.counts[f'ws.close.{self.kind}'] += 1
            fixture.log('ws.close', conn=self.id, kind=self.kind, reason=reason)

    def close(self):
        self.open = False
        self.outbox.put(None)
        try:
            self.sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass

    def handle(self, message: dict):
        fixture = self.fixture
        tag = message.get('_tag')
        if tag == 'Ping':
            fixture.counts['rpc.ping'] += 1
            self.send_json({'_tag': 'Pong'})
        elif tag == 'Ack':
            fixture.counts[f'rpc.ack.{self.kind}'] += 1
            fixture.ack(self, str(message.get('requestId')))
        elif tag == 'Interrupt':
            request_id = str(message.get('requestId'))
            stream = self.streams.pop(request_id, None)
            fixture.interrupt(self, request_id)
            with fixture.lock:
                fixture.counts[f'rpc.interrupt.{self.kind}'] += 1
                if stream == 'terminal.attach':
                    fixture.counts['terminal.interrupt'] += 1
            fixture.log('rpc.interrupt', conn=self.id, kind=self.kind, requestId=request_id, tag=stream)
            self.send_json({'_tag': 'Exit', 'requestId': request_id,
                            'exit': {'_tag': 'Failure', 'cause': [{'_tag': 'Interrupt'}]}})
        elif tag == 'Request':
            payload = message.get('payload') if isinstance(message.get('payload'), dict) else {}
            self.request(str(message.get('id')), str(message.get('tag')), payload)
        else:
            fixture.log('rpc.unknown_message', conn=self.id, messageTag=safe(tag, 40))

    def request(self, request_id: str, tag: str, payload: dict):
        fixture = self.fixture
        with fixture.lock:
            fixture.rpc_tags[f'{self.kind} {tag}'] += 1
            if tag in UNEXPECTED_TERMINAL:
                fixture.counts[f'terminal.unexpected.{tag}'] += 1
        fixture.log('rpc.request', conn=self.id, kind=self.kind, requestId=request_id, tag=safe(tag, 60),
                    payload=describe(tag, payload))
        need = SCOPE_FOR.get(tag)
        if need and need not in self.session['scopes']:
            # RpcAuthorization.ts: the request fails before the handler runs.
            with fixture.lock:
                fixture.counts['scopeViolations'] += 1
                fixture.counts[f'scope.refused.{self.kind}.{tag}'] += 1
            self.fail(request_id, tag, {'_tag': 'EnvironmentAuthorizationError',
                                        'message': f'The authenticated token is missing required scope: {need}.',
                                        'requiredScope': need})
            return
        if tag == 'terminal.attach':
            fixture.attach(self, request_id, payload)
        elif tag == 'terminal.write':
            fixture.write(self, request_id, payload)
        elif tag == 'terminal.resize':
            fixture.resize(self, request_id, payload)
        elif tag in UNEXPECTED_TERMINAL:
            self.fail(request_id, tag, {'_tag': 'FixtureUnexpected',
                                        'message': f'The PR 41 drawer never sends {tag}, so the fixture refuses it.'})
        elif tag == 'orchestration.subscribeShell':
            self.subscribe_shell(request_id, payload)
        elif tag == 'orchestration.subscribeThread':
            self.subscribe_thread(request_id, payload)
        elif tag == 'subscribeWorktreeSetup':
            with fixture.lock:
                self.streams[request_id] = tag
                self.send_json({'_tag': 'Chunk', 'requestId': request_id, 'values': [None]})
        elif tag == 'subscribeServerConfig':
            with fixture.lock:
                fixture.ensure_world()
                self.streams[request_id] = tag
                fixture.config_streams.append((self, request_id))
                self.send_json({'_tag': 'Chunk', 'requestId': request_id,
                                'values': [{'version': 1, 'type': 'snapshot', 'config': fixture.config_value()}]})
        elif tag == 'server.getConfig':
            fixture.counts['config.read'] += 1
            self.exit_success(request_id, fixture.config_value())
        elif tag == 'orchestration.dispatchCommand':
            kind, tid = payload.get('type'), payload.get('threadId')
            fixture.counts['command.refused'] += 1
            fixture.log('command.refused', conn=self.id, requestId=request_id,
                        type=safe(kind, 60) if isinstance(kind, str) else None,
                        threadId=safe(tid, 60) if isinstance(tid, str) else None)
            self.fail(request_id, tag, {'_tag': 'FixtureReadOnly',
                                        'message': 'The PR 41 fixture takes no commands. Nothing was sent anywhere.'})
        else:
            self.fail(request_id, tag, {'_tag': 'FixtureUnknownTag',
                                        'message': f'The PR 41 fixture does not serve {safe(tag, 60)}.'})

    def subscribe_shell(self, request_id: str, payload: dict):
        """The sidebar's feed: a snapshot, and the marker when asked for. Nothing changes after."""
        fixture = self.fixture
        with fixture.lock:
            self.streams[request_id] = 'orchestration.subscribeShell'
            values = [{'kind': 'snapshot', 'snapshot': fixture.shell_snapshot()}]
            if payload.get('requestCompletionMarker'):
                values.append({'kind': 'synchronized'})
            self.send_json({'_tag': 'Chunk', 'requestId': request_id, 'values': values})

    def subscribe_thread(self, request_id: str, payload: dict):
        fixture = self.fixture
        tid = payload.get('threadId')
        with fixture.lock:
            snapshot = fixture.thread_snapshot(tid) if isinstance(tid, str) else None
            fixture.log('thread.subscribe', conn=self.id, requestId=request_id, threadId=safe(tid, 60),
                        found=snapshot is not None)
            if snapshot is None:
                self.fail(request_id, 'orchestration.subscribeThread', {
                    '_tag': 'OrchestrationV2ThreadNotFoundError', 'threadId': str(tid),
                    'message': f'No fixture thread has id {safe(tid, 60)}.'})
                return
            self.streams[request_id] = 'orchestration.subscribeThread'
            values = [dict(snapshot, kind='snapshot')]
            if payload.get('requestCompletionMarker'):
                values.append({'kind': 'synchronized'})
            self.send_json({'_tag': 'Chunk', 'requestId': request_id, 'values': values})


def describe(tag: str, payload: dict) -> dict:
    """The payload fields worth logging. What was typed, env values and anything unknown stay
    out; terminal.write logs its length and hash itself."""
    if tag == 'terminal.attach':
        shown = {k: safe(payload[k], 160) for k in ('threadId', 'terminalId', 'cwd', 'worktreePath')
                 if k in payload}
        if isinstance(payload.get('env'), dict):
            shown['envKeys'] = sorted(safe(k, 60) for k in payload['env'])
        shown['keys'] = sorted(safe(k, 40) for k in payload)
        return shown
    if tag == 'terminal.write':
        return {'threadId': safe(payload.get('threadId'), 60), 'terminalId': safe(payload.get('terminalId'), 40)}
    if tag == 'terminal.resize':
        return {k: payload.get(k) for k in ('cols', 'rows') if isinstance(payload.get(k), int)}
    keep = {
        'orchestration.subscribeShell': ('afterSequence', 'requestCompletionMarker'),
        'orchestration.subscribeThread': ('threadId', 'afterSequence', 'requestCompletionMarker',
                                          'acceptBoundedSnapshot'),
        'subscribeWorktreeSetup': ('threadId',),
        'orchestration.dispatchCommand': ('type', 'threadId'),
    }.get(tag)
    if keep is None:
        return {'keys': sorted(safe(k, 40) for k in payload)}
    return {k: payload[k] if not isinstance(payload[k], str) else safe(payload[k], 80) for k in keep if k in payload}


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    server_version = 'T3Fixture/pr41'
    sys_version = ''
    fixture = None  # set by cmd_serve

    def log_message(self, format, *args):
        # The default prints the request line, and the WebSocket's carries its ticket.
        pass

    def do_GET(self):
        self.route('GET')

    def do_POST(self):
        self.route('POST')

    def do_PUT(self):
        self.route('PUT')

    def do_DELETE(self):
        self.route('DELETE')

    def read_body(self) -> bytes:
        size = int(self.headers.get('Content-Length') or 0)
        return self.rfile.read(size) if size > 0 else b''

    def reply(self, method: str, path: str, status: int, body, auth: str):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)
        self.fixture.log('http', method=method, path=path, status=status, auth=auth)

    def bearer(self):
        header = self.headers.get('Authorization') or ''
        if not header.startswith('Bearer '):
            return None, None, 'missing'
        token = header[len('Bearer '):].strip()
        session = self.fixture.sessions.get(token)
        if session is None:
            return token, None, 'unknown'
        if session['revoked']:
            return token, None, 'revoked'
        return token, session, f'ok-{session["kind"]}'

    def route(self, method: str):
        fixture = self.fixture
        path, _, query = self.path.partition('?')
        body = self.read_body()
        if path == '/ws' and method == 'GET':
            return self.websocket(urllib.parse.parse_qs(query))
        if path == '/.well-known/t3/environment' and method == 'GET':
            fixture.counts['descriptor.read'] += 1
            return self.reply(method, path, 200, fixture.descriptor(), 'none')
        if path.startswith('/__fixture/'):
            return self.control(method, path, body)
        token, session, auth = self.bearer()
        if path == '/api/auth/session' and method == 'GET':
            return self.reply(method, path, 200, fixture.session_state(session), auth)
        if session is None:
            return self.reply(method, path, 401, {'message': 'The fixture does not accept this bearer session.'}, auth)
        if path == '/api/auth/websocket-ticket' and method == 'POST':
            ticket = fixture.issue_ticket(token)
            fixture.counts[f'auth.ticket.{session["kind"]}'] += 1
            return self.reply(method, path, 200, {'ticket': ticket, 'expiresAt': iso(now_ms() + MINUTE)}, auth)
        if path.startswith('/api/orchestration/'):
            if 'orchestration:read' not in session['scopes']:
                fixture.counts['scopeViolations'] += 1
                fixture.counts[f'scope.refused.{session["kind"]}.http'] += 1
                return self.reply(method, path, 403, {'message': 'The token is missing orchestration:read.'}, auth)
            if self.headers.get('x-t3-orchestration-protocol') != '2':
                return self.reply(method, path, 400, {'message': 'Orchestration routes need protocol 2.'}, auth)
            if path == '/api/orchestration/shell' and method == 'GET':
                return self.reply(method, path, 200, fixture.shell_snapshot(), auth)
            match = re.fullmatch(r'/api/orchestration/threads/([A-Za-z0-9_-]+)(/bounded)?', path)
            if match and method == 'GET':
                snapshot = fixture.thread_snapshot(match.group(1))
                if snapshot is not None:
                    return self.reply(method, path, 200, snapshot, auth)
        return self.reply(method, path, 404, {'message': 'The PR 41 fixture has nothing here.'}, auth)

    def control(self, method: str, path: str, body: bytes):
        fixture = self.fixture
        key = (self.headers.get('X-Fixture-Control') or '').encode()
        if method != 'POST' or not hmac.compare_digest(key, fixture.control_key.encode()):
            return self.reply(method, path, 403, {'message': 'Fixture control needs the fixture key.'}, 'control-denied')
        try:
            request = json.loads(body or b'{}')
        except ValueError:
            return self.reply(method, path, 400, {'message': 'Fixture control takes JSON.'}, 'control')
        if not isinstance(request, dict):
            return self.reply(method, path, 400, {'message': 'Fixture control takes a JSON object.'}, 'control')
        if path == '/__fixture/auth/issue':
            return self.reply(method, path, 200, fixture.issue_session(request), 'control')
        if path == '/__fixture/auth/revoke':
            found = fixture.revoke_session(str(request.get('sessionId') or ''))
            return self.reply(method, path, 200 if found else 404, {'revoked': found}, 'control')
        if path == '/__fixture/term':
            done, refusal = fixture.term_action(request)
        elif path == '/__fixture/auth/permissions':
            done, refusal = fixture.set_permissions(str(request.get('mode') or ''))
        elif path == '/__fixture/drop':
            try:
                hold = float(request.get('hold') or 3)
            except (TypeError, ValueError):
                hold = -1
            if not 0 < hold <= 120:
                done, refusal = None, 'drop takes a hold of 0 to 120 seconds'
            else:
                done, refusal = fixture.drop(hold, bool(request.get('everything')))
        elif path == '/__fixture/state':
            done, refusal = fixture.state(), None
        else:
            return self.reply(method, path, 404, {'message': 'No such fixture control.'}, 'control')
        if done is None:
            return self.reply(method, path, 409, {'message': refusal}, 'control')
        return self.reply(method, path, 200, done, 'control')

    def websocket(self, query: dict):
        fixture = self.fixture
        ticket = (query.get('wsTicket') or [''])[0]
        protocol = (query.get('orchestrationProtocol') or [''])[0]
        session = fixture.take_ticket(ticket) if ticket else None
        key = self.headers.get('Sec-WebSocket-Key')
        held = now_ms() < fixture.ws_hold_until or (
            session is not None and session['kind'] == 'terminal' and now_ms() < fixture.terminal_ws_hold_until)
        if held:
            fixture.counts[f'ws.held.{session["kind"] if session else "unknown"}'] += 1
            return self.reply('GET', '/ws', 503, {'message': 'The fixture is holding sockets closed.'}, 'held')
        if session is None:
            return self.reply('GET', '/ws', 401, {'message': 'The WebSocket ticket is missing, used or revoked.'},
                              'ticket-rejected')
        if protocol != '2':
            return self.reply('GET', '/ws', 400, {'message': 'The socket needs orchestrationProtocol=2.'}, 'ticket-ok')
        if not key or 'websocket' not in (self.headers.get('Upgrade') or '').lower():
            return self.reply('GET', '/ws', 400, {'message': 'Not a WebSocket upgrade.'}, 'ticket-ok')
        accept = base64.b64encode(hashlib.sha1((key + WS_GUID).encode()).digest()).decode()
        self.send_response(101, 'Switching Protocols')
        self.send_header('Upgrade', 'websocket')
        self.send_header('Connection', 'Upgrade')
        self.send_header('Sec-WebSocket-Accept', accept)
        self.end_headers()
        self.close_connection = True
        fixture.log('http', method='GET', path='/ws', status=101, auth=f'ticket-ok-{session["kind"]}')
        Connection(fixture, self, session).serve()


class Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True
    block_on_close = False


# ---- fixture directory and state ----


def write_json(path: Path, value, mode: int = 0o600):
    partial = path.with_name(path.name + '.partial')
    fd = os.open(partial, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, mode)
    with os.fdopen(fd, 'w') as handle:
        json.dump(value, handle, indent=1)
        handle.write('\n')
    os.replace(partial, path)


def read_json(path: Path):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return None


def owned(root: Path, state: dict) -> bool:
    """Whether root is a fixture directory this script made."""
    try:
        marker = (root / MARKER).read_text().strip()
    except OSError:
        return False
    return (root.name.startswith(PREFIX)
            and root.parent == Path(os.path.realpath('/tmp'))
            and bool(state) and hmac.compare_digest(marker, str(state.get('nonce', ''))))


def locate(dir_arg):
    if dir_arg:
        root = Path(os.path.realpath(dir_arg))
    elif CURRENT.is_symlink():
        root = Path(os.path.realpath(CURRENT))
    else:
        die('no current fixture; pass --dir', 2)
    state = read_json(root / 'fixture.json')
    if not state or not owned(root, state):
        die(f'{root} is not a fixture directory made by this script', 2)
    return root, state


def mark(root: Path, event: str, **fields):
    """Appends a helper event to the fixture's log, in the server's line format."""
    stamp = now_ms()
    record = {'wall': iso(stamp), 'wallMs': stamp, 'event': event, 'by': 'helper'}
    world = read_json(root / 'world.json')
    if world:
        record['sinceWorldMs'] = stamp - world['t0Ms']
    record.update(fields)
    fd = os.open(root / 'logs' / 'events.jsonl', os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
    try:
        os.write(fd, (json.dumps(record) + '\n').encode())
    finally:
        os.close(fd)


def command_of(pid: int) -> str:
    result = subprocess.run(['ps', '-o', 'command=', '-p', str(pid)], capture_output=True, text=True)
    return result.stdout.strip() if result.returncode == 0 else ''


def alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True


def terminate(pid, needles: list, label: str) -> str:
    """SIGTERM, then SIGKILL, a pid this fixture recorded, if it still runs what it ran."""
    if not pid:
        return f'{label}: no pid recorded'
    command = command_of(pid)
    if not command:
        return f'{label}: pid {pid} already gone'
    if not all(n in command for n in needles):
        return f'{label}: pid {pid} now runs something else, left alone'
    os.kill(pid, signal.SIGTERM)
    deadline = time.monotonic() + 6
    while time.monotonic() < deadline and alive(pid):
        time.sleep(0.1)
    if alive(pid) and all(n in command_of(pid) for n in needles):
        os.kill(pid, signal.SIGKILL)
        return f'{label}: pid {pid} needed SIGKILL'
    return f'{label}: pid {pid} stopped'


def tmux(state: dict, *args, check: bool = False):
    # Without TMUX, a helper run from inside your tmux talks only to the private socket.
    env = {k: v for k, v in os.environ.items() if k not in ('TMUX', 'TMUX_PANE')}
    return subprocess.run(['tmux', '-S', state['socket'], '-f', '/dev/null', *args],
                          capture_output=True, text=True, timeout=15, check=check, env=env)


def pane(state: dict):
    """(dead, dead_status, pid) for t3term's pane, or None when the session is gone."""
    result = tmux(state, 'display-message', '-p', '-t', state['session'],
                  '#{pane_dead} #{pane_dead_status} #{pane_pid}')
    if result.returncode != 0:
        return None
    parts = result.stdout.split()
    if not parts:
        return None
    dead = parts[0] == '1'
    status = parts[1] if dead and len(parts) > 2 else None
    pid = int(parts[-1]) if parts[-1].isdigit() else None
    return dead, status, pid


def t3term_env(root: Path, state: dict) -> dict:
    home = root / 'home'
    env = {
        'HOME': str(home),
        'T3CODE_HOME': str(home / '.t3'),
        'T3TERM_NO_SAVED_LOGIN': '1',
        'T3TERM_T3_COMMAND': str(root / 'bin' / 't3-shim'),
        'PATH': '/usr/bin:/bin:/usr/sbin:/sbin',
        'TERM': 'xterm-256color',
        'LANG': 'en_US.UTF-8',
        'LC_ALL': 'en_US.UTF-8',
        'TMPDIR': str(root / 'tmp'),
        'NO_PROXY': '127.0.0.1,localhost',
        'no_proxy': '127.0.0.1,localhost',
    }
    if state['color'] == 'truecolor':
        env.update(COLORTERM='truecolor', T3TERM_COLOR='truecolor')
    else:
        env['T3TERM_COLOR'] = '256'
    return env


def t3term_command(root: Path, state: dict) -> list:
    env = t3term_env(root, state)
    return ['/usr/bin/env', '-i', *[f'{k}={v}' for k, v in env.items()], state['binary']]


def control_post(root: Path, route: str, body: dict):
    """POSTs to one of this fixture's control routes with its key, as the t3 shim does. Returns
    the status and the JSON answer. The key never reaches the output or the log."""
    runtime_file = read_json(root / 'home' / '.t3' / 'userdata' / 'server-runtime.json') or {}
    origin = str(runtime_file.get('origin', ''))
    if not re.fullmatch(r'http://127\.0\.0\.1:\d+', origin):
        die('the fixture server is not running', 1)
    key = (root / 'control.key').read_text().strip()
    request = urllib.request.Request(origin + route, data=json.dumps(body).encode(), method='POST',
                                     headers={'Content-Type': 'application/json', 'X-Fixture-Control': key})
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        with opener.open(request, timeout=10) as response:
            status, data = response.status, response.read()
    except urllib.error.HTTPError as error:
        status, data = error.code, error.read()
    except (urllib.error.URLError, OSError) as error:
        die(f'the fixture server did not answer: {error}', 1)
    try:
        return status, json.loads(data or b'{}')
    except ValueError:
        return status, {}


def post(root: Path, route: str, body: dict) -> dict:
    """POSTs to a control route, prints its note, and dies on a refusal."""
    status, answer = control_post(root, route, body)
    if status != 200:
        die(f'refused: {answer.get("message")}', 1)
    if answer.get('note'):
        print(answer['note'])
    return answer


def print_terminal(row: dict):
    if 'sequence' in row:
        line = (f'  {row["threadId"]:<13} {row["status"]:<8} sequence {row["sequence"]}, pid {row["pid"]}, '
                f'{row["historyBytes"]} bytes and {row["historyLines"]} lines of history, '
                f'size {row["size"]}, shell {row["shell"]}')
    else:
        line = f'  {row["threadId"]:<13} {row["status"]:<8} {row["keptHistory"]} bytes of history kept'
    print(line + f'; {row["streams"]} stream(s), {row["unacked"]} chunk(s) unacked, {row["waiting"]} held back')
    print(f'      header   {row["header"]}')
    if row.get('lastRow'):
        print(f'      last row {row["lastRow"]}')
    if row['status'] in ('missing', 'closed'):
        launch = row['expectedLaunch']
        print(f'      Start    cwd {safe(launch["cwd"], 80) or "(blank: t3term must not start one)"}'
              f', worktreePath {launch.get("worktreePath")}, env {sorted(launch["env"])}')


def print_terminals(rows: list):
    for row in rows:
        print_terminal(row)
    print('  (a header is what the drawer shows once attached; a label shows once it has had a snapshot)')


# ---- commands ----


def cmd_serve(args) -> int:
    root = Path(os.path.realpath(args.dir))
    state = read_json(root / 'fixture.json')
    if not state or not owned(root, state):
        die(f'{root} is not a fixture directory made by this script', 2)
    fixture = Fixture(root, {})
    Handler.fixture = fixture
    server = Server(('127.0.0.1', 0), Handler)
    port = server.server_address[1]
    fixture.port = port
    origin = f'http://127.0.0.1:{port}'
    runtime_file = root / 'home' / '.t3' / 'userdata' / 'server-runtime.json'
    # The shape tests/cli.rs writes, after apps/server/src/serverRuntimeState.ts.
    write_json(runtime_file, {'version': 1, 'pid': os.getpid(), 'port': port, 'origin': origin,
                              'startedAt': iso(now_ms())})
    (root / 'server.pid').write_text(f'{os.getpid()}\n')
    stop = threading.Event()
    reason = {'why': 'signal'}
    for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(sig, lambda *_: stop.set())
    threading.Thread(target=server.serve_forever, kwargs={'poll_interval': 0.2}, daemon=True).start()
    fixture.ensure_world()
    fixture.log('server.listening', origin=origin, pid=os.getpid())
    deadline = time.monotonic() + args.max_seconds
    while not stop.wait(0.5):
        if time.monotonic() >= deadline:
            reason['why'] = f'--max-seconds {args.max_seconds} ran out'
            if state.get('tmux'):
                # Quit t3term while the server can still take its revokes. Ctrl+Q, since the
                # drawer sends Ctrl+C to the shell.
                tmux(state, 'send-keys', '-t', state['session'], 'C-q')
                time.sleep(4)
                tmux(state, 'kill-server')
            break
    fixture.log('server.stopping', reason=reason['why'])
    server.shutdown()
    server.server_close()
    for connection in list(fixture.connections):
        connection.close()
    for timer in fixture.timers:
        timer.cancel()
    try:
        runtime_file.unlink()
    except OSError:
        pass
    write_json(root / 'logs' / 'summary.json', fixture.state())
    fixture.log('server.stopped')
    return 0


def shim(argv: list) -> int:
    """`t3 auth session issue|revoke` for t3term, answered by this fixture's server only."""
    def fail(message: str) -> int:
        print(f't3 fixture shim: {message}', file=sys.stderr)
        return 1

    if len(argv) < 2 or argv[0] != '--fixture-dir':
        return fail('internal: --fixture-dir missing')
    root = Path(os.path.realpath(argv[1]))
    rest = argv[2:]
    if rest[:2] != ['auth', 'session'] or len(rest) < 3 or rest[2] not in ('issue', 'revoke'):
        return fail('only `auth session issue` and `auth session revoke` are supported')
    action, words = rest[2], rest[3:]
    flags, positional, scopes = {}, [], []
    i = 0
    while i < len(words):
        word = words[i]
        if word in ('--ttl', '--label', '--subject', '--scope', '--base-dir') and i + 1 < len(words):
            if word == '--scope':
                scopes.append(words[i + 1])
            else:
                flags[word] = words[i + 1]
            i += 2
        elif word.startswith('--'):
            flags[word] = True
            i += 1
        else:
            positional.append(word)
            i += 1
    expected_home = root / 'home' / '.t3'
    if Path(os.path.realpath(str(flags.get('--base-dir', '')))) != expected_home:
        return fail(f"--base-dir must be this fixture's T3 home, {expected_home}")
    runtime_file = read_json(expected_home / 'userdata' / 'server-runtime.json') or {}
    origin = str(runtime_file.get('origin', ''))
    if not re.fullmatch(r'http://127\.0\.0\.1:\d+', origin):
        return fail('the fixture server is not running')
    try:
        key = (root / 'control.key').read_text().strip()
    except OSError:
        return fail('the fixture control key is missing')
    if action == 'issue':
        route, body = '/__fixture/auth/issue', {'scopes': scopes, 'ttl': flags.get('--ttl'),
                                                 'label': flags.get('--label'), 'subject': flags.get('--subject')}
    else:
        if not positional:
            return fail('revoke needs a session id')
        route, body = '/__fixture/auth/revoke', {'sessionId': positional[0]}
    request = urllib.request.Request(origin + route, data=json.dumps(body).encode(), method='POST',
                                     headers={'Content-Type': 'application/json', 'X-Fixture-Control': key})
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        with opener.open(request, timeout=5) as response:
            answer = response.read()
    except (urllib.error.URLError, OSError) as error:
        return fail(f'the fixture server refused {action}: {error}')
    if action == 'issue':
        # The credential goes to t3term on stdout, as `t3 auth session issue --json` does.
        sys.stdout.write(answer.decode() + '\n')
    return 0


def cmd_pipe_sink(args) -> int:
    """Reads t3term's output from tmux pipe-pane into raw.ansi and an asciicast v2 file."""
    root = Path(os.path.realpath(args.dir))
    state = read_json(root / 'fixture.json') or {}
    if not NAME.match(args.cast):
        die('a cast name is letters, digits, dot, dash and underscore', 2)
    started = time.time()
    capture = root / 'capture'
    decoder = codecs.getincrementaldecoder('utf-8')('replace')
    with open(capture / 'raw.ansi', 'ab') as raw, open(capture / args.cast, 'a', encoding='utf-8') as cast:
        cast.write(json.dumps({'version': 2, 'width': state.get('cols', 160), 'height': state.get('rows', 45),
                               'timestamp': int(started), 'title': 't3term PR 41 fixture',
                               'env': {'TERM': 'xterm-256color'}}) + '\n')
        cast.flush()
        while True:
            try:
                data = os.read(0, 65536)
            except OSError:
                break
            if not data:
                break
            raw.write(data)
            raw.flush()
            text = decoder.decode(data)
            if text:
                cast.write(json.dumps([round(time.time() - started, 6), 'o', text]) + '\n')
                cast.flush()
    return 0


def launch_script(go: Path, command: list) -> str:
    """The pane's shell line: wait until the pipe to pipe-sink is attached, then become t3term."""
    return (f'while [ ! -e {shlex.quote(str(go))} ]; do sleep 0.05; done; '
            f'exec {" ".join(shlex.quote(x) for x in command)}')


def sink_command(root: Path, cast: str) -> str:
    return ' '.join(shlex.quote(x) for x in (
        'exec', sys.executable, str(SCRIPT), 'pipe-sink', '--dir', str(root), '--cast', cast))


def wait_for_t3term(state: dict):
    """t3term's pid once the pane's shell has become it, or None if it exited or took over 5 s."""
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        info = pane(state)
        if info and info[2] and state['binary'] in command_of(info[2]):
            return info[2]
        if info and info[0]:
            return None
        time.sleep(0.05)
    return None


def list_terminals(root: Path) -> list:
    status, answer = control_post(root, '/__fixture/term', {'action': 'list'})
    if status != 200:
        die(f'the fixture server answered {status}: {answer.get("message")}', 1)
    return answer.get('terminals', [])


def cmd_start(args) -> int:
    binary = Path(os.path.realpath(os.path.expanduser(args.bin)))
    if not binary.is_file() or not os.access(binary, os.X_OK):
        die(f'{binary} is not an executable t3term build (cargo build first)', 2)
    if not args.no_tmux and not shutil.which('tmux'):
        die('tmux is not on PATH; install it or use --no-tmux', 2)
    if args.cols < 80 or args.rows < 24:
        die('start at 80x24 or more; `resize` goes smaller afterwards', 2)
    root = Path(os.path.realpath(tempfile.mkdtemp(prefix=PREFIX, dir='/tmp')))
    if any(c.isspace() for c in str(root)):
        die(f'{root} has whitespace, which T3TERM_T3_COMMAND cannot carry', 1)
    nonce = secrets.token_hex(16)
    for sub in ('home/.t3/userdata', 'bin', 'logs', 'capture', 'run', 'tmp'):
        (root / sub).mkdir(parents=True, mode=0o700, exist_ok=True)
    (root / MARKER).write_text(nonce + '\n')
    fd = os.open(root / 'control.key', os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'w') as handle:
        handle.write(secrets.token_hex(32) + '\n')
    shim_path = root / 'bin' / 't3-shim'
    shim_path.write_text('#!/bin/sh\n' + ' '.join(shlex.quote(x) for x in (
        'exec', sys.executable, str(SCRIPT), 't3-shim', '--fixture-dir', str(root))) + ' "$@"\n')
    shim_path.chmod(0o700)
    state = {
        'root': str(root), 'nonce': nonce, 'binary': str(binary), 'python': sys.executable,
        'script': str(SCRIPT), 'socket': str(root / 'tmux.sock'), 'session': SESSION,
        'cols': args.cols, 'rows': args.rows, 'color': args.color, 'tmux': not args.no_tmux,
        'restarts': 0, 'createdAt': iso(now_ms()),
    }
    write_json(root / 'fixture.json', state)
    previous = os.path.realpath(CURRENT) if CURRENT.is_symlink() else None
    # Only a link to a fixture directory gives way, so a start never removes anything else.
    ours = previous is not None and Path(previous).name.startswith(PREFIX) \
        and Path(previous).parent == root.parent
    if ours or not os.path.lexists(CURRENT):
        if ours:
            CURRENT.unlink()
        CURRENT.symlink_to(root)
    else:
        print(f'fixture: {CURRENT} is not a link to a fixture directory, so it stays; pass --dir {root}',
              file=sys.stderr)

    log = open(root / 'logs' / 'server.log', 'ab')
    server = subprocess.Popen([sys.executable, str(SCRIPT), 'serve', '--dir', str(root),
                               '--max-seconds', str(args.max_seconds)],
                              stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True,
                              close_fds=True)
    log.close()
    state['serverPid'] = server.pid
    write_json(root / 'fixture.json', state)
    runtime_file = root / 'home' / '.t3' / 'userdata' / 'server-runtime.json'
    deadline = time.monotonic() + 10
    while not runtime_file.exists():
        if server.poll() is not None or time.monotonic() > deadline:
            print((root / 'logs' / 'server.log').read_text()[-2000:], file=sys.stderr)
            terminate(server.pid, [SCRIPT.name, 'serve', str(root)], 'fixture server')
            die(f'the fixture server did not start; files are in {root}')
        time.sleep(0.05)
    origin = read_json(runtime_file)['origin']
    me = f'{shlex.quote(sys.executable)} {shlex.quote(str(SCRIPT))}'

    command = t3term_command(root, state)
    if args.no_tmux:
        print(f'Fixture server is up at {origin}. Files: {root}\n')
        print('Run t3term in any terminal with exactly this line:\n')
        print('  ' + ' '.join(shlex.quote(x) for x in command) + '\n')
        print(f'Then: {me} term list   |   {me} status   |   {me} cleanup')
        return 0

    go = root / 'run' / 'go'
    steps = [
        ('new-session', '-d', '-s', SESSION, '-x', str(args.cols), '-y', str(args.rows), '/bin/sh', '-c',
         launch_script(go, command)),
        ('set-option', '-g', 'remain-on-exit', 'on'),
        ('set-option', '-g', 'status', 'off'),
        ('set-option', '-g', 'window-size', 'manual'),
        ('set-option', '-g', 'escape-time', '10'),
        ('pipe-pane', '-t', SESSION, sink_command(root, 'session.cast')),
    ]
    for step in steps:
        result = tmux(state, *step)
        if result.returncode != 0:
            tmux(state, 'kill-server')
            terminate(server.pid, [SCRIPT.name, 'serve', str(root)], 'fixture server')
            die(f'tmux {step[0]} failed: {result.stderr.strip()}; files are in {root}')
    go.write_text('')
    mark(root, 'fixture.t3term_started', binary=str(binary), cols=args.cols, rows=args.rows)
    t3term_pid = wait_for_t3term(state)
    state['t3termPid'] = t3term_pid
    write_json(root / 'fixture.json', state)
    time.sleep(1.0)
    info = pane(state)
    if info is None or info[0]:
        print(f'fixture: t3term exited at once (status {info[1] if info else "unknown"}). '
              f'See {root}/capture/raw.ansi and {root}/logs/events.jsonl, then run: {me} cleanup',
              file=sys.stderr)
        return 1
    previous_note = ''
    if previous and previous != str(root):
        previous_note = f'  previous   {previous} is still there; it was current before this one\n'
    print(f"""Fixture is up.
  dir        {root}
  server     {origin}, pid {server.pid}
  t3term     pid {t3term_pid}, {args.cols}x{args.rows}, color {args.color}
  log        {root}/logs/events.jsonl
  raw        {root}/capture/raw.ansi
  recording  {root}/capture/session.cast  (asciicast v2)
{previous_note}
Terminals now:""")
    print_terminals(list_terminals(root))
    print(f"""
Watch:     env -u TMUX tmux -S {root}/tmux.sock attach -r -t {SESSION}     (read-only; detach with C-b d)
Open:      {me} click --text "Running: a shell"
Drawer:    {me} keys F4   |   keys F6   |   keys Enter   |   click --text "Start terminal"
Type:      {me} type "echo hello"   |   type showkeys   |   keys C-c Tab Up F5 M-x   |   keys a b c --gap 0
Paste:     {me} paste --text "hello paste"   |   paste --bytes {PASTE_BYTES - PASTE_BRACKETS}   |   paste --bytes {PASTE_BYTES - PASTE_BRACKETS + 1}
Terminal:  {me} term list   |   term activity --label "cargo test"   |   term exit --code 3   |   term restart
           term error --message "pty broke"   |   term close   |   term start   |   term clear
Skip:      {me} term stranger   |   term old   |   term duplicate   |   term malformed
Limits:    {me} term ignore-window --chunks 20   |   term oversize   |   type "flood 16"
Failures:  {me} term fail-writes on   |   term fail-resize on   |   term missing-cwd on   |   term slow 800
Attach:    {me} term hold-attach 5   |   drop --hold 3   |   drop --hold 60   |   auth permissions extra
Size:      {me} resize 120 36   |   resize 80 24   |   resize 40 12   |   resize 16 8
Status:    {me} mark before-hostile, then {me} status   |   {me} check
Capture:   {me} capture running
Stop:      {me} stop      (C-q to t3term, which revokes its fixture sessions; files stay)
Clean up:  {me} cleanup
`term` acts on thr-running unless --thread names another. The module docstring
({me} --help) walks through every case in order.""")
    return 0


def cmd_term(args) -> int:
    root, _ = locate(args.dir)
    action = args.action
    if action == 'list':
        print_terminals(list_terminals(root))
        return 0
    if action in ('hold-attach', 'slow', 'fail-writes', 'fail-resize', 'missing-cwd'):
        post(root, '/__fixture/term', {'action': action, 'value': args.value})
        return 0
    request = {'action': action, 'threadId': args.thread}
    if action == 'start':
        request['starting'] = args.starting
    elif action == 'exit':
        request.update(code=args.code, signal=args.signal)
    elif action == 'error':
        request['message'] = args.message
    elif action == 'close':
        request['deleteHistory'] = args.delete_history
    elif action == 'activity':
        request.update(label=args.label, preset=args.preset)
    elif action == 'output':
        request.update(text=args.text, escapes=args.escapes, noSequence=args.no_sequence)
    elif action == 'ignore-window':
        request['chunks'] = args.chunks
    post(root, '/__fixture/term', request)
    row = next((r for r in list_terminals(root) if r['threadId'] == args.thread), None)
    if row:
        print_terminal(row)
    return 0


def cmd_auth(args) -> int:
    root, _ = locate(args.dir)
    post(root, '/__fixture/auth/permissions', {'mode': args.mode})
    return 0


def cmd_drop(args) -> int:
    root, _ = locate(args.dir)
    post(root, '/__fixture/drop', {'hold': args.hold, 'everything': args.everything})
    return 0


def cmd_mark(args) -> int:
    root, _ = locate(args.dir)
    if not NAME.match(args.name):
        die('a mark name is letters, digits, dot, dash and underscore', 2)
    offset = raw_bytes(root) or 0
    mark(root, 'fixture.mark', name=args.name, rawOffset=offset)
    print(f'mark {args.name} at byte {offset} of capture/raw.ansi; `status` counts leaks after it in '
          f'controlLeaksSinceMark')
    return 0


def capture_screen(root: Path, state: dict, name: str):
    plain = tmux(state, 'capture-pane', '-p', '-t', state['session'])
    styled = tmux(state, 'capture-pane', '-p', '-e', '-t', state['session'])
    if plain.returncode != 0 or styled.returncode != 0:
        die(f'tmux capture-pane failed: {(plain.stderr or styled.stderr).strip()}')
    text_path, ansi_path = root / 'capture' / f'{name}.txt', root / 'capture' / f'{name}.ans'
    text_path.write_text(plain.stdout)
    ansi_path.write_text(styled.stdout)
    return plain.stdout, text_path, ansi_path


def cmd_capture(args) -> int:
    root, state = locate(args.dir)
    if not state.get('tmux'):
        die('capture needs the tmux pane', 2)
    if not NAME.match(args.name):
        die('a capture name is letters, digits, dot, dash and underscore', 2)
    _, text_path, ansi_path = capture_screen(root, state, args.name)
    mark(root, 'fixture.capture', name=args.name)
    print(f'{text_path}\n{ansi_path}')
    return 0


def cmd_keys(args) -> int:
    root, state = locate(args.dir)
    if not state.get('tmux'):
        die('keys needs the tmux pane', 2)
    for i, key in enumerate(args.keys):
        result = tmux(state, 'send-keys', '-t', state['session'], key)
        if result.returncode != 0:
            die(f'tmux send-keys failed: {result.stderr.strip()}')
        mark(root, 'fixture.keys', key=key)
        if args.gap > 0 and i + 1 < len(args.keys):
            time.sleep(args.gap)
    return 0


def cmd_type(args) -> int:
    """Types text into t3term as keys, then Enter. The log keeps only its length."""
    root, state = locate(args.dir)
    if not state.get('tmux'):
        die('type needs the tmux pane', 2)
    if args.text:
        result = tmux(state, 'send-keys', '-t', state['session'], '-l', '--', args.text)
        if result.returncode != 0:
            die(f'tmux send-keys failed: {result.stderr.strip()}')
    if not args.no_enter:
        time.sleep(0.05)
        tmux(state, 'send-keys', '-t', state['session'], 'Enter')
    mark(root, 'fixture.type', length=len(args.text), enter=not args.no_enter)
    return 0


def paste_body(size: int) -> str:
    """`size` characters of printable ASCII in rows that end in LF."""
    row = 'paste row {:06d} of the fixture body\n'
    rows = size // len(row.format(0)) + 1
    return ''.join(row.format(n) for n in range(rows))[:size]


def cmd_paste(args) -> int:
    """Pastes text into t3term through tmux, bracketed when t3term asked tmux for it, as one
    paste from a terminal. Prints what the drawer should do with it."""
    root, state = locate(args.dir)
    if not state.get('tmux'):
        die('paste needs the tmux pane', 2)
    if args.text is not None:
        body = unescape(args.text) if args.escapes else args.text
    else:
        if not 1 <= args.bytes <= 4 << 20:
            die('--bytes takes 1 to 4194304', 2)
        body = paste_body(args.bytes)
    if not body:
        die('a paste needs text', 2)
    path = root / 'tmp' / f'paste-{secrets.token_hex(4)}.txt'
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'wb') as handle:
        handle.write(body.encode('utf-8'))
    try:
        # -p brackets the paste when t3term turned bracketed paste on, which it does at start;
        # -r keeps each LF as LF; -d drops the buffer after.
        for step in (('load-buffer', '-b', 'pr41', str(path)),
                     ('paste-buffer', '-p', '-r', '-d', '-b', 'pr41', '-t', state['session'])):
            result = tmux(state, *step)
            if result.returncode != 0:
                die(f'tmux {step[0]} failed: {result.stderr.strip()}')
    finally:
        path.unlink()
    kept = body.replace('\x1b', '')
    digest = sha(kept)[:12]
    bracketed = len(kept.encode('utf-8')) + PASTE_BRACKETS
    plain = len(body.replace('\r\n', '\r').replace('\n', '\r').encode('utf-8'))
    mark(root, 'fixture.paste', chars=len(body), sha=digest)
    print(f'pasted {len(body)} characters, LF {body.count(chr(10))}, CR {body.count(chr(13))}; '
          f'the shell should report sha256 {digest}')
    for mode, size in (('bracketed (the prompt asked)', bracketed), ('unbracketed (paste off)', plain)):
        verdict = 'goes whole' if size <= PASTE_BYTES else 'is refused whole, with no write'
        print(f'  {mode}: {size} bytes to send, so it {verdict} (the drawer takes at most {PASTE_BYTES})')
    return 0


def text_places(screen: str, text: str) -> list:
    """(column, row) of each place `text` starts on screen, rows top to bottom, counting a wide
    character as two columns."""
    places = []
    for row, line in enumerate(screen.split('\n')):
        start = line.find(text)
        while start >= 0:
            places.append((cells(line[:start]), row))
            start = line.find(text, start + 1)
    return places


def send_mouse(state: dict, report: str):
    result = tmux(state, 'send-keys', '-t', state['session'], '-H', *(f'{b:02x}' for b in report.encode()))
    if result.returncode != 0:
        die(f'tmux send-keys failed: {result.stderr.strip()}')


def cmd_click(args) -> int:
    root, state = locate(args.dir)
    if not state.get('tmux'):
        die('click needs the tmux pane', 2)
    if args.text:
        plain = tmux(state, 'capture-pane', '-p', '-t', state['session'])
        if plain.returncode != 0:
            die(f'tmux capture-pane failed: {plain.stderr.strip()}')
        places = [p for p in text_places(plain.stdout, args.text) if p[0] >= args.from_col and p[1] >= args.from_row]
        if args.nth < 1 or len(places) < args.nth:
            die(f'that text shows {len(places)} time(s) on screen now at column {args.from_col} and row '
                f'{args.from_row} or past them', 1)
        col, row = places[args.nth - 1]
    else:
        col, row = args.at
        if not (0 <= col < state['cols'] and 0 <= row < state['rows']):
            die('--at takes a column and a row inside the pane, counted from 0', 2)
    if not 1 <= args.repeat <= 50:
        die('--repeat takes 1 to 50', 2)
    # SGR (1006) mouse reports count from 1: a left-button press, then its release. A wheel notch
    # is one press of button 64 (up) or 65 (down), with no release.
    if args.button == 'left':
        report = f'\x1b[<0;{col + 1};{row + 1}M\x1b[<0;{col + 1};{row + 1}m'
    else:
        report = f'\x1b[<{64 if args.button == "wheel-up" else 65};{col + 1};{row + 1}M'
    send_mouse(state, report * args.repeat)
    mark(root, 'fixture.click', col=col, row=row, textLength=len(args.text) if args.text else None,
         nth=args.nth if args.text else None, button=args.button, repeat=args.repeat)
    print(f'{"clicked" if args.button == "left" else args.button} column {col}, row {row}'
          + (f' x{args.repeat}' if args.repeat > 1 else '')
          + (f', match {args.nth} of the text' if args.text else ''))
    return 0


def cmd_resize(args) -> int:
    root, state = locate(args.dir)
    if not state.get('tmux'):
        die('resize needs the tmux pane; with --no-tmux, resize your own terminal', 2)
    if not (10 <= args.cols <= 500 and 5 <= args.rows <= 200):
        die('resize takes 10 to 500 columns and 5 to 200 rows', 2)
    result = tmux(state, 'resize-window', '-t', state['session'], '-x', str(args.cols), '-y', str(args.rows))
    if result.returncode != 0:
        die(f'tmux resize-window failed: {result.stderr.strip()}')
    state.update(cols=args.cols, rows=args.rows)
    write_json(root / 'fixture.json', state)
    mark(root, 'fixture.resize', cols=args.cols, rows=args.rows)
    print(f'resized to {args.cols}x{args.rows}')
    return 0


def quit_t3term(root: Path, state: dict, why: str):
    """Ctrl+Q to t3term, which the drawer leaves to the TUI, then waits up to 8 s for it to
    exit. The pane's (dead, status, pid) after, or None when the session is gone."""
    info = pane(state)
    if info and not info[0]:
        tmux(state, 'send-keys', '-t', state['session'], 'C-q')
        mark(root, 'fixture.keys', key='C-q', reason=why)
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            info = pane(state)
            if not info or info[0]:
                break
            time.sleep(0.1)
    return info


def cmd_restart(args) -> int:
    root, state = locate(args.dir)
    if not state.get('tmux'):
        die('restart needs the tmux pane; with --no-tmux, quit t3term and run the env line again', 2)
    if pane(state) is None:
        die('the tmux session is gone; start a new fixture', 1)
    info = quit_t3term(root, state, 'restart')
    if not info or not info[0]:
        die('t3term did not quit within 8 s; run stop instead', 1)
    mark(root, 'fixture.t3term_exit', status=info[1])
    count = state.get('restarts', 0) + 1
    go, cast = root / 'run' / f'go-{count}', f'session-{count}.cast'
    # The new shell waits for `go`, so the pipe is attached before t3term writes anything.
    steps = (('respawn-pane', '-k', '-t', state['session'], '/bin/sh', '-c',
              launch_script(go, t3term_command(root, state))),
             ('pipe-pane', '-t', state['session'], sink_command(root, cast)))
    for step in steps:
        result = tmux(state, *step)
        if result.returncode != 0:
            die(f'tmux {step[0]} failed: {result.stderr.strip()}')
    go.write_text('')
    pid = wait_for_t3term(state)
    state.update(t3termPid=pid, restarts=count)
    write_json(root / 'fixture.json', state)
    mark(root, 'fixture.t3term_restarted', restart=count, pid=pid)
    if pid is None:
        print(f'fixture: t3term did not come back; see {root}/capture/raw.ansi', file=sys.stderr)
        return 1
    print(f't3term restarted (restart {count}), pid {pid}, same HOME. It issues a terminal session again only '
          f'once a drawer shows.\nrecording: {root}/capture/{cast}')
    return 0


def read_log(root: Path) -> list:
    records = []
    try:
        with open(root / 'logs' / 'events.jsonl') as handle:
            for line in handle:
                try:
                    records.append(json.loads(line))
                except ValueError:
                    pass
    except OSError:
        pass
    return records


def control_leaks(root: Path, offset: int = 0) -> dict:
    """How often each of LEAK_MARKERS shows in what t3term wrote to the pane from byte `offset`."""
    try:
        with open(root / 'capture' / 'raw.ansi', 'rb') as handle:
            handle.seek(offset)
            raw = handle.read()
    except OSError:
        return {name: None for name in LEAK_MARKERS}
    return {name: raw.count(marker) for name, marker in LEAK_MARKERS.items()}


def screen_now(state: dict):
    if not state.get('tmux') or pane(state) is None:
        return None
    result = tmux(state, 'capture-pane', '-p', '-t', state['session'])
    return result.stdout if result.returncode == 0 else None


def screen_leaks(screen):
    """How often each of SCREEN_MARKERS shows on the screen now, or None without a tmux pane. A
    marker split across two rows doesn't count, so a 0 says only that no row holds a whole one."""
    if screen is None:
        return None
    return {marker: screen.count(marker) for marker in SCREEN_MARKERS}


def raw_bytes(root: Path):
    try:
        return (root / 'capture' / 'raw.ansi').stat().st_size
    except OSError:
        return None


def prefixed(counts: dict, prefix: str) -> dict:
    return {k[len(prefix):]: v for k, v in sorted(counts.items()) if k.startswith(prefix)}


def status_report(root: Path, state: dict) -> dict:
    """What `status` prints: the server's counts and the log, summed up."""
    records = read_log(root)
    running = bool(state.get('serverPid')) and alive(state['serverPid'])
    live = None
    if running:
        status, answer = control_post(root, '/__fixture/state', {})
        live = answer if status == 200 else None
    if live is None:
        # The last state the server wrote as it stopped.
        live = read_json(root / 'logs' / 'summary.json')
    counts = (live or {}).get('counts') or {}
    maxima = (live or {}).get('maxima') or {}
    issued = Counter(r.get('kind') for r in records if r.get('event') == 'auth.issue')
    revoked = Counter(r.get('kind') for r in records if r.get('event') == 'auth.revoke' and r.get('kind'))
    first = next((i for i, r in enumerate(records)
                  if r.get('event') == 'auth.issue' and r.get('kind') == 'terminal'), None)
    before = None
    if first is not None:
        before = next((r for r in reversed(records[:first]) if r.get('by') == 'helper'
                       and r.get('event') in ('fixture.keys', 'fixture.click', 'fixture.type', 'fixture.paste',
                                              'fixture.t3term_started', 'fixture.t3term_restarted')), None)
    attaches = [r for r in records if r.get('event') == 'rpc.request' and r.get('tag') == 'terminal.attach']
    marks = [r for r in records if r.get('event') == 'fixture.mark']
    last_mark = marks[-1] if marks else None
    screen = screen_now(state)
    report = {
        'dir': str(root),
        'serverRunning': running,
        'tmuxSession': pane(state) is not None if state.get('tmux') else None,
        'sessionsIssued': dict(issued),
        'terminalSessionsIssued': issued['terminal'],
        'firstTerminalIssueAfter': (f'{before["event"]} {before.get("key") or before.get("button") or ""}'.strip()
                                    if before else None),
        'sessionsRevoked': dict(revoked),
        'sessionChecks': prefixed(counts, 'auth.check.'),
        'permissions': (live or {}).get('permissions'),
        'socketsOpened': prefixed(counts, 'ws.open.'),
        'socketsClosed': prefixed(counts, 'ws.close.'),
        'socketsHeld': prefixed(counts, 'ws.held.'),
        'attaches': {k: v for k, v in prefixed(counts, 'attach.').items() if not k.startswith(('violation.',
                                                                                            'launchMismatch.'))},
        'attachKeys': dict(Counter(','.join((r.get('payload') or {}).get('keys') or []) for r in attaches)),
        'attachViolations': prefixed(counts, 'attach.violation.'),
        'launchMismatches': prefixed(counts, 'attach.launchMismatch.'),
        'terminalInterrupts': counts.get('terminal.interrupt', 0),
        'writes': prefixed(counts, 'write.'),
        'writesOverlapped': counts.get('write.overlapped', 0),
        'largestWrite': maxima.get('write.largest', 0),
        'resizes': prefixed(counts, 'resize.'),
        'resizesSent': [f'{r.get("cols")}x{r.get("rows")}' for r in records if r.get('event') == 'term.resize'],
        'window': prefixed(counts, 'window.'),
        'maxUnacked': maxima.get('window.maxUnacked', 0),
        'maxChunkBytes': maxima.get('window.maxChunkBytes', 0),
        'maxWaiting': maxima.get('window.maxWaiting', 0),
        'acksOnOtherStreams': prefixed(counts, 'rpc.ackOther.'),
        'scopeViolations': counts.get('scopeViolations', 0),
        'scopeRefusals': prefixed(counts, 'scope.refused.'),
        'unexpectedTerminalRpcs': prefixed(counts, 'terminal.unexpected.'),
        'rpcRequestsByTag': (live or {}).get('rpcRequestsByTag'),
        'commandsRefused': counts.get('command.refused', 0),
        'shell': prefixed(counts, 'shell.'),
        'controls': prefixed(counts, 'control.'),
        'drops': [r.get('holdSeconds') for r in records if r.get('event') == 'drop'],
        'captures': [r.get('name') for r in records if r.get('event') == 'fixture.capture'],
        'resizesAsked': [f'{r.get("cols")}x{r.get("rows")}' for r in records if r.get('event') == 'fixture.resize'],
        'restarts': state.get('restarts', 0),
        'rawAnsiBytes': raw_bytes(root),
        'controlLeaks': control_leaks(root),
        'lastMark': (f'{last_mark.get("name")} at byte {last_mark.get("rawOffset")}' if last_mark else None),
        'controlLeaksSinceMark': control_leaks(root, last_mark.get('rawOffset') or 0) if last_mark else None,
        'screenLeaks': screen_leaks(screen),
        'duplicateOnScreen': screen.count(DUPLICATE) if screen is not None else None,
    }
    return report


def cmd_status(args) -> int:
    root, state = locate(args.dir)
    report = status_report(root, state)
    if args.json:
        print(json.dumps(report, indent=1))
        return 0
    for key, value in report.items():
        print(f'{key:<26} {json.dumps(value)}')
    return 0


def broken_rules(report: dict) -> list:
    """The rules in `report` that a run broke, one line each. These hold at every step of the
    walkthrough, so `check` can run after any of them. The rest of what a step should show
    needs a reader and the captures."""
    broken = []

    def rule(kept: bool, line: str):
        if not kept:
            broken.append(line)

    def nonzero(counts) -> dict:
        return {name: count for name, count in (counts or {}).items() if count}

    rule(report['scopeViolations'] == 0,
         f'scopeViolations {report["scopeViolations"]}: a request reached the server without its scope')
    for name, why in (('attachViolations', 'a terminal.attach broke its contract'),
                      ('launchMismatches', "a launch differed from the thread's cwd, worktree or env"),
                      ('unexpectedTerminalRpcs', 'the drawer sent an RPC it never sends')):
        rule(not nonzero(report[name]), f'{name} {json.dumps(nonzero(report[name]))}: {why}')
    rule(report['maxUnacked'] <= WINDOW_CHUNKS,
         f'maxUnacked {report["maxUnacked"]}: more than {WINDOW_CHUNKS} chunks waited for an Ack')
    rule(report['window'].get('excessAcks', 0) == 0,
         f'excessAcks {report["window"].get("excessAcks")}: an Ack came for no chunk')
    stray = report['acksOnOtherStreams'].get('terminal', 0)
    rule(stray == 0, f'acksOnOtherStreams.terminal {stray}: an Ack on the terminal socket named no open stream')
    rule(report['writesOverlapped'] == 0,
         f'writesOverlapped {report["writesOverlapped"]}: a write went before the last one was answered')
    rule(report['largestWrite'] <= WRITE_LIMIT,
         f'largestWrite {report["largestWrite"]}: a write was over {WRITE_LIMIT} UTF-16 units')
    bad_writes = report['writes'].get('badInput', 0)
    rule(bad_writes == 0,
         f'writes.badInput {bad_writes}: a write named no terminal, or its data was empty, not text '
         f'or over {WRITE_LIMIT} UTF-16 units')
    if report['tmuxSession'] is not None:
        rule(report['rawAnsiBytes'] is not None, 'capture/raw.ansi is missing, so leaks went unchecked')
        rule(not nonzero(report['controlLeaks']),
             f'controlLeaks {json.dumps(nonzero(report["controlLeaks"]))}: t3term wrote what it never writes')
    if report['screenLeaks'] is not None:
        rule(not nonzero(report['screenLeaks']),
             f'screenLeaks {json.dumps(nonzero(report["screenLeaks"]))}: text that must stay hidden is on screen')
    if report['duplicateOnScreen'] is not None:
        sent = report['controls'].get('duplicate', 0)
        rule(report['duplicateOnScreen'] <= sent,
             f'duplicateOnScreen {report["duplicateOnScreen"]} after {sent} `term duplicate`: an event showed twice')
    return broken


def cmd_check(args) -> int:
    root, state = locate(args.dir)
    broken = broken_rules(status_report(root, state))
    for line in broken:
        print(f'broken: {line}')
    if not broken:
        print('check: every rule held')
    mark(root, 'fixture.check', broken=len(broken))
    return 1 if broken else 0


def cmd_stop(args) -> int:
    root, state = locate(args.dir)
    notes = []
    if state.get('tmux'):
        info = quit_t3term(root, state, 'stop')
        if info and info[0]:
            notes.append(f't3term: exited with status {info[1]}')
            mark(root, 'fixture.t3term_exit', status=info[1])
        notes.append(terminate(state.get('t3termPid'), [state['binary']], 't3term'))
        tmux(state, 'kill-server')
        notes.append("tmux: private server on this fixture's socket stopped")
    notes.append(terminate(state.get('serverPid'), [SCRIPT.name, 'serve', str(root)], 'fixture server'))
    mark(root, 'fixture.stopped')
    print('\n'.join(notes))
    print(f'files kept in {root}')
    return 0


def cmd_cleanup(args) -> int:
    root, state = locate(args.dir)
    cmd_stop(args)
    time.sleep(0.3)  # lets pipe-sink see the end of its pipe
    if not owned(root, state):
        die(f'{root} no longer looks like this fixture; not deleting it')
    if CURRENT.is_symlink() and os.path.realpath(CURRENT) == str(root):
        CURRENT.unlink()
    shutil.rmtree(root)
    print(f'deleted {root}')
    return 0


def cmd_env(args) -> int:
    root, state = locate(args.dir)
    print(' '.join(shlex.quote(x) for x in t3term_command(root, state)))
    return 0


def main(argv: list) -> int:
    if len(argv) > 1 and argv[1] == 't3-shim':
        return shim(argv[2:])
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest='command', required=True)
    with_dir = []  # parsers that take --dir

    start = commands.add_parser('start', help='start the server and t3term in tmux')
    start.add_argument('--bin', default='target/debug/t3term', help='the t3term binary (default target/debug/t3term)')
    start.add_argument('--cols', type=int, default=160)
    start.add_argument('--rows', type=int, default=45)
    start.add_argument('--color', choices=('truecolor', '256'), default='truecolor')
    start.add_argument('--no-tmux', action='store_true', help='start only the server and print the t3term line')
    start.add_argument('--max-seconds', type=float, default=7200, help='the server stops itself after this')

    serve = commands.add_parser('serve', help='internal: run the fake server in the foreground')
    serve.add_argument('--dir', required=True)
    serve.add_argument('--max-seconds', type=float, default=7200)

    sink = commands.add_parser('pipe-sink', help='internal: tmux pipe-pane target')
    sink.add_argument('--dir', required=True)
    sink.add_argument('--cast', default='session.cast')

    term = commands.add_parser('term', help="change a thread's term-1 as T3 or the desktop would")
    actions = term.add_subparsers(dest='action', required=True)
    with_thread = []
    with_dir.append(actions.add_parser('list', help='each terminal and the header the drawer should show'))
    start_term = actions.add_parser('start', help='start term-1 as the desktop would, counting from 0')
    start_term.add_argument('--starting', type=float, default=0.0,
                            help='show starting this many seconds first (not T3)')
    with_thread.append(start_term)
    for name, what in (('restart', 'restart term-1: its history goes and attach streams get restarted'),
                       ('clear', 'a cleared event'),
                       ('malformed', 'an output event whose data is 42 (not T3)'),
                       ('stranger', 'events for term-2, another thread and a future type (not T3)'),
                       ('duplicate', f'{DUPLICATE} twice with one sequence (not T3)'),
                       ('old', 'an event from before the last one (not T3)'),
                       ('oversize', 'one output chunk of about 57.4 MiB (not T3)')):
        with_thread.append(actions.add_parser(name, help=what))
    term_exit = actions.add_parser('exit', help='the shell exits with --code (default 0) or --signal')
    how = term_exit.add_mutually_exclusive_group()
    how.add_argument('--code', type=int)
    how.add_argument('--signal', type=int)
    with_thread.append(term_exit)
    term_error = actions.add_parser('error', help='an error event: the shell failed')
    term_error.add_argument('--message', default='The fixture failed this shell on purpose.')
    with_thread.append(term_error)
    term_close = actions.add_parser('close', help='T3 closes term-1; attach streams stay open')
    term_close.add_argument('--delete-history', action='store_true')
    with_thread.append(term_close)
    term_activity = actions.add_parser('activity', help='an activity event with a label')
    label = term_activity.add_mutually_exclusive_group(required=True)
    label.add_argument('--label')
    label.add_argument('--preset', choices=(*LABEL_PRESETS, 'idle'))
    with_thread.append(term_activity)
    term_output = actions.add_parser('output', help='output on term-1')
    term_output.add_argument('text')
    term_output.add_argument('--escapes', action='store_true', help='turn \\xHH, \\uHHHH, \\e, \\r, \\n into what they name')
    term_output.add_argument('--no-sequence', action='store_true', help='leave the sequence out')
    with_thread.append(term_output)
    window = actions.add_parser('ignore-window', help='chunks past the window in one frame (not T3)')
    window.add_argument('--chunks', type=int, default=20)
    with_thread.append(window)
    hold = actions.add_parser('hold-attach', help='answer each terminal.attach after S seconds; 0 ends it')
    hold.add_argument('value', type=float, metavar='SECONDS')
    slow = actions.add_parser('slow', help='feed each write and apply each resize MS milliseconds late')
    slow.add_argument('value', type=int, metavar='MS')
    toggles = [hold, slow]
    for name, what in (('fail-writes', 'each terminal.write fails with TerminalWriteError'),
                       ('fail-resize', 'each terminal.resize fails with TerminalResizeError'),
                       ('missing-cwd', 'a start fails with TerminalCwdNotFoundError')):
        toggle = actions.add_parser(name, help=what)
        toggle.add_argument('value', choices=('on', 'off'))
        toggles.append(toggle)
    for sub in with_thread:
        sub.add_argument('--thread', default='thr-running', help='default thr-running')
    with_dir.extend(with_thread + toggles)

    auth = commands.add_parser('auth', help='what /api/auth/session says of terminal sessions')
    auth.add_argument('what', choices=('permissions',))
    auth.add_argument('mode', choices=PERMISSION_MODES)
    with_dir.append(auth)

    drop = commands.add_parser('drop', help='close the terminal sockets and answer /ws with 503 for a while')
    drop.add_argument('--hold', type=float, default=3.0, help='seconds (default 3, at most 120)')
    drop.add_argument('--everything', action='store_true', help="the TUI's own socket too")
    with_dir.append(drop)

    marker = commands.add_parser('mark', help='note where capture/raw.ansi ends now, for controlLeaksSinceMark')
    marker.add_argument('name')
    with_dir.append(marker)

    capture = commands.add_parser('capture', help='save the screen as NAME.txt and NAME.ans')
    capture.add_argument('name')
    with_dir.append(capture)

    keys = commands.add_parser('keys', help='send tmux keys to t3term and log them')
    keys.add_argument('keys', nargs='+')
    keys.add_argument('--gap', type=float, default=0.2)
    with_dir.append(keys)

    typed = commands.add_parser('type', help='type text as keys, then Enter')
    typed.add_argument('text')
    typed.add_argument('--no-enter', action='store_true')
    with_dir.append(typed)

    paste = commands.add_parser('paste', help='paste text through tmux as a terminal pastes it')
    body = paste.add_mutually_exclusive_group(required=True)
    body.add_argument('--text')
    body.add_argument('--bytes', type=int, help='this many characters of ASCII rows')
    paste.add_argument('--escapes', action='store_true', help='with --text, turn \\xHH and the like into what they name')
    with_dir.append(paste)

    click = commands.add_parser('click', help='send t3term a left click or wheel notches by SGR mouse report')
    where = click.add_mutually_exclusive_group(required=True)
    where.add_argument('--text', help='the first place this text shows on screen now, or the --nth')
    where.add_argument('--at', nargs=2, type=int, metavar=('COL', 'ROW'), help='0-based')
    click.add_argument('--nth', type=int, default=1, help='with --text, which place it shows, from 1')
    click.add_argument('--from-col', type=int, default=0, help='with --text, count only places at this column or right')
    click.add_argument('--from-row', type=int, default=0, help='with --text, count only places on this row or below')
    click.add_argument('--button', choices=('left', 'wheel-up', 'wheel-down'), default='left')
    click.add_argument('--repeat', type=int, default=1, help='send it this many times (1 to 50)')
    with_dir.append(click)

    resize = commands.add_parser('resize', help="resize t3term's tmux window")
    resize.add_argument('cols', type=int)
    resize.add_argument('rows', type=int)
    with_dir.append(resize)

    with_dir.append(commands.add_parser('restart', help='quit t3term with C-q and start it again with the same HOME'))

    status = commands.add_parser('status', help='summarize the event log and the live server')
    status.add_argument('--json', action='store_true')
    with_dir.append(status)
    with_dir.append(commands.add_parser('check', help="exit 1 if the run broke a rule that holds at every step"))

    with_dir.append(commands.add_parser('stop', help='quit t3term, stop tmux and the server; keep files'))
    with_dir.append(commands.add_parser('cleanup', help='stop, then delete the fixture directory'))
    with_dir.append(commands.add_parser('env', help='print the env -i line that runs t3term against this fixture'))

    # On the parsers that run, not on `term` itself, so a nested parser's default can't
    # overwrite a --dir given before its action.
    for sub in with_dir:
        sub.add_argument('--dir', help='fixture directory (default: the current one)')

    args = parser.parse_args(argv[1:])
    handler = {
        'start': cmd_start, 'serve': cmd_serve, 'pipe-sink': cmd_pipe_sink, 'term': cmd_term, 'auth': cmd_auth,
        'drop': cmd_drop, 'mark': cmd_mark, 'capture': cmd_capture, 'keys': cmd_keys, 'type': cmd_type,
        'paste': cmd_paste, 'click': cmd_click, 'resize': cmd_resize, 'restart': cmd_restart,
        'status': cmd_status, 'check': cmd_check, 'stop': cmd_stop, 'cleanup': cmd_cleanup, 'env': cmd_env,
    }[args.command]
    return handler(args)


if __name__ == '__main__':
    sys.exit(main(sys.argv))
