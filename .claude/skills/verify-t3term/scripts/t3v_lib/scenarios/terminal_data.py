"""The terminal scenario's constants, the helpers its terminals and checks share, and its world.

The limits follow T3 at f570bd2: packages/contracts/src/terminal.ts, apps/server/src/terminal/
Manager.ts and OutputProtocol.ts. The drawer's own limits follow t3term's src/tui/terminal.rs.
"""

from __future__ import annotations

import base64
import re
import unicodedata

from ..text import DAY, HOUR, MINUTE, cells, iso, safe
from ..world import Thread

DESK_ROOT = '/fixture/terminal-desk'
TERMINAL_ID = 'term-1'
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
# TerminalWriteInput's longest data, and the longest label, in UTF-16 code units (terminal.ts).
WRITE_LIMIT = 65536
LABEL_LIMIT = 128
# How the fixture cuts output: a PTY read is at most this long here, and a chunk holds at most
# this many events.
OUTPUT_PIECE = 4096
EVENTS_PER_CHUNK = 4
# The longest line the fake shell keeps at its prompt.
LINE_LIMIT = 2000
# The fake shell's pids, and the sequence the seeded terminals have counted to.
FIRST_PID = 41001
SEEDED_SEQUENCE = 120
# What the drawer never sends. Each one counts in unexpectedTerminalRpcs.
UNEXPECTED_TERMINAL = ('terminal.open', 'terminal.observe', 'terminal.clear', 'terminal.restart',
                       'terminal.close', 'subscribeTerminalEvents', 'subscribeTerminalMetadata')
# Fields T3's attach input has that the drawer never sends. Each one counts in attachViolations.
ATTACH_NEVER = ('cols', 'rows', 'restartIfNotRunning', 'providerInstanceId')
# The queries T3 keeps out of history (shouldStripCsiSequence in Manager.ts), simplified.
QUERY = re.compile(r'\x1b\[[0-9;?]*n|\x1b\[[0-9;?]*R|\x1b\[[>0-9;?]*c')
# A CSI sequence that output ended before its final byte. T3 holds it until the next output
# finishes it (pendingHistoryControlSequence in Manager.ts).
UNFINISHED = re.compile(r'\x1b(?:\[[\x20-\x3f]*)?\Z')
PASTE_START, PASTE_END = '\x1b[200~', '\x1b[201~'

CLIPBOARD = 'T3V-CLIPBOARD'
HOSTILE_OUTPUT = (
    '\x1b]52;c;' + base64.b64encode(CLIPBOARD.encode()).decode() + '\x07'
    '\x1b]0;T3V-WINDOW-TITLE\x07'
    '\x1b]2;T3V-WINDOW-TITLE\x1b\\'
    'hostile: a \x1b]8;;https://t3v-link.invalid/\x07hyperlink\x1b]8;;\x07 in a line\r\n'
    '\x1bPT3V-DCS\x1b\\'
    '\x1b_T3V-APC\x1b\\'
    '\x07'
    '\x9b0m'
    'hostile: done. Nothing above should reach your terminal, its title or your clipboard.\r\n')
HOSTILE_LABEL = ('\t\x1b[31mT3V-LABEL\x1b[0m \x07bel \x9b2J c1 \x1b]0;osc\x07 '
                 + 'L' * 300)
# `term activity --preset`'s labels. `idle` goes back to Terminal 1.
ACTIVITY_PRESETS = {
    'cargo': 'cargo test',
    'long': 'node ' + 'x' * 200,
    'hostile': HOSTILE_LABEL,
    # 127 units and an emoji, so T3's cut at 128 units splits the emoji's surrogate pair.
    'split': 'a' * 127 + '\U0001f600',
}
# Byte strings that must never reach raw.ansi: the controls t3term never writes, the text the
# hostile command hides inside them, and the text of events the drawer must skip.
LEAK_MARKERS = {
    'osc': b'\x1b]',
    'bel': b'\x07',
    'dcs': b'\x1bP',
    'apc': b'\x1b_',
    'c1Csi': b'\xc2\x9b',
    'clipboardText': CLIPBOARD.encode(),
    'clipboardBase64': base64.b64encode(CLIPBOARD.encode()),
    'windowTitle': b'T3V-WINDOW-TITLE',
    'linkTarget': b't3v-link.invalid',
    'dcsText': b'T3V-DCS',
    'apcText': b'T3V-APC',
    'strangerTerminal': b'STRANGER-TERM-2',
    'strangerThread': b'STRANGER-THREAD',
    'futureEvent': b'T3V-FUTURE',
    'oldEvent': b'OLD-EVENT',
}
# The markers above that are plain text, which a screen can show.
SCREEN_MARKERS = ('clipboardText', 'windowTitle', 'linkTarget', 'dcsText', 'apcText', 'strangerTerminal',
                  'strangerThread', 'futureEvent', 'oldEvent')
