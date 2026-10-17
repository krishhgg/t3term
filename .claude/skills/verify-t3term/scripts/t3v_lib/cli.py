"""The command line: argparse for every t3v command."""

from __future__ import annotations

import argparse
import sys

from . import commands as c
from . import controls
from .build import cmd_build
from .scenarios import DEFAULT, SCENARIOS
from .text import JSON_ERRORS
from .world import LABEL_PRESETS

DESCRIPTION = """\
Runs the real t3term TUI in tmux against a fake T3 server on 127.0.0.1, drives it with keys and
mouse reports, and keeps what it showed as evidence.

  t3v start                  start a run: the fake server, then t3term in tmux
  t3v wait --text T          wait for the screen; drives use it after each action
  t3v keys/click/drag        drive t3term
  t3v capture NAME           save the screen; `t3v render NAME NAME.png` draws it
  t3v check                  exit 1 if the run broke a rule
  t3v cleanup                stop the run and delete it, keeping the evidence

A command that acts on a run takes --run DIR, else $T3V_RUN, else /tmp/t3v-current.
`t3v COMMAND --help` describes each one."""


def run_option(parser):
    parser.add_argument('--run', '--dir', dest='run', metavar='DIR',
                        help='the run directory (default: $T3V_RUN, else /tmp/t3v-current)')


def json_option(parser):
    parser.add_argument('--json', action='store_true', help='print one JSON object on stdout and nothing else')


def add_start(commands):
    start = commands.add_parser('start', help='start a run: the fake server, then t3term in tmux, and wait until '
                                              'it is ready')
    start.add_argument('--scenario', choices=sorted(SCENARIOS), default=DEFAULT,
                       help=f'the world the server serves (default {DEFAULT})')
    start.add_argument('--binary', metavar='PATH', help='the t3term to run (default: target/debug/t3term under '
                                                        'the git toplevel of the current directory)')
    start.add_argument('--rev', metavar='REV', help='run `t3v build REV` first and run its result')
    start.add_argument('--repo', metavar='PATH', help='with --rev, the checkout to build from')
    start.add_argument('--evidence', metavar='DIR', help='where captures, raw.ansi, the recordings, events.jsonl '
                                                         'and summary.json go (default /tmp/t3v-evidence/RUN)')
    start.add_argument('--label', choices=LABEL_PRESETS, default='normal', help="the descriptor's label")
    start.add_argument('--shell-hold', type=float, default=0.0, metavar='S',
                       help='answer subscribeShell only S seconds after the server starts')
    for scenario in SCENARIOS.values():
        scenario.add_start_options(start)
    start.add_argument('--cols', type=int, default=200, help='pane width (default 200)')
    start.add_argument('--rows', type=int, default=50, help='pane height (default 50)')
    start.add_argument('--color', choices=('truecolor', '256'), default='truecolor')
    start.add_argument('--no-tmux', action='store_true', help='start only the server and print the t3term line')
    start.add_argument('--ready', metavar='TEXT', help="the text that means t3term is ready (default: the "
                                                       "scenario's, for links its first thread's title)")
    start.add_argument('--ready-timeout', type=float, default=20.0, metavar='S',
                       help='seconds to wait for the ready text (default 20)')
    start.add_argument('--max-seconds', type=float, default=7200, metavar='S',
                       help='the server quits t3term and stops itself after S seconds (default 7200)')
    json_option(start)
    start.set_defaults(func=c.cmd_start)


