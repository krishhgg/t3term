"""The `terminal` scenario: threads whose term-1 is running, missing, exited, failed, busy or huge,
for t3term's terminal drawer, and T3's terminal RPCs behind the terminal scopes.

What the fake follows from T3 at f570bd2 (packages/contracts/src/terminal.ts,
apps/server/src/terminal/Manager.ts and OutputProtocol.ts, apps/server/src/auth/RpcAuthorization.ts):

- Scopes. A socket carries the scopes of the session whose ticket opened it. terminal.attach,
  write, resize, open, clear, restart and close need terminal:operate, terminal.observe and the
  two terminal subscriptions need terminal:read, and the orchestration and config RPCs need
  orchestration:read or orchestration:operate. A request without its scope fails with
  EnvironmentAuthorizationError and counts in scopeViolations.
- /api/auth/session answers {authenticated, scopes, permissions}. `t3v auth permissions` changes
  what `permissions` says for terminal sessions.
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
  and DA queries T3 strips never reach it (a simpler match than T3's), even split between two
  events, since it holds an unfinished CSI sequence until the next output as T3 does.
- terminal.write: a missing terminal fails with TerminalSessionLookupError, an exited one succeeds
  and does nothing, and any other that isn't running fails with TerminalNotRunningError. A write
  takes at most 65,536 UTF-16 units. terminal.resize does nothing for a missing terminal or one
  that isn't running. A write goes to the PTY the terminal has when the request comes, and a
  resize and a restart take the thread's lock in turn, so a resize ends before a restart begins.
- The label is "Terminal 1" while the shell is idle, and the running child's command while there
  is one, cut at 128 UTF-16 code units the way T3 cuts it.
- closed takes the event count plus one and removes the terminal, and attach streams stay open.
  restart clears the history and counts on, or counts from 0 when the terminal was gone. A
  started event reaches attach streams as a snapshot.
- The drawer's own limits (t3term's src/tui/terminal.rs): it holds at most 16 chunks and 56 MiB
  it hasn't drawn, and sends a paste of at most 256 KiB, brackets included, or nothing.

What isn't T3, and says so where it appears: the stranger, duplicate, old, malformed,
ignore-window and oversize controls, a project with a blank workspace root, `--starting` (T3
shows starting only while the PTY spawns), and the fake shell.

The world. Project proj-term, Terminal Desk, at /fixture/terminal-desk, and proj-rootless,
Rootless Desk, whose workspaceRoot is blank (not T3). A worktree is at
/fixture/terminal-desk/.t3/worktrees/NAME, with the branch t3/NAME.

  thr-running   Running: a shell with history     term-1 running, colored history
  thr-fresh     Fresh: no terminal yet            worktree fresh, no term-1
  thr-local     Local: no worktree                no worktree, no term-1
  thr-exited    Exited: the shell exited with 1   term-1 exited with code 1
  thr-failed    Failed: the shell never started   term-1 in status error, no history
  thr-busy      Busy: npm run dev                 term-1 running a dev server until Ctrl+C
  thr-huge      Huge: 8 MiB of history            4,990 lines, about 27 MiB as JSON
  thr-rootless  Rootless: no workspace root       in proj-rootless, no term-1

`t3v start --scenario terminal` returns once the sidebar shows "Running: a shell". The fake
shell's pids start at 41001, a new shell is 120x30, and the seeded terminals have counted to
sequence 120.

The fake shell. Commands: help, ls, colors, echo, env, seq N, flood MIB, clear, size, showkeys,
cursorkeys, fullscreen [--mouse], dsr, sync [--hang], paste on|off, hostile, sleep N, dev,
exit [N] and kill [SIGNAL]. At the prompt, a key it has no use for (Tab, an arrow, Esc, a
function key, Ctrl+X) prints a line such as `[key ^I  09]`, so you can see what reached it.
Ctrl+C prints ^C and Ctrl+D on an empty line exits. Its prompt turns bracketed paste on, as zsh's
does, and a bracketed paste prints `[paste N characters, LF n, CR n, sha256 XXXXXXXXXXXX]`. What
it prints while it takes one write goes out when the write ends, as one read of a PTY would
gather it. The log keeps a write's length and a hash, never what was typed.

Checks. On top of the shared rules, `t3v check` fails a run with an attach field the drawer
never sends (attachViolations), a launch that differs from the thread's (launchMismatches), a
terminal RPC the drawer never sends (unexpectedTerminalRpcs), more than 8 chunks unacked
(maxUnacked), an Ack for no chunk (excessAcks) or for no attach stream on the terminal socket
(acksOnOtherStreams.terminal), a write before the last one's answer (writesOverlapped), a write
over 65,536 UTF-16 units (largestWrite), a terminal request T3's schema refuses (badInput in
attaches, terminalWrites or terminalResizes), one `term duplicate` event on screen more than once
(duplicateOnScreen), and, after stop, a terminal socket still open or a terminal session not
revoked.

Cases worth driving. `t3v term list` shows each terminal and the header the drawer should show
for it, and a failed shell's last row. `--thread` defaults to thr-running.

 1. Nothing before the drawer. Open Running: terminalSessionsIssued stays 0 and no terminal RPC
    goes out.
 2. Show it with F4. t3term issues one session with terminal:read and terminal:operate, checks
    it at /api/auth/session, opens a second socket and sends one terminal.attach with threadId
    and terminalId alone. Once the snapshot comes, one terminal.resize brings the size the
    drawer drew. The header reads ` Terminal · Terminal 1 · running` with `F4 hide` at its right.
 3. Type. `t3v type "echo hello"` prints hello. `t3v keys Tab BTab Escape C-c C-x`: each reaches
    the shell. F6 moves the keyboard off the drawer.
 4. Hide and show. F4 with the keyboard on the drawer interrupts the attach once, and the shell
    keeps running. F4 again attaches by identity and gets a new snapshot.
 5. Start. On Fresh, F4 shows "This thread has no terminal yet." and Enter starts one: an
    attach with the worktree as cwd, its worktreePath, T3CODE_PROJECT_ROOT and
    T3CODE_WORKTREE_PATH. On Local, cwd is the workspace root with T3CODE_PROJECT_ROOT alone.
    On Rootless, Enter sends no launch.
 6. A start T3 refuses: `term close --thread thr-fresh --delete-history`, `term missing-cwd on`,
    then F4 twice and Enter on Fresh: "The shell's directory doesn't exist.".
 7. Read-only. Exited reads `exited with 1, read-only` and takes no write. Failed reads `failed,
    read-only` over an empty screen, since its snapshot carries no message.
 8. Busy reads ` Terminal · npm run dev · running`. C-c prints ^C and the label goes back.
 9. Live changes: `term activity --label "cargo test"`, `--preset long`, `--preset hostile`
    (cleaned and cut to 64 columns), `--preset idle`; `term clear`; `term exit --code 3`; `term
    restart`; `term error --message "pty broke"` (the last row reads ` pty broke`); `term close`
    (closed, read-only, the stream stays open); `term start`.
10. Events to skip: `term stranger`, `term old` and `term duplicate`, which sends
    DUPLICATE-CHECK-N twice. duplicateOnScreen is at most 1 and screenLeaks stays 0. `term
    activity --preset split` cuts an emoji in half, which JSON writes as a lone \\ud83d escape
    that a strict JSON reader refuses.
11. Malformed: `term malformed` stops the drawer, naming the event and field. Enter attaches
    again.
12. Limits: `term ignore-window --chunks 20` (which also breaks maxUnacked on purpose) and `term
    oversize` each stop the drawer, and no attach follows until Enter.
13. The window: `t3v type "flood 16"` keeps maxUnacked at most 8 and ends at `flood: done`.
14. Huge: F4 on Huge shows the last lines, and S-PPage scrolls with `↑ N rows` at the right.
15. Paste: `t3v paste --text "hello paste"` prints the [paste] line with the sha the helper
    printed. `t3v paste --bytes 262132` goes whole, `--bytes 262133` is refused whole.
16. Replies: `t3v type dsr` shows two replies. A shell started with `term start --starting 8`
    shows `starting` and refuses keys until it runs.
17. Order: `term slow 800`, then `t3v keys a b c d e f --gap 0`: writesOverlapped stays 0. A
    write or resize that `term slow` holds over `term restart` belongs to the shell it came for.
    The write reaches no shell (terminalWrites.toEndedShell), and the resize keeps its size with
    no redraw in the new shell (terminalResizes.heldOverRestart), as T3's order would have it.
18. Input failure: `term fail-writes on`, then a key: `input stopped` and a notice.
19. Resize: `t3v resize 120 36` sends one terminal.resize with the size drawn; `t3v type size`
    says what the shell got. `term fail-resize on` and a resize: a notice.
20. Full screen: `t3v type fullscreen`, arrows move the @, q quits.
21. Hostile: `t3v mark before-hostile`, `t3v type hostile`: controlLeaksSinceMark and screenLeaks
    stay 0.
22. Sync: `t3v type sync` and `t3v type "sync --hang"`.
23. Reconnect: `t3v drop --hold 3`: `reconnecting, try N of 8`, then running with a new snapshot
    and no key sent again. `t3v drop --hold 60` stops the drawer after 8 tries.
24. Permissions: `t3v auth permissions extra`, `t3v restart`, open Running and F4: the drawer
    stops and names the permissions. `t3v auth permissions normal` after.
25. Slow attach: `term hold-attach 5`, then F4: `attaching`, and a key says the terminal is still
    attaching.
26. Sizes: `t3v resize 80 24`, `40 12` and `16 8`: a narrow header drops `F4 hide`, then cuts
    the label or leaves it out, then gives the state its short form.
27. Quit: `t3v stop` sends C-q, and t3term revokes both sessions.
"""

