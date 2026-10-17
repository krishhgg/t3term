"""The terminal scenario's terminals: their history, their attach streams and the fake shell that
answers their input.

History and the output window follow T3 at f570bd2: BoundedTerminalHistory in
apps/server/src/terminal/Manager.ts and the window in apps/server/src/terminal/OutputProtocol.ts.
The header and last row follow header_row and the notices in t3term's src/tui/terminal.rs.
"""

from __future__ import annotations

import collections
import hashlib
import threading
import time

from ..text import cells, encode, iso, now_ms, safe
from .terminal_data import (DEFAULT_COLS, DEFAULT_ROWS, HISTORY_BYTES, HISTORY_LINES, HISTORY_PIECE, HOSTILE_OUTPUT,
                            LINE_LIMIT, PASTE_END, PASTE_START, QUERY, SIGNALS, UNFINISHED, WINDOW_BYTES,
                            WINDOW_CHUNKS, caret, hexes, printable, shown, utf8_len)


class History:
    """A terminal's history, as BoundedTerminalHistory keeps it (Manager.ts): at most
    HISTORY_LINES lines and HISTORY_BYTES bytes of UTF-8, less the queries T3 strips. Text after
    the last newline counts as a line, as in T3. Like T3, it adds text in pieces of at most
    HISTORY_PIECE characters and trims after each piece: whole lines from the front until the
    lines fit, then whole code points from the front until the bytes fit. A trim copies at most
    one piece, so a flood never copies the history. Like T3, it holds a CSI sequence that one
    add ended early until the next add finishes it, so a query split between two output events
    still stays out."""

    def __init__(self, text: str = ''):
        self.pieces = collections.deque()  # [text, its UTF-8 bytes, its newlines]
        self.bytes = self.newlines = 0
        self.pending = ''
        if text:
            self.add(text)

    @property
    def lines(self) -> int:
        if not self.pieces:
            return 0
        return self.newlines + (0 if self.pieces[-1][0].endswith('\n') else 1)

    def add(self, text: str):
        text = self.pending + text
        unfinished = UNFINISHED.search(text)
        self.pending = unfinished.group(0) if unfinished else ''
        text = QUERY.sub('', text[:len(text) - len(self.pending)])
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

    def __init__(self, fixture: 'TerminalFixture', tid: str, terminal: str, launch: dict, history: str = '',
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
        self.held = None  # what the shell printed during the write it is taking, or None between writes

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

    def __init__(self, fixture: 'TerminalFixture', connection, request_id: str, tid: str, terminal: str):
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
        """Not T3: sends `chunks` in one frame as one JSON array, past the window. They count
        toward maxUnacked, so `check` says the window broke."""
        encoded = [encode({'_tag': 'Chunk', 'requestId': self.request_id, 'values': values}) for values in chunks]
        if self.connection.send_frame(0x1, b'[' + b','.join(encoded) + b']'):
            self.window.extend(len(data) for data in encoded)
            self.sent += len(encoded)
            self.fixture.counts['window.forced'] += len(encoded)
            self.fixture.most('window.maxUnacked', len(self.window))


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


def decimal(word: str):
    """`word` as a whole number when it is ASCII digits alone, or None. isdigit() alone also takes
    a digit such as ², which int() refuses."""
    return int(word) if word.isascii() and word.isdigit() else None


def number(args: list, default: int, low: int, high: int):
    """The first word as a whole number from `low` to `high`, `default` without one, or None."""
    if not args:
        return default
    value = decimal(args[0])
    if value is None or not low <= value <= high:
        return None
    return value


COMMANDS = {name: f'do_{name}' for name in (
    'help', 'ls', 'colors', 'echo', 'env', 'seq', 'flood', 'clear', 'size', 'showkeys', 'cursorkeys',
    'fullscreen', 'dsr', 'sync', 'paste', 'hostile', 'sleep', 'dev', 'exit', 'kill')}


class FakeShell:
    """What answers term-1's input: a line editor and the commands in HELP, all invented. It runs
    no program and reads no file. All it prints comes from this module, and what it shows of its
    input is printable characters, caret notation and hex. Callers hold the fixture's lock."""

    def __init__(self, fixture: 'TerminalFixture', session: Session):
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
                'The t3v fixture invented this shell, and it runs nothing. Type help.\r\n')

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
        self.fixture.counts['fakeShell.keys'] += 1
        if self.mode == 'fullscreen':
            self.fullscreen_key(token)
        elif self.mode == 'dsr':
            if token == '\x03':
                self.dsr_end(self.epoch)
            else:
                self.replies += 1
                self.fixture.counts['fakeShell.dsrReplies'] += 1
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
            self.fixture.counts['fakeShell.otherKeys'] += 1
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
        self.fixture.counts['fakeShell.pastes'] += 1
        self.fixture.log('fakeShell.paste', threadId=self.session.tid, chars=paste['chars'], lf=paste['lf'],
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
        self.fixture.counts[f'fakeShell.{name if method else "unknown"}'] += 1
        self.fixture.log('fakeShell.command', threadId=self.session.tid, command=name if method else None,
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
        self.fixture.counts['fakeShell.showkeysWrites'] += 1
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
            self.fixture.counts['fakeShell.fullscreenArrows'] += 1
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
        value = decimal(name)
        if signum is None and value is not None and 0 < value < 65:
            signum = value
        if signum is None:
            self.done('kill: takes HUP, INT, QUIT, KILL, TERM or a number from 1 to 64\r\n')
            return
        self.out(f'kill: the shell dies of signal {signum}\r\n')
        self.fixture.exit_session(self.session, None, signum, 'kill')
