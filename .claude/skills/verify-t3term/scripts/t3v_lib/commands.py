"""The commands that start, drive, check and end a run. cli.py wires them to argparse; the
world's commands are in controls.py and each scenario's module."""

from __future__ import annotations

import json
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from . import rules
from .build import choose_binary, describe_binary, renderer, toplevel
from .report import build_report, doctor, raw_bytes
from .runs import (CURRENT, RUN_PREFIX, SCRIPTS, SESSION, control_post, evidence_of, forget_current, inspect,
                   locate, make_evidence, make_run, mark, origin_of, owned, point_current, python_command,
                   read_log, run_argument, save_state, server_needles, terminate)
from .scenarios import SCENARIOS, scenario_of
from .text import NAME, die, iso, now_ms, safe, text_places
from .tmux import (capture_screen, last_rows, launch, needs_pane, pane, pane_size, quit_t3term, respawn,
                   send_mouse, sidebar_edge, t3term_command, tmux, wait_for_t3term, wait_until)


def emit(args, value: dict, lines: list) -> None:
    """One JSON object on stdout with --json, else the lines."""
    if getattr(args, 'json', False):
        print(json.dumps(value, indent=1))
    else:
        print('\n'.join(lines))


def watch_line(root: Path) -> str:
    return f'env -u TMUX tmux -S {root}/tmux.sock attach -r -t {SESSION}'


def wait_ready(state: dict, timeout: float, ready=None) -> dict:
    """Waits until t3term shows `ready`, else the run's ready text, as `t3v wait --text` does."""
    return wait_until(state, texts=[ready or state['ready']], timeout=timeout)


def not_ready(args, root: Path, state: dict, waited: dict, what: str):
    """Exits 1 for a t3term that never showed the ready text, leaving the run for a look."""
    why = '; '.join(waited['failed'])
    message = (f'{what}: {why} after {waited["elapsed"]:.1f} s. The run is still up: `t3v doctor --run {root}` '
               f'checks it, `t3v cleanup --run {root}` ends it')
    if getattr(args, 'json', False):
        print(json.dumps({'ok': False, 'error': message, 'run': str(root), 'evidence': state['evidence'],
                          'lastRows': last_rows(waited['screen'])}, indent=1))
    else:
        print(f't3v: {message}. The last rows:\n{last_rows(waited["screen"])}', file=sys.stderr)
    sys.exit(1)


# -- start --

