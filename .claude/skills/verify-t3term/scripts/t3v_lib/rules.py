"""The rules `t3v check` holds a run to, and the leak markers two of them look for.

A rule reads the report `t3v status` builds and returns one line for each way the run broke it,
or nothing. The rules hold at every step of a drive, so `check` can run after any of them; what a
step should show needs a reader and the captures. To add a rule, decorate a function with @rule:

    @rule
    def writes_too_big(report):
        if report['largestWrite'] > WRITE_LIMIT:
            yield f'largestWrite {report["largestWrite"]}: a write was over {WRITE_LIMIT} units'

A note is the same with @note, and never fails a check.
"""

from __future__ import annotations

import json

# Byte strings that only a control character t3term let through, or text past the label's cut or
# cap, could put in its output. t3term draws with its own SGR codes, but never these. ratatui also
# drops any grapheme with a control character in it. A scenario adds its own (its LEAKS).
LEAK_MARKERS = {
    'sgrRed': b'\x1b[31mred',
    'osc52Clipboard': b'\x1b]52;c;eA==',
    'c1Csi': b'\xc2\x9b2J',
    'bell': b'\x07bel',
    'labelPastTheCut': b'must not show',
    'labelPastTheCap': b'BEYOND-CAP',
    'labelSplitEmoji': '\U0001f469'.encode(),
}
# The markers that are plain text, which a screen capture can show.
SCREEN_MARKERS = ('labelPastTheCut', 'labelPastTheCap', 'labelSplitEmoji')

RULES = []
NOTES = []


def rule(check):
    RULES.append(check)
    return check


def note(check):
    NOTES.append(check)
    return check


def markers(scenario) -> tuple:
    """(every leak marker by name, the names a screen can show) for a run of `scenario`."""
    leaks = dict(LEAK_MARKERS, **getattr(scenario, 'LEAKS', {}))
    return leaks, SCREEN_MARKERS + tuple(getattr(scenario, 'SCREEN', ()))


def control_leaks(raw_path, leaks: dict, offset: int = 0):
    """How often each marker shows in what t3term wrote to the pane from byte `offset`, or None
    for each when raw.ansi is missing."""
    try:
        with open(raw_path, 'rb') as handle:
            handle.seek(offset)
            raw = handle.read()
    except OSError:
        return {name: None for name in leaks}
    return {name: raw.count(marker) for name, marker in leaks.items()}


def screen_leaks(screen, leaks: dict, names: tuple):
    """How often each text marker shows on `screen`, or None without one. A marker that t3term
    splits across two rows doesn't count here, so a 0 says only that no row holds a whole one."""
    if screen is None:
        return None
    return {name: screen.count(leaks[name].decode()) for name in names}


def nonzero(counts) -> dict:
    return {name: count for name, count in (counts or {}).items() if count}


def broken_rules(report: dict) -> list:
    """One line for each way the run broke a rule."""
    return [line for check in RULES for line in check(report) or ()]


def notes(report: dict) -> list:
    return [line for check in NOTES for line in check(report) or ()]


@rule
def server_counts(report):
    if report['countsFrom'] is None:
        yield ("the server's counts are missing: it is not running and wrote no summary.json, so "
               'scopeViolations and unknownRpcs went unchecked')


@rule
def control_leaks_held(report):
    if report['tmuxSession'] is None:
        return
    if report['rawAnsiBytes'] is None:
        yield 'raw.ansi is missing from the evidence, so controlLeaks went unchecked'
    leaks = nonzero(report['controlLeaks'])
    if leaks:
        yield f'controlLeaks {json.dumps(leaks)}: t3term wrote what it never writes'


@rule
def secrets_held(report):
    if report['secretLeaks'] is None and report['countsFrom'] is not None:
        yield 'secretLeaks is missing: the server answered without it, so no evidence was searched for its tokens'
    leaks = nonzero(report['secretLeaks'])
    if leaks:
        yield (f'secretLeaks {json.dumps(leaks)}: a token, ticket or control key the server issued is in '
               'the evidence or on screen')


@rule
def screen_leaks_held(report):
    leaks = nonzero(report['screenLeaks'])
    if leaks:
        yield f'screenLeaks {json.dumps(leaks)}: text that must stay hidden is on screen'


@rule
def scopes_held(report):
    if report['scopeViolations']:
        yield (f'scopeViolations {report["scopeViolations"]} {json.dumps(report["scopeRefusals"])}: a request '
               'reached the server without the scope it needs')


@rule
def known_rpcs(report):
    if report['unknownRpcs']:
        yield (f'unknownRpcs {report["unknownRpcs"]} {json.dumps(report["unknownRpcTags"])}: t3term called a '
               'method the server does not serve')


@rule
def stopped_cleanly(report):
    if not report['stopped']:
        return
    if report['sessionsAtStop'] is None:
        yield 'summary.json is missing from the evidence, so the after-stop rules went unchecked'
        return
    left = [s['id'] for s in report['sessionsAtStop'] if not s.get('revoked')]
    if left:
        yield f'sessionsUnrevoked {json.dumps(left)}: t3term quit without revoking every session it was issued'
    if report['openSocketsAtStop']:
        yield f'openSocketsAtStop {report["openSocketsAtStop"]}: a socket was still open when the server stopped'


@note
def commands_refused(report):
    if report['commandsRefused']:
        yield (f'commandsRefused {report["commandsRefused"]}: the server refused that many orchestration '
               'commands, as it refuses every one')
