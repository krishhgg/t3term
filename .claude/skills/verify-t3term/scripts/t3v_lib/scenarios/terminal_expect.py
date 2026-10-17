"""What the terminal scenario prints and checks: the terminals `t3v expect` and `t3v term` show,
its part of `t3v status`, and the rules it adds to `t3v check`.

Every rule here reads a field only the terminal scenario's report has, and skips when the field
is missing, so a run of another scenario never trips one.
"""

from __future__ import annotations

import json
from collections import Counter

from .. import controls
from ..rules import nonzero, rule
from ..text import safe
from .terminal_data import DUPLICATE, UNEXPECTED_TERMINAL, WINDOW_CHUNKS, WRITE_LIMIT

# What a helper did just before t3term asked for a terminal session, for firstTerminalIssueAfter.
HELPER_STEPS = ('fixture.keys', 'fixture.click', 'fixture.type', 'fixture.paste', 'fixture.t3term_started',
                'fixture.t3term_restarted')


def prefixed(counts: dict, prefix: str) -> dict:
    return {k[len(prefix):]: v for k, v in sorted(counts.items()) if k.startswith(prefix)}


# ---- `t3v expect` and `t3v term` ----


def print_terminal(row: dict):
    if 'sequence' in row:
        line = (f'  {row["threadId"]:<13} {row["status"]:<8} sequence {row["sequence"]}, pid {row["pid"]}, '
                f'{row["historyBytes"]} bytes and {row["historyLines"]} lines of history, '
                f'size {row["size"]}, shell {row["shell"]}')
    else:
        line = f'  {row["threadId"]:<13} {row["status"]:<8} {row["keptHistory"]} bytes of history kept'
    print(line + f'; {row["streams"]} stream(s), {row["unacked"]} chunk(s) unacked, {row["waiting"]} held back')
    print(f'      header   {safe(row["header"], 200)}')
    if row.get('lastRow'):
        print(f'      last row {safe(row["lastRow"], 200)}')
    if row['status'] in ('missing', 'closed'):
        launch = row['expectedLaunch']
        print(f'      Start    cwd {safe(launch["cwd"], 80) or "(blank: t3term must not start one)"}'
              f', worktreePath {launch.get("worktreePath")}, env {sorted(launch["env"])}')


def print_terminals(rows: list):
    for row in rows:
        print_terminal(row)
    print('  (a header is what the drawer shows once attached; a label shows once it has had a snapshot)')


def print_expect(expect: dict):
    rows = expect.get('terminals')
    if rows is not None:
        print('  terminals')
        print_terminals(rows)
    settings = expect.get('settings') or {}
    print(f'  permissions   /api/auth/session answers a terminal session with the {expect.get("permissions")} '
          'permissions')
    print(f'  term settings hold-attach {settings.get("holdAttachSecondsLeft")} s left, slow '
          f'{settings.get("slowMs")} ms, fail-writes {"on" if settings.get("failWrites") else "off"}, '
          f'fail-resize {"on" if settings.get("failResize") else "off"}, '
          f'missing-cwd {"on" if settings.get("missingCwd") else "off"}')
    controls.print_world_expect(expect)


# ---- `t3v status` ----


def status_fields(records: list, live) -> dict:
    """The terminal scenario's part of `t3v status`, from the event log and the live server."""
    issued = Counter(r.get('kind') for r in records if r.get('event') == 'auth.issue')
    first = next((i for i, r in enumerate(records)
                  if r.get('event') == 'auth.issue' and r.get('kind') == 'terminal'), None)
    before = None
    if first is not None:
        before = next((r for r in reversed(records[:first])
                       if r.get('by') == 'helper' and r.get('event') in HELPER_STEPS), None)
    requests = [r for r in records if r.get('event') == 'rpc.request']
    attaches = [r for r in requests if r.get('tag') == 'terminal.attach']
    by_kind = {}
    for r in requests:
        by_kind.setdefault(str(r.get('kind')), Counter())[r.get('tag')] += 1
    return {
        'terminalSessionsIssued': issued['terminal'],
        'firstTerminalIssueAfter': (f'{before["event"]} {before.get("key") or before.get("button") or ""}'.strip()
                                    if before else None),
        'attachKeys': dict(Counter(','.join((r.get('payload') or {}).get('keys') or []) for r in attaches)),
        # From the log, so a request refused for its scope counts too.
        'unexpectedTerminalRpcs': dict(Counter(r.get('tag') for r in requests if r.get('tag') in UNEXPECTED_TERMINAL)),
        'rpcRequestsByKind': {kind: dict(tags) for kind, tags in sorted(by_kind.items())},
        'terminalResizesSent': [f'{r.get("cols")}x{r.get("rows")}' for r in records if r.get('event') == 'term.resize'],
        'drops': [r.get('holdSeconds') for r in records if r.get('event') == 'drop'],
        'permissions': live.get('permissions') if live else None,
        'terminals': {row['threadId']: row['status'] for row in live.get('terminals') or []} if live else None,
    }