def cmd_start(args) -> int:
    scenario = SCENARIOS[args.scenario]
    if not args.no_tmux and not shutil.which('tmux'):
        die('tmux is not on PATH; install it (brew install tmux) or pass --no-tmux', 2)
    if args.cols < 80 or args.rows < 24:
        die('start at 80x24 or more; `t3v resize` goes smaller afterwards', 2)
    if not 0 <= args.shell_hold <= 120:
        die('--shell-hold takes 0 to 120 seconds', 2)
    if args.max_seconds <= 0 or args.ready_timeout <= 0:
        die('--max-seconds and --ready-timeout take a number of seconds above 0', 2)
    binary = choose_binary(args.binary, args.rev, args.repo)
    described = describe_binary(binary)
    root, nonce = make_run()
    try:
        evidence = make_evidence(args.evidence, root)
    except SystemExit:
        shutil.rmtree(root)
        raise
    state = {
        'root': str(root), 'nonce': nonce, 'scenario': scenario.NAME, 'binary': described,
        'evidence': str(evidence), 'socket': str(root / 'tmux.sock'), 'session': SESSION,
        'cols': args.cols, 'rows': args.rows, 'color': args.color, 'tmux': not args.no_tmux,
        'options': {'label': args.label, 'shellHold': args.shell_hold, **scenario.options_of(args)},
        'ready': args.ready or scenario.READY, 'maxSeconds': args.max_seconds,
        'environmentId': f'env-t3v-{root.name[len(RUN_PREFIX):]}', 'restarts': 0, 'createdAt': iso(now_ms()),
    }
    save_state(root, state)
    previous = os.path.realpath(CURRENT) if CURRENT.is_symlink() else None
    current_note = point_current(root)

    with open(root / 'logs' / 'server.log', 'ab') as log:
        server = subprocess.Popen([*python_command(), 'serve', '--run', str(root)], stdin=subprocess.DEVNULL,
                                  stdout=log, stderr=log, start_new_session=True, close_fds=True)
    state['serverPid'] = server.pid
    save_state(root, state)
    deadline = time.monotonic() + 10
    while origin_of(root) is None:
        if server.poll() is not None or time.monotonic() > deadline:
            terminate(server.pid, server_needles(root), 'server')
            tail = (root / 'logs' / 'server.log').read_text(errors='replace')[-2000:]
            print(tail, file=sys.stderr)
            die(f'the fake server did not start (its log is above); `t3v cleanup --run {root}` removes the run')
        time.sleep(0.05)
    origin = origin_of(root)
    summary = {'ok': True, 'run': str(root), 'evidence': str(evidence), 'scenario': scenario.NAME,
               'options': state['options'], 'binary': described, 'server': {'origin': origin, 'pid': server.pid},
               'environmentId': state['environmentId'], 'note': current_note or None}
    options = ', '.join(f'{k} {v}' for k, v in state['options'].items())
    head = [f'  run        {root}', f'  evidence   {evidence}', f'  scenario   {scenario.NAME} ({options})',
            f'  server     {origin}, pid {server.pid}',
            f'  binary     {described["path"]} ({described["version"]}'
            + (f', commit {described["commit"][:12]})' if described.get('commit') else ')')]
    if current_note:
        head.append(f'  note       {current_note}')
    elif previous and previous != str(root):
        head.append(f'  previous   {CURRENT} named {previous}; reach it with --run {previous}')

    if args.no_tmux:
        line = ' '.join(shlex.quote(x) for x in t3term_command(root, state))
        summary.update(t3term=None, env=line)
        emit(args, summary, ['The fake server is up. Run t3term in any terminal with exactly this line:', '',
                             '  ' + line, '', *head, '',
                             '`t3v expect` prints what t3term should show; `t3v cleanup` ends the run.'])
        return 0

    problems = launch(root, state)
    if problems:
        tmux(state, 'kill-server')
        terminate(server.pid, server_needles(root), 'server')
        die(f'{problems[0]}; `t3v cleanup --run {root}` removes the run')
    pid = wait_for_t3term(state)
    state['t3termPid'] = pid
    save_state(root, state)
    mark(state, 'fixture.t3term_started', binary=described['path'], cols=args.cols, rows=args.rows, pid=pid)
    waited = wait_ready(state, args.ready_timeout)
    if not waited['ok']:
        not_ready(args, root, state, waited, f't3term did not show {json.dumps(state["ready"])}')
    mark(state, 'fixture.ready', text=state['ready'], elapsedMs=round(waited['elapsed'] * 1000))
    summary.update(t3term={'pid': pid, 'cols': args.cols, 'rows': args.rows, 'color': args.color},
                   readySeconds=round(waited['elapsed'], 3), watch=watch_line(root))
    emit(args, summary, [f't3term is up and shows {json.dumps(state["ready"])} after {waited["elapsed"]:.1f} s.',
                         *head, f'  t3term     pid {pid}, {args.cols}x{args.rows}, color {args.color}',
                         f'  watch      {watch_line(root)}  (read-only; detach with C-b d)', '',
                         '`t3v expect` prints what t3term should show; `t3v doctor` checks the run; '
                         '`t3v cleanup` ends it.'])
    return 0


# -- stop and cleanup --

def sinks_of(root: Path) -> list:
    """The pids of this run's pipe-sink processes."""
    result = subprocess.run(['ps', '-axo', 'pid=,command='], capture_output=True, text=True)
    pids = []
    for line in result.stdout.splitlines():
        pid, _, command = line.strip().partition(' ')
        if 'pipe-sink' in command and f'--run {root} ' in command + ' ' and pid.isdigit():
            pids.append(int(pid))
    return pids


def stop_run(root: Path, state: dict) -> list:
    """Quits t3term, stops the run's tmux server and the fake server. Notes on each step."""
    notes = []
    if state.get('tmux'):
        info = quit_t3term(state, 'stop')
        if info and info[0]:
            notes.append(f't3term: exited with status {info[1]}')
            mark(state, 'fixture.t3term_exit', status=info[1])
        notes.append(terminate(state.get('t3termPid'), [state['binary']['path']], 't3term'))
        killed = tmux(state, 'kill-server').returncode == 0
        notes.append("tmux: the run's own server stopped" if killed else "tmux: no server on the run's socket")
        # pipe-sink ends once tmux closes its pipe; wait for it so raw.ansi is whole.
        deadline = time.monotonic() + 3
        while sinks_of(root) and time.monotonic() < deadline:
            time.sleep(0.05)
        left = sinks_of(root)
        if left:
            notes.append(f'pipe-sink: pid(s) {left} still run 3 s after tmux stopped')
    notes.append(terminate(state.get('serverPid'), server_needles(root), 'server'))
    if not state.get('stoppedAt'):
        state['stoppedAt'] = iso(now_ms())
        save_state(root, state)
        mark(state, 'fixture.stopped')
    return notes


