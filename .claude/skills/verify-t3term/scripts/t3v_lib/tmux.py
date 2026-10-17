"""t3term in tmux: starting it, keys, mouse reports, captures, waits and the pipe sink.

Each run has its own tmux server on the run's socket (`tmux -S RUN/tmux.sock`, started with
`-f /dev/null`), so neither your tmux server nor your tmux.conf is touched. t3term runs in it
under `env -i` with HOME and T3CODE_HOME inside the run directory, T3TERM_NO_SAVED_LOGIN=1 (so no
Keychain) and NO_PROXY for loopback. It never sees your ~/.t3, your Keychain, a real T3 server or
a real credential.
"""

from __future__ import annotations

import codecs
import json
import os
import re
import select
import shlex
import signal
import subprocess
import time
from pathlib import Path

from .runs import SESSION, command_of, evidence_of, mark, python_command, read_json
from .text import cells, die

# What `capture-pane -e` writes besides text: SGR, and OSC 8 for a link.
ESCAPES = re.compile(r'\x1b\[[0-9;:]*m|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)')

POLL = 0.05  # seconds between two looks at the pane


def tmux(state: dict, *args, check: bool = False):
    # Without TMUX, a helper run from inside your tmux talks only to the run's socket.
    env = {k: v for k, v in os.environ.items() if k not in ('TMUX', 'TMUX_PANE')}
    return subprocess.run(['tmux', '-S', state['socket'], '-f', '/dev/null', *args],
                          capture_output=True, text=True, timeout=15, check=check, env=env)


def pane(state: dict):
    """(dead, ending, pid) for t3term's pane, or None when the session is gone. ENDING says how a
    dead pane's process ended, such as 'exited with status 1' or 'was killed by SIGKILL (9)',
    and is None while it runs."""
    if not state.get('tmux'):
        return None
    result = tmux(state, 'display-message', '-p', '-t', state['session'],
                  '#{pane_dead}|#{pane_dead_status}|#{pane_dead_signal}|#{pane_pid}')
    if result.returncode != 0:
        return None
    parts = result.stdout.strip().split('|')
    if len(parts) != 4:
        return None
    dead_flag, status, killer, pid = parts
    dead = dead_flag == '1'
    ending = None
    if dead and killer and killer != '0':
        ending = f'was killed by {signal_name(killer)}'
    elif dead:
        ending = f'exited with status {status}' if status.isdigit() else 'exited'
    return dead, ending, int(pid) if pid.isdigit() else None


def signal_name(killer: str) -> str:
    """SIGKILL for tmux's 'kill' or '9'. tmux 3.6a gives the name, older ones the number."""
    try:
        found = signal.Signals(int(killer)) if killer.isdigit() else signal.Signals['SIG' + killer.upper()]
    except (KeyError, ValueError):
        return f'signal {killer}'
    return f'{found.name} ({found.value})'


def pane_size(state: dict):
    """(cols, rows) of t3term's pane, or None when the session is gone."""
    result = tmux(state, 'display-message', '-p', '-t', state['session'], '#{pane_width} #{pane_height}')
    parts = result.stdout.split() if result.returncode == 0 else []
    if len(parts) != 2 or not all(p.isdigit() for p in parts):
        return None
    return int(parts[0]), int(parts[1])


def needs_pane(state: dict, what: str):
    if not state.get('tmux'):
        die(f'{what} needs the tmux pane, and this run started with --no-tmux', 2)