from __future__ import annotations

import itertools
from collections import Counter

from ..auth import PERMISSION_MODES
from ..orchestration import Orchestration
from ..runs import locate, post
from ..server import DESCRIBERS
from ..text import iso, now_ms, safe, sha, unescape
from .terminal_data import (ACTIVITY_PRESETS, ATTACH_NEVER, DUPLICATE, EVENTS_PER_CHUNK, FIRST_PID, LABEL_LIMIT,
                            LEAK_MARKERS, OUTPUT_PIECE, SCREEN_MARKERS, SEEDED_SEQUENCE, TERMINAL_ID,
                            UNEXPECTED_TERMINAL, WRITE_LIMIT, build_world, describe_attach, expected_launch,
                            launch_mismatches, lookup_error, named, schema_error, shown, utf8_len, utf16_cut,
                            utf16_len, whole)
from .terminal_expect import print_expect, print_terminal, print_terminals, run_fields, status_fields
from .terminal_shell import DEV, LS, AttachStream, FakeShell, History, Session, colors

NAME = 'terminal'
# A thread in the sidebar: `t3v start` returns once it shows. The sidebar may cut the rest.
READY = 'Running: a shell'
# What `t3v check` looks for on top of rules.py's markers.
LEAKS, SCREEN = LEAK_MARKERS, SCREEN_MARKERS
# The `term` actions that change a setting rather than a terminal.
SETTINGS = ('hold-attach', 'slow', 'fail-writes', 'fail-resize', 'missing-cwd')