def cmd_stop(args) -> int:
    root, state = locate(args.run)
    print('\n'.join(stop_run(root, state)))
    print(f'evidence kept in {state["evidence"]}; `t3v check` now holds the run to the after-stop rules')
    return 0


def cmd_cleanup(args) -> int:
    root, state = locate(args.run)
    print('\n'.join(stop_run(root, state)))
    _, state, problem = inspect(root)
    if problem or not owned(root, state):
        die(f'{root} no longer looks like this run, so t3v left it in place')
    forget_current(root)
    shutil.rmtree(root)
    print(f'deleted {root}\nevidence kept in {state["evidence"]}')
    return 0


# -- what a run is doing --

def cmd_status(args) -> int:
    root, state = locate(args.run)
    report = build_report(root, state)
    if args.json:
        print(json.dumps(report, indent=1))
        return 0
    for key, value in report.items():
        print(f'{key:<26} {json.dumps(value)}')
    return 0


def cmd_check(args) -> int:
    root, state = locate(args.run)
    report = build_report(root, state)
    broken, notes = rules.broken_rules(report), rules.notes(report)
    mark(state, 'fixture.check', broken=len(broken))
    lines = [f'broken: {line}' for line in broken] + [f'note: {line}' for line in notes]
    lines.append(f'check: {len(broken)} rule(s) broken' if broken else 'check: every rule held')
    emit(args, {'ok': not broken, 'run': str(root), 'stopped': report['stopped'], 'countsFrom': report['countsFrom'],
                'broken': broken, 'notes': notes}, lines)
    return 1 if broken else 0


def cmd_doctor(args) -> int:
    path, _ = run_argument(args.run)
    result = doctor(path)
    failed = [c for c in result['checks'] if not c['ok']]
    lines = [f'{"ok" if c["ok"] else "FAIL":<5} {c["check"]:<7} {c["detail"]}' for c in result['checks']]
    lines.append('doctor: every check passed' if not failed else f'doctor: {len(failed)} check(s) failed')
    emit(args, result, lines)
    return 0 if result['ok'] else 1


def cmd_env(args) -> int:
    root, state = locate(args.run)
    print(' '.join(shlex.quote(x) for x in t3term_command(root, state)))
    return 0


def cmd_wait(args) -> int:
    root, state = locate(args.run)
    needs_pane(state, 'wait')
    if not (args.text or args.gone or args.idle):
        die('wait needs at least one --text, --gone or --idle', 2)
    if args.idle < 0 or args.timeout <= 0:
        die('--idle takes milliseconds from 0 and --timeout seconds above 0', 2)
    result = wait_until(state, texts=args.text or (), gone=args.gone or (), idle_ms=args.idle, timeout=args.timeout)
    mark(state, 'fixture.wait', ok=result['ok'], elapsedMs=round(result['elapsed'] * 1000),
         text=[safe(t) for t in args.text or ()] or None, gone=[safe(t) for t in args.gone or ()] or None,
         idleMs=args.idle or None)
    if result['ok']:
        print(f'held after {result["elapsed"]:.2f} s')
        return 0
    print(f'timed out after {result["elapsed"]:.2f} s: ' + '; '.join(result['failed']))
    print('the last screen:')
    print(result['screen'] if result['screen'] is not None else '(no screen)')
    return 1


# -- input --

def cmd_capture(args) -> int:
    root, state = locate(args.run)
    needs_pane(state, 'capture')
    if not NAME.match(args.name):
        die('a capture name is letters, digits, dot, dash and underscore', 2)
    _, text_path, ansi_path = capture_screen(state, args.name)
    size = pane_size(state) or (state['cols'], state['rows'])
    mark(state, 'fixture.capture', name=args.name, cols=size[0], rows=size[1])
    print(f'{text_path}\n{ansi_path}')
    return 0