def add_run_commands(commands):
    """The commands that check, drive and end a run."""
    def command(name, func, help_text, json=False):
        parser = commands.add_parser(name, help=help_text)
        parser.set_defaults(func=func)
        run_option(parser)
        if json:
            json_option(parser)
        return parser

    command('status', c.cmd_status, 'what the event log, the server and the screen say now', json=True)
    command('check', c.cmd_check, 'exit 1 with one line per rule the run broke', json=True)
    command('doctor', c.cmd_doctor, 'check the run itself: its directory, pids, port, pane and binary; reads only',
            json=True)
    wait = command('wait', c.cmd_wait, 'wait until the screen shows what you ask, polling every 50 ms')
    wait.add_argument('--text', action='append', metavar='T', help='T must show (repeatable; all must show)')
    wait.add_argument('--gone', action='append', metavar='T', help='T must not show (repeatable)')
    wait.add_argument('--idle', type=int, default=0, metavar='MS', help='the screen has not changed for MS ms')
    wait.add_argument('--timeout', type=float, default=10.0, metavar='S', help='give up after S seconds (default 10)')

    capture = command('capture', c.cmd_capture, 'save the screen as NAME.txt and NAME.ans in the evidence directory')
    capture.add_argument('name')
    keys = command('keys', c.cmd_keys, 'send tmux keys to t3term, such as Enter, C-b or M-b, and log them')
    keys.add_argument('keys', nargs='+')
    keys.add_argument('--gap', type=float, default=0.2, metavar='S', help='seconds between two keys (default 0.2)')
    keys.add_argument('--literal', action='store_true', help='send each argument as literal text')
    click = command('click', c.cmd_click, 'send a left click or wheel notches as SGR mouse reports')
    where = click.add_mutually_exclusive_group(required=True)
    where.add_argument('--text', help='where this text first shows on screen now, or the --nth place')
    where.add_argument('--at', nargs=2, type=int, metavar=('COL', 'ROW'), help='counted from 0')
    click.add_argument('--nth', type=int, default=1, help='with --text, which place it shows, from 1')
    click.add_argument('--from-col', type=int, default=0, metavar='COL', help='with --text, only at COL or right of it')
    click.add_argument('--from-row', type=int, default=0, metavar='ROW', help='with --text, only at ROW or below it')
    click.add_argument('--button', choices=('left', 'wheel-up', 'wheel-down'), default='left')
    click.add_argument('--repeat', type=int, default=1, help='send it this many times (1 to 50)')
    drag = command('drag', c.cmd_drag, "drag the sidebar's edge, or from --at, and release at column TO")
    drag.add_argument('to', type=int, help='the column to release at, from 0')
    drag.add_argument('--at', nargs=2, type=int, metavar=('COL', 'ROW'), help='press here instead of on the edge')
    resize = command('resize', c.cmd_resize, "resize t3term's tmux window")
    resize.add_argument('cols', type=int)
    resize.add_argument('rows', type=int)
    marker = command('mark', c.cmd_mark, 'note where raw.ansi ends now, for controlLeaksSinceMark in status')
    marker.add_argument('name')
    restart = command('restart', c.cmd_restart, 'quit t3term and start it again with the same HOME')
    restart.add_argument('--ready-timeout', type=float, default=20.0, metavar='S',
                         help='seconds to wait for the ready text (default 20)')
    render = command('render', c.cmd_render, 'draw a capture as a PNG with render.swift')
    render.add_argument('capture', metavar='NAME|FILE.ans', help="a capture's name in the evidence directory, "
                                                                 'or a file')
    render.add_argument('out', metavar='OUT.png', help='a bare name goes to the evidence directory')
    render.add_argument('--cols', type=int, help='the width of the capture (default: what the capture recorded)')
    render.add_argument('--rows', type=int, help='the height of the capture (default: what the capture recorded)')
    command('env', c.cmd_env, 'print the env -i line that runs t3term against this run')
    command('stop', c.cmd_stop, 'quit t3term, stop tmux and the server; keep the run and the evidence')
    command('cleanup', c.cmd_cleanup, 'stop, then delete the run directory; the evidence stays')


def add_build(commands):
    build = commands.add_parser('build', help='build t3term at REV once into /tmp/t3v-bin/SHA/t3term and print '
                                              'the path')
    build.add_argument('rev')
    build.add_argument('--repo', metavar='PATH', help='the checkout to build from (default: the git toplevel of '
                                                      'the current directory)')
    build.set_defaults(func=cmd_build)


def add_internal(commands):
    serve = commands.add_parser('serve', help='internal: run the fake server in the foreground')
    serve.add_argument('--run', required=True)
    serve.set_defaults(func=c.cmd_serve)
    sink = commands.add_parser('pipe-sink', help='internal: the target of tmux pipe-pane')
    sink.add_argument('--run', required=True)
    sink.add_argument('--cast', default='session.cast')
    sink.set_defaults(func=c.cmd_pipe_sink)


def parser() -> argparse.ArgumentParser:
    top = argparse.ArgumentParser(prog='t3v', description=DESCRIPTION,
                                  formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = top.add_subparsers(dest='command', required=True, metavar='COMMAND')
    add_start(commands)
    add_run_commands(commands)
    add_build(commands)
    controls.add_commands(commands, run_option)
    for scenario in SCENARIOS.values():
        scenario.add_commands(commands, run_option)
    add_internal(commands)
    return top


def main(argv=None) -> int:
    argv = sys.argv[1:] if argv is None else argv
    if argv[:1] == ['t3-shim']:
        # t3term runs this as its `t3` command; it must not parse t3's own flags.
        from .auth import shim
        return shim(argv[1:])
    args = parser().parse_args(argv)
    JSON_ERRORS['on'] = bool(getattr(args, 'json', False))
    return args.func(args)
