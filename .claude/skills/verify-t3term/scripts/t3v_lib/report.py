"""What `t3v status`, `t3v check` and `t3v doctor` read: the event log, the live server or the
summary.json it wrote as it stopped, the screen and raw.ansi."""

from __future__ import annotations

import json
import os
import urllib.error
import urllib.request
from collections import Counter
from pathlib import Path

from . import rules
from .runs import (CURRENT, control_post, evidence_of, inspect, opener, origin_of, read_json, read_log,
                   runs_command, server_needles)
from .scenarios import scenario_of
from .tmux import own_t3term, pane, pane_size, screen_now


def prefixed(counts: dict, prefix: str) -> dict:
    return {k[len(prefix):]: v for k, v in sorted(counts.items()) if k.startswith(prefix)}


def raw_bytes(evidence: Path):
    try:
        return (evidence / 'raw.ansi').stat().st_size
    except OSError:
        return None


def settings_now(root: Path):
    """The keys of the run HOME's t3term settings.json, or None when there is none."""
    settings = read_json(root / 'home' / '.config' / 't3term' / 'settings.json')
    return sorted(settings) if isinstance(settings, dict) else None


def server_now(root: Path, state: dict) -> tuple:
    """(running, the live state or None, the server's own numbers or None, where they came from)."""
    running = runs_command(state.get('serverPid'), server_needles(root))
    live = None
    if running:
        status, answer = control_post(root, '/__fixture/state', {}, quiet=True)
        live = answer if status == 200 else None
    if live is not None:
        return running, live, live.get('server'), 'the live server'
    summary = read_json(evidence_of(state) / 'summary.json')
    if isinstance(summary, dict):
        return running, None, summary, 'summary.json'
    return running, None, None, None


def build_report(root: Path, state: dict) -> dict:
    """What `t3v status` prints and `t3v check` reads."""
    scenario = scenario_of(state)
    evidence = evidence_of(state)
    records = read_log(state)
    by_event = Counter(r.get('event') for r in records)
    tags = Counter(r.get('tag') for r in records if r.get('event') == 'rpc.request')
    running, live, server, counts_from = server_now(root, state)
    counts = (server or {}).get('counts') or {}
    screen = screen_now(state)
    if live is not None:
        status, answer = control_post(root, '/__fixture/leaks', {'screen': screen}, quiet=True)
        secret_leaks = answer.get('secretLeaks') if status == 200 else None
    else:
        secret_leaks = (server or {}).get('secretLeaks')
    shell_chunks = [r for r in records if r.get('event') == 'shell.chunk' and r.get('delivered')]
    marks = [r for r in records if r.get('event') == 'fixture.mark']
    last_mark = marks[-1] if marks else None
    leaks, screen_names = rules.markers(scenario)
    raw = evidence / 'raw.ansi'
    stopped = bool(state.get('stoppedAt'))
    binary = state.get('binary') or {}
    report = {
        'run': str(root),
        'evidence': str(evidence),
        'scenario': state.get('scenario'),
        'binary': {k: binary.get(k) for k in ('path', 'version', 'commit')},
        'serverRunning': running,
        'tmuxSession': pane(state) is not None if state.get('tmux') else None,
        'stopped': stopped,
        'feedExpected': live.get('feed') if live else None,
        'threadWatches': live.get('watches') if live else None,
        'labelRead': live.get('labelRead') if live else None,
        'labelServed': live.get('labelServed') if live else None,
    }
    report.update(scenario.status_fields(records, live))
    run_fields = getattr(scenario, 'run_fields', None)
    if run_fields is not None:
        # The scenario's numbers from the server, live or from summary.json, and from the screen.
        report.update(run_fields(server or {}, screen))
    report.update({
        'descriptorReads': by_event['descriptor.read'],
        'labelChanges': [r.get('preset') for r in records if r.get('event') == 'label.change'],
        'shellSubscribes': dict(Counter('resume' if r.get('resume') else 'fresh' for r in records
                                        if r.get('event') == 'shell.subscribe')),
        'shellHeld': by_event['shell.held'],
        'shellReleased': by_event['shell.released'],
        'shellEnded': sum(r.get('streams', 0) for r in records if r.get('event') == 'shell.end'),
        'shellFailed': sum(r.get('streams', 0) for r in records if r.get('event') == 'shell.fail'),
        'shellRefused': by_event['shell.refused'],
        'markersSent': sum(1 for r in shell_chunks if 'synchronized' in (r.get('kinds') or [])),
        'markersWithheld': by_event['shell.marker_withheld'],
        'shellNow': live.get('shell') if live else None,
        'feedHistory': [f'{r.get("state")}: {r.get("why")}' for r in records if r.get('event') == 'feed.expected'],
        'threadSubscribesByThread': dict(Counter(str(r.get('threadId')) for r in records
                                                 if r.get('event') == 'thread.subscribe')),
        'threadActions': dict(Counter(f'{r.get("action")}:{r.get("threadId")}' for r in records
                                      if r.get('event') == 'thread.action')),
        'threadHeld': by_event['thread.held'],
        'threadReleased': by_event['thread.released'],
        'threadRefused': by_event['thread.refused'],
        'watchRefusals': [r.get('on') for r in records if r.get('event') == 'watch.refusal'],
        'pulseReplies': by_event['pulse.reply'],
        'setupRequests': by_event['setup.request'],
        'configReads': tags['server.getConfig'],
        'rpcRequestsByTag': dict(tags),
        'reconnects': [r.get('holdSeconds') for r in records if r.get('event') == 'reconnect'],
        'interrupts': by_event['rpc.interrupt'],
        'commandsRefused': by_event['command.refused'],
        'authIssued': by_event['auth.issue'],
        'authRevoked': by_event['auth.revoke'],
        'sessionsIssued': dict(Counter(r.get('kind') for r in records if r.get('event') == 'auth.issue')),
        'sessionsRevoked': dict(Counter(r.get('kind') for r in records
                                        if r.get('event') == 'auth.revoke' and r.get('kind'))),
        'socketsOpened': by_event['ws.open'],
        'socketsClosed': by_event['ws.close'],
        'countsFrom': counts_from,
        'scopeViolations': counts.get('scopeViolations', 0),
        'scopeRefusals': prefixed(counts, 'scope.refused.'),
        'unknownRpcs': counts.get('unknownRpcs', 0),
        'unknownRpcTags': prefixed(counts, 'rpc.unknown.'),
        'fixtureErrors': counts.get('fixtureErrors', 0),
        'openSockets': (server or {}).get('openSockets') if counts_from == 'the live server' else None,
        'openSocketsAtStop': (server or {}).get('openSocketsAtStop') if counts_from == 'summary.json' else None,
        'sessionsAtStop': (server or {}).get('sessions') if counts_from == 'summary.json' else None,
        'captures': [r.get('name') for r in records if r.get('event') == 'fixture.capture'],
        'resizes': [f'{r.get("cols")}x{r.get("rows")}' for r in records if r.get('event') == 'fixture.resize'],
        'drags': [f'{r.get("fromCol")}->{r.get("toCol")}' for r in records if r.get('event') == 'fixture.drag'],
        'restarts': state.get('restarts', 0),
        'rawAnsiBytes': raw_bytes(evidence),
        'controlLeaks': rules.control_leaks(raw, leaks),
        'lastMark': (f'{last_mark.get("name")} at byte {last_mark.get("rawOffset")}' if last_mark else None),
        'controlLeaksSinceMark': (rules.control_leaks(raw, leaks, last_mark.get('rawOffset') or 0)
                                  if last_mark else None),
        'screenLeaks': rules.screen_leaks(screen, leaks, screen_names),
        'secretLeaks': secret_leaks,
        'settingsFile': settings_now(root),
    })
    return report