def run_fields(server: dict, screen) -> dict:
    """The terminal counters, from the live server's numbers or summary.json, and the screen."""
    counts = server.get('counts') or {}
    maxima = server.get('maxima') or {}
    stopped = 'stoppedAt' in server
    sessions = server.get('sessions') or []
    return {
        'attaches': {k: v for k, v in prefixed(counts, 'attach.').items()
                     if not k.startswith(('violation.', 'launchMismatch.'))},
        'attachViolations': prefixed(counts, 'attach.violation.'),
        'launchMismatches': prefixed(counts, 'attach.launchMismatch.'),
        'terminalInterrupts': counts.get('terminal.interrupt', 0),
        'terminalWrites': prefixed(counts, 'write.'),
        'writesOverlapped': counts.get('write.overlapped', 0),
        'largestWrite': maxima.get('write.largest', 0),
        'terminalResizes': prefixed(counts, 'resize.'),
        'window': prefixed(counts, 'window.'),
        'maxUnacked': maxima.get('window.maxUnacked', 0),
        'maxChunkBytes': maxima.get('window.maxChunkBytes', 0),
        'maxWaiting': maxima.get('window.maxWaiting', 0),
        'excessAcks': counts.get('window.excessAcks', 0),
        'acksOnOtherStreams': prefixed(counts, 'rpc.ackOther.'),
        'fakeShell': prefixed(counts, 'fakeShell.'),
        'termControls': prefixed(counts, 'term.control.'),
        'socketsByKind': None if stopped else server.get('socketsByKind'),
        'socketsHeld': prefixed(counts, 'ws.held.'),
        'sessionChecks': prefixed(counts, 'auth.check.'),
        'terminalSocketsAtStop': (server.get('openSocketsAtStopByKind') or {}).get('terminal', 0) if stopped else None,
        'terminalSessionsUnrevoked': ([s.get('id') for s in sessions if s.get('kind') == 'terminal'
                                       and not s.get('revoked')] if stopped else None),
        'duplicateOnScreen': screen.count(DUPLICATE) if screen is not None else None,
    }


# ---- `t3v check` ----


@rule
def attach_contract(report):
    for name, why in (('attachViolations', 'a terminal.attach carried a field the drawer never sends'),
                      ('launchMismatches', "a launch differed from the thread's cwd, worktree or env"),
                      ('unexpectedTerminalRpcs', 'the drawer sent a terminal RPC it never sends')):
        broken = nonzero(report.get(name))
        if broken:
            yield f'{name} {json.dumps(broken)}: {why}'


@rule
def output_window(report):
    if report.get('maxUnacked') is None:
        return
    if report['maxUnacked'] > WINDOW_CHUNKS:
        yield f'maxUnacked {report["maxUnacked"]}: more than {WINDOW_CHUNKS} chunks waited for an Ack'
    if report.get('excessAcks'):
        yield f'excessAcks {report["excessAcks"]}: an Ack came for no chunk'
    stray = (report.get('acksOnOtherStreams') or {}).get('terminal', 0)
    if stray:
        yield f'acksOnOtherStreams.terminal {stray}: an Ack on the terminal socket named no open attach stream'


@rule
def writes_in_order(report):
    if report.get('writesOverlapped'):
        yield f'writesOverlapped {report["writesOverlapped"]}: a write went before the last one was answered'
    if (report.get('largestWrite') or 0) > WRITE_LIMIT:
        yield f'largestWrite {report["largestWrite"]}: a write was over {WRITE_LIMIT} UTF-16 units'


@rule
def duplicate_once(report):
    shown = report.get('duplicateOnScreen')
    if shown is None:
        return
    sent = (report.get('termControls') or {}).get('duplicate', 0)
    if shown > sent:
        yield f'duplicateOnScreen {shown} after {sent} `term duplicate`: an event showed twice'


@rule
def terminal_stopped_cleanly(report):
    if report.get('terminalSocketsAtStop'):
        yield (f'terminalSocketsAtStop {report["terminalSocketsAtStop"]}: a terminal socket was still open when '
               'the server stopped')
    if report.get('terminalSessionsUnrevoked'):
        yield (f'terminalSessionsUnrevoked {json.dumps(report["terminalSessionsUnrevoked"])}: t3term quit without '
               'revoking its terminal session')