def cmd_keys(args) -> int:
    root, state = locate(args.run)
    needs_pane(state, 'keys')
    if args.gap < 0:
        die('--gap takes seconds from 0', 2)
    for i, key in enumerate(args.keys):
        flags = ['-l'] if args.literal else []
        result = tmux(state, 'send-keys', '-t', state['session'], *flags, key)
        if result.returncode != 0:
            die(f'tmux send-keys failed: {result.stderr.strip()}; `t3v doctor` checks the run')
        # Literal text may be a message, so the log keeps only its length.
        mark(state, 'fixture.keys', key=f'<{len(key)} literal characters>' if args.literal else key,
             literal=args.literal)
        if args.gap > 0 and i + 1 < len(args.keys):
            time.sleep(args.gap)
    return 0


def screen_text(state: dict) -> str:
    plain = tmux(state, 'capture-pane', '-p', '-t', state['session'])
    if plain.returncode != 0:
        die(f'tmux capture-pane failed: {plain.stderr.strip()}; `t3v doctor` checks the run')
    return plain.stdout


def cmd_click(args) -> int:
    root, state = locate(args.run)
    needs_pane(state, 'click')
    if args.text:
        places = [p for p in text_places(screen_text(state), args.text)
                  if p[0] >= args.from_col and p[1] >= args.from_row]
        if args.nth < 1 or len(places) < args.nth:
            die(f'that text shows {len(places)} time(s) on screen now at column {args.from_col} and row '
                f'{args.from_row} or past them; `t3v wait --text` waits for it', 1)
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
    mark(state, 'fixture.click', col=col, row=row, textLength=len(args.text) if args.text else None,
         nth=args.nth if args.text else None, button=args.button, repeat=args.repeat)
    print(f'{"clicked" if args.button == "left" else args.button} column {col}, row {row}'
          + (f' x{args.repeat}' if args.repeat > 1 else '')
          + (f', match {args.nth} of the text' if args.text else ''))
    return 0


def cmd_drag(args) -> int:
    root, state = locate(args.run)
    needs_pane(state, 'drag')
    if args.at:
        col, row = args.at
    else:
        col = sidebar_edge(screen_text(state))
        if col is None:
            die("the sidebar's Settled footer isn't whole on screen; show the sidebar (keys C-b) or pass "
                '--at COL ROW', 1)
        row = state['rows'] // 2
    if not (0 <= col < state['cols'] and 0 <= row < state['rows'] and 0 <= args.to < state['cols']):
        die('the press and the release must be inside the pane, counted from 0', 2)
    # A press, motion with the left button held (button 32 in SGR), then the release.
    steps = 6
    path = [round(col + (args.to - col) * k / steps) for k in range(1, steps + 1)]
    report = f'\x1b[<0;{col + 1};{row + 1}M'
    report += ''.join(f'\x1b[<32;{x + 1};{row + 1}M' for x in path)
    report += f'\x1b[<0;{args.to + 1};{row + 1}m'
    send_mouse(state, report)
    mark(state, 'fixture.drag', fromCol=col, row=row, toCol=args.to)
    print(f'dragged from column {col} to column {args.to} on row {row}')
    return 0


def cmd_resize(args) -> int:
    root, state = locate(args.run)
    needs_pane(state, 'resize')
    if not (10 <= args.cols <= 500 and 5 <= args.rows <= 200):
        die('resize takes 10 to 500 columns and 5 to 200 rows', 2)
    result = tmux(state, 'resize-window', '-t', state['session'], '-x', str(args.cols), '-y', str(args.rows))
    if result.returncode != 0:
        die(f'tmux resize-window failed: {result.stderr.strip()}; `t3v doctor` checks the run')
    state.update(cols=args.cols, rows=args.rows)
    save_state(root, state)
    mark(state, 'fixture.resize', cols=args.cols, rows=args.rows)
    print(f'resized to {args.cols}x{args.rows}')
    return 0


def cmd_mark(args) -> int:
    root, state = locate(args.run)
    if not NAME.match(args.name):
        die('a mark name is letters, digits, dot, dash and underscore', 2)
    offset = raw_bytes(evidence_of(state)) or 0
    mark(state, 'fixture.mark', name=args.name, rawOffset=offset)
    print(f'mark {args.name} at byte {offset} of raw.ansi; `t3v status` counts leaks after it in '
          'controlLeaksSinceMark')
    return 0