# Each `term duplicate` sends DUPLICATE-CHECK-N, with N its own number, and each must show at
# most once.
DUPLICATE = 'DUPLICATE-CHECK'
DUPLICATE_MARK = re.compile(re.escape(DUPLICATE) + r'-(\d+)(?!\d)')
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


# ---- text ----


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
    first = ' '.join(f'{b:02x}' for b in data[:limit])
    return first + (f' … ({len(data)} bytes)' if len(data) > limit else '')


def printable(c: str) -> bool:
    code = ord(c)
    return code >= 0x20 and code != 0x7f and not 0x80 <= code < 0xa0 and not 0xd800 <= code < 0xe000


# ---- requests ----


def named(value) -> bool:
    return isinstance(value, str) and bool(value.strip())


def whole(value, high: int) -> bool:
    return isinstance(value, int) and not isinstance(value, bool) and 1 <= value <= high


def schema_error(message: str) -> dict:
    """Not T3's shape: T3 fails a request its schema refuses with a parse error, which t3term
    reports the same way as any other refusal."""
    return {'_tag': 'FixtureSchemaError', 'message': message}


def lookup_error(tid: str, terminal: str) -> dict:
    # T3's terminal errors carry their fields and no message (terminal.ts).
    return {'_tag': 'TerminalSessionLookupError', 'threadId': tid, 'terminalId': terminal}


def expected_launch(thread: Thread) -> dict:
    """The launch t3term sends to start `thread`'s term-1, as Launch::for_thread builds it from
    what the desktop sends: the worktree or the workspace root as cwd, the worktree, and
    T3CODE_PROJECT_ROOT, with T3CODE_WORKTREE_PATH when there is a worktree."""
    root, worktree = thread.project['workspaceRoot'], thread.record['worktreePath']
    launch = {'cwd': worktree or root, 'env': {'T3CODE_PROJECT_ROOT': root}}
    if worktree:
        launch['worktreePath'] = worktree
        launch['env']['T3CODE_WORKTREE_PATH'] = worktree
    return launch


def launch_mismatches(thread: Thread, payload: dict) -> list:
    """The launch fields of an attach that differ from `expected_launch`, by name."""
    expected = expected_launch(thread)
    return [field for field in ('cwd', 'worktreePath', 'env') if payload.get(field) != expected.get(field)]


def describe_attach(payload: dict) -> dict:
    """What the log keeps of a terminal.attach: ids, the launch's paths and env names, and every
    field's name. Env values stay out."""
    kept = {k: safe(payload[k], 160) for k in ('threadId', 'terminalId', 'cwd', 'worktreePath') if k in payload}
    if isinstance(payload.get('env'), dict):
        kept['envKeys'] = sorted(safe(k, 60) for k in payload['env'])
    kept['keys'] = sorted(safe(k, 40) for k in payload)
    return kept


# ---- the world ----


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
        thread = Thread(tid, projects[project], title, at(-(3 + k) * DAY), at(-k * MINUTE))
        # expected_launch reads the project's workspace root from here.
        thread.project = projects[project]
        thread.set_fields({'branch': f't3/{thread.short}' if worktree else None, 'worktreePath': path}, 'both')
        thread.history(at, 1 + k % 2, -(k + 1) * HOUR, 20 * MINUTE)
        thread.settle_record()
        threads[tid] = thread
        order.append(tid)
    return {'projects': list(projects.values()), 'threads': threads, 'order': order}