# What the log keeps of each terminal request: never what was typed or an env value.
DESCRIBERS.update({
    'terminal.attach': describe_attach,
    'terminal.write': lambda payload: {'threadId': safe(payload.get('threadId'), 60),
                                       'terminalId': safe(payload.get('terminalId'), 40)},
    'terminal.resize': lambda payload: {k: payload.get(k) for k in ('cols', 'rows') if isinstance(payload.get(k), int)},
})

__all__ = ['NAME', 'READY', 'LEAKS', 'SCREEN', 'TerminalFixture', 'add_start_options', 'options_of', 'add_commands',
           'print_expect', 'status_fields', 'run_fields']


class TerminalFixture(Orchestration):
    """The terminal world on the orchestration feeds, with T3's terminals and their attach
    streams. Every change holds `lock`."""

    def __init__(self, root, run):
        super().__init__(root, run)
        self.terminals = {}  # (thread id, terminal id) -> Session
        self.persisted = {}  # (thread id, terminal id) -> the history a close kept
        self.closed_labels = {}  # terminal a close removed -> its last label, until something starts it again
        self.attach_streams = []  # open AttachStreams
        self.held_attaches = []  # (connection, request id, payload) of attaches `hold-attach` holds
        self.busy_writes = Counter()  # terminal -> writes T3 hasn't answered yet
        self.busy_resizes = Counter()
        self.hold_attach_until = 0
        self.slow_ms = 0
        self.fail_writes = self.fail_resize = self.missing_cwd = False
        self.pids = itertools.count(FIRST_PID)
        self.rpc.update({
            'terminal.attach': self.attach,
            'terminal.write': self.write,
            'terminal.resize': self.resize,
        })
        for tag in UNEXPECTED_TERMINAL:
            self.rpc[tag] = lambda connection, request_id, _, tag=tag: self.unexpected(connection, request_id, tag)
        self.controls.update({
            '/__fixture/term/': lambda request, action: self.term_action(action, request),
            '/__fixture/drop': lambda request, _: self.drop_control(request),
        })

    # -- world --

    def build_world(self, t0: int) -> dict:
        return build_world(t0)

    def world_built(self, world: dict):
        super().world_built(world)
        self.seed()
        self.log('term.seeded', terminals=sorted(tid for tid, _ in self.terminals))

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

    # -- auth and sockets --

    def set_permissions(self, request: dict):
        answer, refusal = super().set_permissions(request)
        if answer:
            answer['note'] += ('. t3term checks them each time it connects the terminal; `t3v restart` gives it a '
                               'session to check.')
        return answer, refusal

    def drop_control(self, request: dict):
        """Closes the terminal sockets, or every socket, and has /ws answer 503 to the same for a
        while."""
        try:
            hold = float(request.get('hold') or 3)
        except (TypeError, ValueError):
            hold = -1
        if not 0 < hold <= 120:
            return None, 'drop takes a hold of 0 to 120 seconds'
        everything = request.get('everything') is True
        with self.lock:
            self.ensure_world()
            self.counts['drops'] += 1
            self.log('drop', holdSeconds=hold, everything=everything,
                     sockets=sum(1 for c in self.connections if everything or c.kind == 'terminal'))
        closed = self.drop_sockets(hold, None if everything else ('terminal',))
        whose = 'every session' if everything else 'a terminal session'
        return {'note': f'closed {closed} socket(s); /ws answers 503 to {whose} for {hold:g} s'}, None

    def report(self) -> dict:
        with self.lock:
            return dict(super().report(), attachStreams=len(self.attach_streams),
                        heldAttaches=len(self.held_attaches), permissions=self.permissions,
                        socketsByKind=dict(Counter(c.kind for c in self.connections)))

    # -- T3's terminals --

    def current(self, session: Session) -> bool:
        return self.terminals.get(session.key) is session

    def broadcast(self, key: tuple, values: list):
        for stream in list(self.attach_streams):
            if stream.key == key and stream.open:
                stream.push(values)

    def backlog(self, session: Session) -> int:
        return max((len(s.waiting) for s in self.attach_streams if s.key == session.key and s.open), default=0)

    def output(self, session: Session, text: str):
        """Output from the shell, as T3 reads it from the PTY: events of at most OUTPUT_PIECE
        characters, each with its sequence, into the history, and to the attach streams at most
        EVENTS_PER_CHUNK events to a chunk. What the shell prints while it takes a write waits
        until the write ends, as one read of a PTY gathers it, so a paste's echo is a few events
        and not one a character."""
        if not self.current(session) or not text:
            return
        if session.held is not None:
            session.held.append(text)
            return
        events = []
        for start in range(0, len(text), OUTPUT_PIECE):
            piece = text[start:start + OUTPUT_PIECE]
            session.history.add(piece)
            events.append(session.event('output', data=piece))
        for start in range(0, len(events), EVENTS_PER_CHUNK):
            self.broadcast(session.key, events[start:start + EVENTS_PER_CHUNK])
        self.counts['term.outputEvents'] += len(events)

    def send_held(self, session: Session):
        """Sends what the shell printed during this write so far, before an event that follows it."""
        held = session.held
        if held:
            session.held = None
            self.output(session, ''.join(held))
            session.held = []

    def activity(self, session: Session, label: str, busy: bool):
        """An activity event, with the label cut at 128 UTF-16 code units as T3 cuts it."""
        if not self.current(session):
            return
        self.send_held(session)
        session.label, session.busy = utf16_cut(label, LABEL_LIMIT), busy
        self.broadcast(session.key, [session.event('activity', hasRunningSubprocess=busy, label=session.label)])

    def exit_session(self, session: Session, code, signum, why: str):
        if not self.current(session) or session.status in ('exited', 'error'):
            return
        self.send_held(session)
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
        self.closed_labels.pop(key, None)
        self.counts['term.created'] += 1
        self.log('term.created', threadId=tid, terminalId=terminal, starting=starting or None)
        if starting > 0:
            session.status, session.pid = 'starting', None
            self.broadcast(key, [{'type': 'snapshot', 'snapshot': session.snapshot()}])
            self.later(starting, lambda: self.started(session, True))
        else:
            self.started(session, False)
        return session

    def started(self, session: Session, waited: bool):
        """The started snapshot. `waited` is the `--starting` timer, which also starts the shell,
        and does nothing once a restart, an exit or a close has moved the terminal on."""
        with self.lock:
            ready = ('starting',) if waited else ('starting', 'running')
            if not self.current(session) or session.status not in ready:
                return
            session.status = 'running'
            if session.pid is None:
                session.pid = next(self.pids)
            session.sequence += 1
            session.updated = iso(now_ms())
            self.broadcast(session.key, [{'type': 'snapshot', 'snapshot': session.snapshot()}])
            if waited:
                session.shell.start()

    # -- the terminal RPCs --

    def find(self, connection, request_id: str):
        return next((s for s in self.attach_streams if s.connection is connection and s.request_id == request_id),
                    None)

    def unexpected(self, connection, request_id: str, tag: str):
        with self.lock:
            self.counts[f'terminal.unexpected.{tag}'] += 1
        connection.fail(request_id, tag, {'_tag': 'FixtureUnexpected',
                                          'message': f'The drawer never sends {tag}, so the t3v fixture refuses it.'})

    def attach(self, connection, request_id: str, payload: dict):
        tid, terminal = payload.get('threadId'), payload.get('terminalId')
        launch = 'cwd' in payload
        violations = [name for name in ATTACH_NEVER if name in payload]
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
                self.held_attaches.append((connection, request_id, payload))
                self.counts['attach.held'] += 1
                self.later(wait, lambda: self.release(connection, request_id))
                return
            self.answer_attach(connection, request_id, payload)

    def release(self, connection, request_id: str):
        with self.lock:
            entry = next((h for h in self.held_attaches if h[0] is connection and h[1] == request_id), None)
            if entry is None:
                # Interrupted, or its socket closed, during the hold.
                return
            wait = (self.hold_attach_until - now_ms()) / 1000
            if wait > 0:
                self.later(wait, lambda: self.release(connection, request_id))
                return
            self.held_attaches.remove(entry)
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
                connection.fail(request_id, 'terminal.attach', refusal)
                return
            wrong = launch_mismatches(self.threads[tid], payload)
            for field in wrong:
                self.counts[f'attach.launchMismatch.{field}'] += 1
            env = payload.get('env') if isinstance(payload.get('env'), dict) else {}
            worktree = payload.get('worktreePath')
            self.log('term.launch', threadId=tid, cwd=safe(payload['cwd'], 160),
                     worktreePath=safe(worktree, 160) if worktree is not None else None,
                     envKeys=sorted(safe(k, 60) for k in env), mismatches=wrong or None)
            session = self.create(tid, terminal, {'cwd': payload['cwd'],
                                                  'worktreePath': worktree if named(worktree) else None,
                                                  'env': env})
            fresh = True
        elif 'cwd' in payload:
            self.counts['attach.launchIgnored'] += 1
        stream = AttachStream(self, connection, request_id, tid, terminal)
        self.attach_streams.append(stream)
        stream.push([{'type': 'snapshot', 'snapshot': session.snapshot()}])
        self.counts['attach.answered'] += 1
        if fresh:
            session.shell.start()

    def on_ack(self, connection, request_id: str):
        with self.lock:
            stream = self.find(connection, request_id)
            if stream is None:
                # t3term acks each chunk of the login socket's other streams on receipt.
                self.counts[f'rpc.ackOther.{connection.kind}'] += 1
            else:
                stream.ack()

    def on_interrupt(self, connection, request_id: str, tag):
        if tag == 'terminal.attach':
            with self.lock:
                self.counts['terminal.interrupt'] += 1
        super().on_interrupt(connection, request_id, tag)

    def drop_streams(self, connection, request_id=None):
        """Forgets a connection's attach streams and held attaches after its socket closed, or
        one of them after an interrupt, then the orchestration streams."""
        def gone(c, r) -> bool:
            return c is connection and (request_id is None or r == request_id)

        with self.lock:
            streams = [s for s in self.attach_streams if gone(s.connection, s.request_id)]
            for stream in streams:
                stream.open = False
                self.attach_streams.remove(stream)
            held = [h for h in self.held_attaches if gone(h[0], h[1])]
            for entry in held:
                self.held_attaches.remove(entry)
            if streams:
                self.counts['attach.socketClosed' if request_id is None else 'attach.interrupted'] += len(streams)
            if held and request_id is not None:
                self.counts['attach.interruptedWhileHeld'] += len(held)
            super().drop_streams(connection, request_id)

    def write(self, connection, request_id: str, payload: dict):
        tid, terminal, data = payload.get('threadId'), payload.get('terminalId'), payload.get('data')
        with self.lock:
            self.ensure_world()
            self.counts['write.requests'] += 1
            if not (named(tid) and named(terminal)) or not isinstance(data, str) or not data \
                    or utf16_len(data) > WRITE_LIMIT:
                self.counts['write.badInput'] += 1
                if isinstance(data, str):
                    self.most('write.largest', utf16_len(data))
                connection.fail(request_id, 'terminal.write', schema_error(
                    f'data must be a non-empty string of at most {WRITE_LIMIT} UTF-16 code units'))
                return
            key = (tid, terminal)
            units = utf16_len(data)
            self.counts['write.units'] += units
            self.most('write.largest', units)
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
                shell = session.shell
                self.later(self.slow_ms / 1000,
                           lambda: self.finish_write(connection, request_id, session, shell, data, True))
            else:
                self.finish_write(connection, request_id, session, session.shell, data, False)

    def finish_write(self, connection, request_id: str, session: Session, shell: FakeShell, data: str,
                     slow: bool):
        """Feeds a write to `shell`, the one it came for. T3 writes to the PTY it has when the request
        comes, so a write `term slow` held while a restart, a close or an exit ended that shell went
        to a shell nothing reads."""
        with self.lock:
            if slow:
                self.busy_writes[session.key] -= 1
            if self.current(session) and session.shell is shell and session.status == 'running':
                session.held = []
                try:
                    shell.feed(data)
                finally:
                    self.send_held(session)
                    session.held = None
                self.counts['write.fed'] += 1
            else:
                self.counts['write.toEndedShell'] += 1
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
                shell = session.shell
                self.later(self.slow_ms / 1000,
                           lambda: self.finish_resize(connection, request_id, session, shell, cols, rows, True))
            else:
                self.finish_resize(connection, request_id, session, session.shell, cols, rows, False)

    def finish_resize(self, connection, request_id: str, session: Session, shell: FakeShell, cols: int, rows: int,
                      slow: bool):
        """Applies a resize. T3 ends a resize before it begins a restart, which then starts the new
        shell at that size, so a resize `term slow` held over a restart keeps its size, and only the
        shell it came for redraws."""
        with self.lock:
            if slow:
                self.busy_resizes[session.key] -= 1
            if self.current(session) and session.status == 'running':
                session.cols, session.rows = cols, rows
                session.sizes.append(f'{cols}x{rows}')
                self.counts['resize.applied'] += 1
                if session.shell is shell:
                    shell.resized()
                else:
                    self.counts['resize.heldOverRestart'] += 1
            connection.exit_success(request_id, None)

    # -- what t3term should show --

    def listing(self) -> list:
        """Each thread's term-1, with the header the drawer should show for it."""
        with self.lock:
            self.ensure_world()
            rows = []
            for tid in self.order:
                key = (tid, TERMINAL_ID)
                session = self.terminals.get(key)
                streams = [s for s in self.attach_streams if s.key == key and s.open]
                row = {'threadId': tid, 'title': self.threads[tid].record['title'], 'streams': len(streams),
                       'waiting': sum(len(s.waiting) for s in streams),
                       'unacked': sum(len(s.window) for s in streams),
                       'expectedLaunch': expected_launch(self.threads[tid])}
                if session is None:
                    closed = key in self.closed_labels
                    label = f' · {shown(self.closed_labels[key], 64)}' if closed else ''
                    row.update(status='closed' if closed else 'missing',
                               keptHistory=utf8_len(self.persisted.get(key, '')),
                               header=(f' Terminal{label} · closed, read-only' if closed and streams
                                       else ' Terminal · not started'))
                else:
                    row.update(status=session.status, label=safe(session.label, 80),
                               labelUnits=utf16_len(session.label), busy=session.busy, sequence=session.sequence,
                               pid=session.pid, cwd=safe(session.cwd, 120), worktreePath=session.worktree,
                               historyBytes=session.history.bytes, historyLines=session.history.lines,
                               size=f'{session.cols}x{session.rows}', sizes=list(session.sizes),
                               shell=session.shell.mode, header=session.header(), lastRow=session.last_row())
                rows.append(row)
            return rows

    def settings(self) -> dict:
        return {'holdAttachSecondsLeft': max(0, round((self.hold_attach_until - now_ms()) / 1000, 1)),
                'slowMs': self.slow_ms, 'failWrites': self.fail_writes, 'failResize': self.fail_resize,
                'missingCwd': self.missing_cwd}

    def expect(self) -> dict:
        with self.lock:
            return dict(super().expect(), terminals=self.listing(), permissions=self.permissions,
                        settings=self.settings())

    # -- `term` controls --

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
            attribute = {'fail-writes': 'fail_writes', 'fail-resize': 'fail_resize', 'missing-cwd': 'missing_cwd'}
            setattr(self, attribute[action], value == 'on')
            note = {'fail-writes': 'each terminal.write fails with TerminalWriteError',
                    'fail-resize': 'each terminal.resize fails with TerminalResizeError',
                    'missing-cwd': 'an attach that starts a terminal fails with TerminalCwdNotFoundError'}[action]
            note = note if value == 'on' else f'{action} is off'
        self.log('term.setting', action=action, value=value)
        return {'note': note}, None

    def term_action(self, action: str, request: dict):
        """One `term` control: (answer, None), or (None, why not)."""
        with self.lock:
            self.ensure_world()
            if action == 'list':
                return {'terminals': self.listing()}, None
            if action in SETTINGS:
                return self.setting(action, request.get('value'))
            tid = request.get('threadId')
            thread = self.threads.get(tid) if isinstance(tid, str) else None
            if thread is None:
                return None, f'no fixture thread {safe(tid, 60)}; `t3v term list` lists them'
            method = {'exit': self.term_exit, 'error': self.term_error, 'close': self.term_close,
                      'clear': self.term_clear, 'activity': self.term_activity, 'output': self.term_output,
                      'malformed': self.term_malformed, 'stranger': self.term_stranger,
                      'duplicate': self.term_duplicate, 'old': self.term_old,
                      'ignore-window': self.term_ignore_window, 'oversize': self.term_oversize}.get(action)
            if method is None and action not in ('start', 'restart'):
                return None, f'no term action {safe(action, 40)}'
            key = (tid, TERMINAL_ID)
            session = self.terminals.get(key)
            self.counts[f'term.control.{action}'] += 1
            self.log('term.control', action=action, threadId=tid)
            if action == 'start':
                done, refusal = self.term_start(thread, session, request)
            elif action == 'restart':
                done, refusal = self.term_restart(thread, session)
            elif session is None:
                return None, f'{tid} has no term-1 now; `t3v term start --thread {tid}` starts one'
            else:
                done, refusal = method(session, request)
            return (self.answer(done), None) if refusal is None else (None, refusal)

    def term_start(self, thread, session, request: dict):
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
        return (f'started term-1 of {thread.tid} as the desktop would{how}: counting from 0, with '
                f'{session.history.bytes} bytes of kept history'), None

    def term_restart(self, thread, session):
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
            self.closed_labels.pop(key, None)
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
        return f'restarted term-1 of {thread.tid} at sequence {session.sequence}', None

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
        return f'term-1 of {session.tid}: {session.header().strip()}', None

    def term_error(self, session: Session, request: dict):
        message = str(request.get('message') or 'The fixture failed this shell on purpose.')[:300]
        if not message.strip():
            return None, 'an error event needs a message'
        session.shell.stop()
        session.status, session.pid, session.busy, session.message = 'error', None, False, message
        self.broadcast(session.key, [session.event('error', message=message)])
        return f'term-1 of {session.tid} failed: "{safe(message, 120)}"', None

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
        self.closed_labels[key] = session.label
        kept = utf8_len(self.persisted.get(key, ''))
        return f'closed term-1 of {session.tid}; {kept} bytes of history kept for its next start', None

    def term_clear(self, session: Session, request: dict):
        session.history = History()
        self.broadcast(session.key, [session.event('cleared')])
        return f'cleared term-1 of {session.tid}', None

    def term_activity(self, session: Session, request: dict):
        preset, label = request.get('preset'), request.get('label')
        if preset == 'idle':
            self.activity(session, 'Terminal 1', False)
        elif preset in ACTIVITY_PRESETS:
            self.activity(session, ACTIVITY_PRESETS[preset], True)
        elif isinstance(label, str) and label:
            self.activity(session, label, True)
        else:
            return None, f'activity takes --label TEXT or --preset {"|".join(sorted(ACTIVITY_PRESETS))}|idle'
        split = any(0xd800 <= ord(c) < 0xe000 for c in session.label)
        return (f'term-1 of {session.tid}: label "{safe(session.label, 120)}", {utf16_len(session.label)} UTF-16 '
                f'units{", ending in a lone surrogate as T3 cuts it" if split else ""}'), None

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
        return f'{len(text)} characters of output on term-1 of {session.tid}', None

    def redraw(self, session: Session):
        if session.shell.mode == 'prompt':
            self.output(session, session.shell.prompt() + session.shell.line)

    def term_malformed(self, session: Session, request: dict):
        # Not T3: an output event whose data isn't a string.
        self.broadcast(session.key, [session.event('output', data=42)])
        return f'sent term-1 of {session.tid} an output event with data 42', None

    def term_stranger(self, session: Session, request: dict):
        # Not T3: events for another terminal and another thread, and a type T3 doesn't have.
        self.broadcast(session.key, [
            {'type': 'output', 'threadId': session.tid, 'terminalId': 'term-2', 'sequence': session.sequence + 1,
             'data': '\r\nSTRANGER-TERM-2\r\n'},
            {'type': 'output', 'threadId': 'thr-somewhere-else', 'terminalId': session.terminal,
             'sequence': 10 ** 6, 'data': '\r\nSTRANGER-THREAD\r\n'},
            {'type': 'future', 'threadId': session.tid, 'terminalId': session.terminal, 'data': '\r\nT3V-FUTURE\r\n'},
        ])
        return 'sent three events the drawer must skip; none of their text should show', None

    def term_duplicate(self, session: Session, request: dict):
        # Not T3: the same event twice. term_action counted this control already, so each probe
        # has its own number and an earlier one can't hide a copy of this one.
        mark = f'{DUPLICATE}-{self.counts["term.control.duplicate"]}'
        event = session.event('output', data=f'\r\n{mark}\r\n')
        session.history.add(event['data'])
        self.broadcast(session.key, [event])
        self.broadcast(session.key, [event])
        self.redraw(session)
        return f'sent {mark} twice at sequence {event["sequence"]}; it should show once', None

    def term_old(self, session: Session, request: dict):
        # Not T3: an event from before the last one.
        sequence = max(0, session.sequence - 3)
        self.broadcast(session.key, [{'type': 'output', 'threadId': session.tid, 'terminalId': session.terminal,
                                      'sequence': sequence, 'data': '\r\nOLD-EVENT\r\n'}])
        return f'sent OLD-EVENT at sequence {sequence}, after {session.sequence}', None

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
        streams = [s for s in self.attach_streams if s.key == session.key and s.open]
        for stream in streams:
            stream.force(chunks)
        return f'sent {count} chunks in one frame past the window to {len(streams)} stream(s)', None

    def term_oversize(self, session: Session, request: dict):
        # Not T3: one output event of about 57.4 MiB as JSON, past the drawer's 56 MiB. It stays out
        # of the history.
        event = session.event('output', data='\x1b7\x1b8' * 4_300_000)
        streams = [s for s in self.attach_streams if s.key == session.key and s.open]
        for stream in streams:
            stream.push([event])
        return f'sent one output chunk of about 57.4 MiB to {len(streams)} stream(s)', None