def cmd_restart(args) -> int:
    root, state = locate(args.run)
    needs_pane(state, 'restart')
    if pane(state) is None:
        die('the tmux session is gone; `t3v cleanup` ends this run and `t3v start` starts a new one', 1)
    info = quit_t3term(state, 'restart')
    if not info or not info[0]:
        die('t3term did not quit within 8 s; `t3v stop` stops it', 1)
    mark(state, 'fixture.t3term_exit', status=info[1])
    count = state.get('restarts', 0) + 1
    problems = respawn(root, state, count)
    if problems:
        die(f'{problems[0]}; `t3v doctor` checks the run')
    pid = wait_for_t3term(state)
    state.update(t3termPid=pid, restarts=count)
    save_state(root, state)
    mark(state, 'fixture.t3term_restarted', restart=count, pid=pid)
    if pid is None:
        die(f't3term did not come back; see {state["evidence"]}/raw.ansi', 1)
    ready = args.ready or state['ready']
    waited = wait_ready(state, args.ready_timeout, ready)
    if not waited['ok']:
        not_ready(args, root, state, waited, f'restart: t3term did not show {json.dumps(ready)}')
    print(f't3term restarted (restart {count}), pid {pid}, same HOME, after {waited["elapsed"]:.1f} s. It read the '
          f'descriptor again, so it shows the label served now, and its feed starts over.\n'
          f'recording: {state["evidence"]}/session-{count}.cast')
    status, answer = control_post(root, '/__fixture/state', {}, quiet=True)
    if status == 200:
        scenario_of(state).print_expect(answer)
    return 0


# -- render --

def capture_size(state: dict, name: str):
    """(cols, rows) a capture recorded, else the run's size now."""
    for record in reversed(read_log(state)):
        if record.get('event') == 'fixture.capture' and record.get('name') == name and record.get('cols'):
            return record['cols'], record.get('rows')
    return state.get('cols'), state.get('rows')


def cmd_render(args) -> int:
    given_file = '/' in args.capture or args.capture.endswith('.ans')
    bare_out = '/' not in args.out
    state = None
    if args.run or not given_file or bare_out or not args.cols:
        _, state = locate(args.run)
    if given_file:
        source = Path(os.path.realpath(os.path.expanduser(args.capture)))
        cols, rows = args.cols or (state or {}).get('cols'), args.rows
    else:
        if not NAME.match(args.capture):
            die('a capture name is letters, digits, dot, dash and underscore', 2)
        source = evidence_of(state) / f'{args.capture}.ans'
        cols, rows = capture_size(state, args.capture)
        cols, rows = args.cols or cols, args.rows or rows
    if not source.is_file():
        die(f'{source} is missing; `t3v capture NAME` saves NAME.ans in the evidence directory', 1)
    if not args.out.lower().endswith('.png'):
        die('OUT must end in .png', 2)
    out = evidence_of(state) / args.out if bare_out else Path(os.path.realpath(os.path.expanduser(args.out)))
    repo = toplevel(SCRIPTS)
    if repo is not None and (out == repo or repo in out.parents):
        die(f'{out} is inside the repository; pass a bare name such as pulls.png to write it to the evidence '
            'directory, then copy it where you need it', 2)
    if not out.parent.is_dir():
        die(f'{out.parent} does not exist; make it first or pass a bare name for the evidence directory', 2)
    tool = renderer()
    # A copy in a scratch folder, since the renderer writes OUT only outside the folder IN is in.
    scratch = Path(tempfile.mkdtemp(prefix='t3v-render-', dir='/tmp'))
    try:
        copy = scratch / source.name
        shutil.copyfile(source, copy)
        command = [str(tool), str(copy), str(out), '--cols', str(cols)] + (['--rows', str(rows)] if rows else [])
        result = subprocess.run(command, capture_output=True, text=True, stdin=subprocess.DEVNULL)
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
    if result.stderr.strip():
        print(result.stderr.strip(), file=sys.stderr)
    if result.returncode != 0:
        die(f'the renderer failed with exit {result.returncode}; its message is above', 1)
    if state is not None:
        mark(state, 'fixture.render', capture=source.name, out=str(out), cols=cols, rows=rows)
    print(out)
    return 0


# -- internal --

def cmd_serve(args) -> int:
    from .scenarios import fixture_of
    from .server import serve

    root, state, problem = inspect(args.run)
    if problem:
        die(problem, 2)
    return serve(root, state, fixture_of(state))


def cmd_pipe_sink(args) -> int:
    from .tmux import pipe_sink

    root, state, problem = inspect(args.run)
    if problem:
        die(problem, 2)
    if not NAME.match(args.cast):
        die('a cast name is letters, digits, dot, dash and underscore', 2)
    return pipe_sink(root, args.cast)