def t3term_env(root: Path, state: dict, client: str = 'tui') -> dict:
    """t3term's whole environment. `client` 'cli' gives it the `t3` whose sessions the server
    counts as a CLI line's, which leave what the TUI should show alone."""
    home = root / 'home'
    env = {
        'HOME': str(home),
        'T3CODE_HOME': str(home / '.t3'),
        'T3TERM_NO_SAVED_LOGIN': '1',
        'T3TERM_T3_COMMAND': str(root / 'bin' / ('t3-shim-cli' if client == 'cli' else 't3-shim')),
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


def t3term_command(root: Path, state: dict, client: str = 'tui') -> list:
    env = t3term_env(root, state, client)
    return ['/usr/bin/env', '-i', *[f'{k}={v}' for k, v in env.items()], state['binary']['path']]


def launch_script(go: Path, command: list) -> str:
    """The pane's shell line: wait until the pipe to pipe-sink is attached, then become t3term."""
    return (f'while [ ! -e {shlex.quote(str(go))} ]; do sleep 0.05; done; '
            f'exec {" ".join(shlex.quote(x) for x in command)}')


def sink_command(root: Path, cast: str) -> str:
    return ' '.join(shlex.quote(x) for x in ('exec', *python_command(), 'pipe-sink', '--run', str(root),
                                             '--cast', cast))


def launch(root: Path, state: dict) -> list:
    """Starts the run's tmux server with t3term's pane waiting on `go`. The failed step's
    message, or an empty list."""
    go = root / 'run' / 'go'
    steps = [
        ('new-session', '-d', '-s', SESSION, '-x', str(state['cols']), '-y', str(state['rows']), '/bin/sh', '-c',
         launch_script(go, t3term_command(root, state))),
        ('set-option', '-g', 'remain-on-exit', 'on'),
        ('set-option', '-g', 'status', 'off'),
        ('set-option', '-g', 'window-size', 'manual'),
        ('set-option', '-g', 'escape-time', '10'),
        ('pipe-pane', '-t', SESSION, sink_command(root, 'session.cast')),
    ]
    for step in steps:
        result = tmux(state, *step)
        if result.returncode != 0:
            return [f'tmux {step[0]} failed: {result.stderr.strip()}']
    go.write_text('')
    return []


def respawn(root: Path, state: dict, count: int) -> list:
    """Starts t3term again in the same pane, recording to session-COUNT.cast."""
    go, cast = root / 'run' / f'go-{count}', f'session-{count}.cast'
    # The new shell waits for `go`, so the pipe is attached before t3term writes anything.
    steps = (('respawn-pane', '-k', '-t', state['session'], '/bin/sh', '-c',
              launch_script(go, t3term_command(root, state))),
             ('pipe-pane', '-t', state['session'], sink_command(root, cast)))
    for step in steps:
        result = tmux(state, *step)
        if result.returncode != 0:
            return [f'tmux {step[0]} failed: {result.stderr.strip()}']
    go.write_text('')
    return []


def own_t3term(state: dict):
    """t3term's pid while this run's pane is alive and runs the pid the run recorded with the
    run's binary, else None. The pane is on the run's own tmux socket, so a pid that another
    process took after t3term exited, even another run of the same build, never passes."""
    pid = state.get('t3termPid')
    info = pane(state) if pid else None
    if info is None or info[0] or info[2] != pid:
        return None
    return pid if state['binary']['path'] in command_of(pid) else None


def wait_for_t3term(state: dict, seconds: float = 5):
    """t3term's pid once the pane's shell has become it, or None if it exited or took too long."""
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        info = pane(state)
        if info and info[2] and state['binary']['path'] in command_of(info[2]):
            return info[2]
        if info and info[0]:
            return None
        time.sleep(POLL)
    return None


def quit_t3term(state: dict, why: str):
    """Ctrl+Q to t3term, then waits up to 8 s for it to exit, revoking its session on the way.
    The pane's (dead, status, pid) after, or None when the session is gone."""
    info = pane(state)
    if info and not info[0]:
        tmux(state, 'send-keys', '-t', state['session'], 'C-q')
        mark(state, 'fixture.keys', key='C-q', literal=False, reason=why)
        deadline = time.monotonic() + 8
        while time.monotonic() < deadline:
            info = pane(state)
            if not info or info[0]:
                break
            time.sleep(POLL)
    return info


def screen_now(state: dict):
    """The pane's text now, or None without a live tmux session."""
    if not state.get('tmux') or pane(state) is None:
        return None
    result = tmux(state, 'capture-pane', '-p', '-t', state['session'])
    return result.stdout if result.returncode == 0 else None


def last_rows(screen, count: int = 12) -> str:
    if not screen:
        return '(no screen)'
    rows = screen.rstrip('\n').split('\n')
    return '\n'.join(rows[-count:])


def capture_screen(state: dict, name: str):
    """Saves the screen as NAME.txt (plain) and NAME.ans (ANSI) in the evidence directory, both
    from one capture, so the text and its render show the same frame."""
    styled = tmux(state, 'capture-pane', '-p', '-e', '-t', state['session'])
    if styled.returncode != 0:
        die(f'tmux capture-pane failed: {styled.stderr.strip()}; `t3v doctor` checks the run')
    plain = plain_of(styled.stdout)
    evidence = evidence_of(state)
    text_path, ansi_path = evidence / f'{name}.txt', evidence / f'{name}.ans'
    text_path.write_text(plain)
    ansi_path.write_text(styled.stdout)
    return plain, text_path, ansi_path


def plain_of(styled: str) -> str:
    """What `capture-pane -p` gives for the frame `capture-pane -p -e` gave: the text without
    its escape sequences, and each row without the spaces it ends with."""
    text = ESCAPES.sub('', styled)
    return '\n'.join(row.rstrip(' ') for row in text.split('\n'))


def send_mouse(state: dict, report: str):
    result = tmux(state, 'send-keys', '-t', state['session'], '-H', *(f'{b:02x}' for b in report.encode()))
    if result.returncode != 0:
        die(f'tmux send-keys failed: {result.stderr.strip()}')


def sidebar_edge(screen: str):
    """The sidebar's edge column: three right of the chevron that ends the Settled footer. The
    footer leaves two columns after its chevron, and the edge is the sidebar's next column, its
    last (src/tui/sidebar.rs and `drag_edge` in src/tui/mod.rs's tests). None when the footer
    isn't whole on screen."""
    for line in reversed(screen.split('\n')):
        start = line.find('Settled (')
        if start < 0:
            continue
        for i in range(start, len(line)):
            if line[i] in '›⌄':
                return cells(line[:i]) + 3
        return None
    return None


def wait_until(state: dict, texts=(), gone=(), idle_ms: int = 0, timeout: float = 10.0) -> dict:
    """Polls the pane every 50 ms until every text in `texts` shows, none in `gone` does and the
    screen has not changed for `idle_ms`, all at once. Stops early when t3term exits.

    Returns {'ok', 'elapsed', 'failed': the conditions that didn't hold at the last look,
    'screen': the last screen, 'exited': how t3term ended, or None while it runs}."""
    started = time.monotonic()
    deadline = started + timeout
    last, changed = None, started
    while True:
        now = time.monotonic()
        result = tmux(state, 'capture-pane', '-p', '-t', state['session'])
        screen = result.stdout if result.returncode == 0 else None
        # After the capture, so a screen that a dead t3term left never passes.
        info = pane(state)
        if info is None:
            return {'ok': False, 'elapsed': now - started, 'failed': ['the tmux session is gone'],
                    'screen': screen if screen is not None else last, 'exited': None}
        if screen != last:
            last, changed = screen, now
        failed = [] if screen is not None else [f'tmux capture-pane failed: {result.stderr.strip()}']
        failed += [f'--text {json.dumps(t)} is not on screen' for t in texts if screen is not None and t not in screen]
        failed += [f'--gone {json.dumps(t)} is still on screen' for t in gone if screen is not None and t in screen]
        quiet = (now - changed) * 1000
        if idle_ms and quiet < idle_ms:
            failed.append(f'--idle {idle_ms}: the screen changed {quiet:.0f} ms ago')
        if info[0]:
            return {'ok': False, 'elapsed': now - started, 'failed': [f't3term {info[1]}'] + failed,
                    'screen': screen, 'exited': info[1]}
        if not failed:
            return {'ok': True, 'elapsed': now - started, 'failed': [], 'screen': screen, 'exited': None}
        if now >= deadline:
            return {'ok': False, 'elapsed': now - started, 'failed': failed, 'screen': screen, 'exited': None}
        time.sleep(POLL)


def cast_name(state: dict) -> str:
    """The recording t3term writes to now: session.cast, then session-N.cast after restart N."""
    count = state.get('restarts') or 0
    return f'session-{count}.cast' if count else 'session.cast'


def note_resize(root: Path, state: dict, cols: int, rows: int):
    """Hands a resize to the pipe-sink that records the pane now. It writes it between two output
    records, so a player redraws at the new size from there."""
    try:
        fd = os.open(root / 'run' / f'{cast_name(state)}.resize', os.O_WRONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
    except OSError:
        return
    try:
        # Shorter than PIPE_BUF, so the sink reads it whole.
        os.write(fd, f'{cols}x{rows}\n'.encode())
    except OSError:
        pass
    finally:
        os.close(fd)


def pipe_sink(root: Path, cast_name: str) -> int:
    """Reads t3term's output from tmux pipe-pane into raw.ansi and an asciicast v2 file, both in
    the evidence directory. It is the recording's only writer. `t3v resize` hands it each size
    through a FIFO, it reads a waiting size before more output, and it stamps each record from
    a monotonic clock as it writes it, so the records stay in order."""
    state = read_json(root / 'run.json') or {}
    evidence = Path(state.get('evidence') or root / 'tmp')
    started, clock = time.time(), time.monotonic()
    fifo = root / 'run' / f'{cast_name}.resize'
    try:
        if fifo.is_symlink() or fifo.exists():
            fifo.unlink()
        os.mkfifo(fifo, 0o600)
        sizes = os.open(fifo, os.O_RDONLY | os.O_NONBLOCK)
        # Open for writing too, so the FIFO never reads as ended between two resizes.
        keep = os.open(fifo, os.O_WRONLY | os.O_NONBLOCK)
    except OSError:
        sizes = keep = None  # the recording goes on without resize records
    decoder = codecs.getincrementaldecoder('utf-8')('replace')
    waiting = b''
    with open(evidence / 'raw.ansi', 'ab') as raw, open(evidence / cast_name, 'a', encoding='utf-8') as cast:
        cast.write(json.dumps({'version': 2, 'width': state.get('cols', 200), 'height': state.get('rows', 50),
                               'timestamp': int(started), 'title': f't3v {root.name}',
                               'env': {'TERM': 'xterm-256color'}}) + '\n')
        cast.flush()

        def record(kind: str, data: str):
            cast.write(json.dumps([round(time.monotonic() - clock, 6), kind, data]) + '\n')

        try:
            while True:
                ready = select.select([0] if sizes is None else [0, sizes], [], [])[0]
                if sizes in ready:
                    waiting += os.read(sizes, 4096)
                    *lines, waiting = waiting.split(b'\n')
                    for line in lines:
                        if re.fullmatch(rb'\d{1,4}x\d{1,4}', line):
                            record('r', line.decode())
                    cast.flush()
                    continue
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
                    record('o', text)
                    cast.flush()
        finally:
            if sizes is not None:
                os.close(sizes)
                os.close(keep)
                try:
                    fifo.unlink()
                except OSError:
                    pass
    return 0