def descriptor_id(root: Path):
    """The environment id this run's port answers with, read with the control key so the server
    counts it as a probe rather than a read by t3term. (id, None) or (None, why)."""
    origin = origin_of(root)
    if origin is None:
        return None, 'no server-runtime.json, so no port'
    try:
        key = (root / 'control.key').read_text().strip()
    except OSError:
        return None, 'the control key is missing'
    request = urllib.request.Request(origin + '/.well-known/t3/environment', headers={'X-Fixture-Control': key})
    try:
        with opener().open(request, timeout=5) as response:
            answer = json.loads(response.read() or b'{}')
    except (urllib.error.URLError, OSError, ValueError) as error:
        return None, f'{origin} did not answer: {error}'
    return answer.get('environmentId'), None


def doctor(path) -> dict:
    """Each check as {'check', 'ok', 'detail'}, and the verdict. Reads only."""
    checks = []

    def check(name: str, ok, detail: str):
        checks.append({'check': name, 'ok': ok, 'detail': detail})

    root, state, problem = inspect(path)
    check('run', problem is None, problem or f'{root}: marker and nonce match run.json')
    if problem:
        return {'run': str(root), 'checks': checks, 'ok': False}

    server_pid = state.get('serverPid')
    check('server', runs_command(server_pid, server_needles(root)),
          f'pid {server_pid} runs `t3v serve` for this run' if runs_command(server_pid, server_needles(root))
          else f'pid {server_pid} is gone or runs something else; `t3v start` starts a new run')

    found, why = descriptor_id(root)
    want = state.get('environmentId')
    check('port', found == want, f'{origin_of(root)} answers with {want}' if found == want
          else why or f'the port answers with {found}, not {want}')

    binary = state.get('binary') or {}
    if state.get('tmux'):
        pid = state.get('t3termPid')
        running = own_t3term(state) is not None
        check('t3term', running, f'pid {pid} runs {binary.get("path")} in the run\'s pane' if running
              else f'pid {pid} is not running in the run\'s pane; `t3v restart` starts t3term again')
        info = pane(state)
        size = pane_size(state) if info is not None else None
        want_size = (state.get('cols'), state.get('rows'))
        if info is None:
            check('tmux', False, f'no session {state.get("session")} on {state.get("socket")}')
        elif info[0]:
            check('tmux', False, f'the pane is dead: t3term {info[1]}')
        else:
            check('tmux', size == want_size, f'the pane is {size[0]}x{size[1]}' if size == want_size
                  else f'the pane is {size} but run.json records {want_size[0]}x{want_size[1]}')
    else:
        check('t3term', True, 'started with --no-tmux, so t3term runs wherever you ran it')
        check('tmux', True, 'started with --no-tmux')

    try:
        stat = os.stat(binary.get('path', ''))
        same = stat.st_size == binary.get('size') and stat.st_mtime_ns == binary.get('mtimeNs')
        check('binary', same, f'{binary.get("path")} has the size and mtime run.json records' if same
              else f'{binary.get("path")} changed since start (size {stat.st_size}, was {binary.get("size")})')
    except OSError as error:
        check('binary', False, f'{binary.get("path")}: {error.strerror}')

    broken = rules.broken_rules(build_report(root, state))
    check('rules', not broken, 'every rule in `t3v check` holds so far' if not broken else '; '.join(broken))
    current = os.path.realpath(CURRENT) == str(root) if CURRENT.is_symlink() else False
    return {'run': str(root), 'current': current, 'checks': checks, 'ok': all(c['ok'] for c in checks)}