# ---- the command line ----


def add_start_options(start):
    """The terminal scenario has no `t3v start` options of its own."""


def options_of(args) -> dict:
    return {}


def term_post(args, action: str, body: dict) -> dict:
    root, _ = locate(args.run)
    return post(root, f'/__fixture/term/{action}', body)


def cmd_term(args) -> int:
    action = args.action
    if action == 'list':
        print_terminals(term_post(args, 'list', {}).get('terminals') or [])
        return 0
    if action in SETTINGS:
        print(term_post(args, action, {'value': args.value}).get('note'))
        return 0
    request = {'threadId': args.thread}
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
    answer = term_post(args, action, request)
    print(answer.get('note'))
    rows = (answer.get('expect') or {}).get('terminals') or []
    row = next((r for r in rows if r['threadId'] == args.thread), None)
    if row:
        print_terminal(row)
    return 0


def cmd_auth(args) -> int:
    root, _ = locate(args.run)
    print(post(root, '/__fixture/auth/permissions', {'mode': args.mode}).get('note'))
    return 0


def cmd_drop(args) -> int:
    root, _ = locate(args.run)
    print(post(root, '/__fixture/drop', {'hold': args.hold, 'everything': args.everything}).get('note'))
    return 0


def add_commands(commands, run_option):
    """The terminal scenario's commands. `run_option` adds --run to a parser."""
    term = commands.add_parser('term', help="terminal: change a thread's term-1 as T3 or the desktop would")
    actions = term.add_subparsers(dest='action', required=True, metavar='ACTION')
    with_thread, rest = [], [actions.add_parser('list', help='each terminal and the header the drawer should show')]
    start = actions.add_parser('start', help='start term-1 as the desktop would, counting from 0')
    start.add_argument('--starting', type=float, default=0.0, metavar='S',
                       help='show starting for S seconds first (not T3; at most 60)')
    with_thread.append(start)
    for name, what in (('restart', 'restart term-1: its history goes and attach streams get restarted'),
                       ('clear', 'a cleared event'),
                       ('malformed', 'an output event whose data is 42 (not T3)'),
                       ('stranger', 'events for term-2, another thread and a future type (not T3)'),
                       ('duplicate', f'{DUPLICATE}-N twice with one sequence (not T3)'),
                       ('old', 'an event from before the last one (not T3)'),
                       ('oversize', 'one output chunk of about 57.4 MiB (not T3)')):
        with_thread.append(actions.add_parser(name, help=what))
    term_exit = actions.add_parser('exit', help='the shell exits with --code (default 0) or --signal')
    how = term_exit.add_mutually_exclusive_group()
    how.add_argument('--code', type=int, help='-255 to 255')
    how.add_argument('--signal', type=int, help='1 to 64')
    with_thread.append(term_exit)
    term_error = actions.add_parser('error', help='an error event: the shell failed')
    term_error.add_argument('--message', default='The fixture failed this shell on purpose.')
    with_thread.append(term_error)
    term_close = actions.add_parser('close', help='T3 closes term-1; attach streams stay open')
    term_close.add_argument('--delete-history', action='store_true', help='keep no history for the next start')
    with_thread.append(term_close)
    term_activity = actions.add_parser('activity', help='an activity event with a label, cut as T3 cuts it')
    label = term_activity.add_mutually_exclusive_group(required=True)
    label.add_argument('--label')
    label.add_argument('--preset', choices=(*ACTIVITY_PRESETS, 'idle'))
    with_thread.append(term_activity)
    term_output = actions.add_parser('output', help='output on term-1, as if the shell wrote it')
    term_output.add_argument('text')
    term_output.add_argument('--escapes', action='store_true',
                             help='turn \\xHH, \\uHHHH, \\e, \\a, \\r, \\n, \\t and \\\\ into what they name')
    term_output.add_argument('--no-sequence', action='store_true', help='leave the sequence out')
    with_thread.append(term_output)
    window = actions.add_parser('ignore-window', help='chunks past the window in one frame (not T3)')
    window.add_argument('--chunks', type=int, default=20, help='1 to 64 (default 20)')
    with_thread.append(window)
    hold = actions.add_parser('hold-attach', help='answer each terminal.attach after S seconds; 0 ends it')
    hold.add_argument('value', type=float, metavar='SECONDS')
    slow = actions.add_parser('slow', help='feed each write and apply each resize MS milliseconds late')
    slow.add_argument('value', type=int, metavar='MS')
    rest += [hold, slow]
    for name, what in (('fail-writes', 'each terminal.write fails with TerminalWriteError'),
                       ('fail-resize', 'each terminal.resize fails with TerminalResizeError'),
                       ('missing-cwd', 'a start fails with TerminalCwdNotFoundError')):
        toggle = actions.add_parser(name, help=what)
        toggle.add_argument('value', choices=('on', 'off'))
        rest.append(toggle)
    for parser in with_thread:
        parser.add_argument('--thread', default='thr-running', help='the thread (default thr-running)')
    # On the parsers that run, not on `term` itself, so a nested parser's default can't
    # overwrite a --run given before its action.
    for parser in with_thread + rest:
        parser.set_defaults(func=cmd_term)
        run_option(parser)

    auth = commands.add_parser('auth', help='terminal: what /api/auth/session says of terminal sessions')
    auth.add_argument('what', choices=('permissions',))
    auth.add_argument('mode', choices=PERMISSION_MODES,
                      help="normal: the session's scopes; extra: one more; missing: without terminal:operate; "
                           'absent: no permissions field')
    auth.set_defaults(func=cmd_auth)
    drop = commands.add_parser('drop', help='terminal: close the terminal sockets and answer /ws with 503 for a while')
    drop.add_argument('--hold', type=float, default=3.0, help='seconds (default 3, at most 120)')
    drop.add_argument('--everything', action='store_true', help="the TUI's own socket too")
    drop.set_defaults(func=cmd_drop)
    for parser in (auth, drop):
        run_option(parser)
