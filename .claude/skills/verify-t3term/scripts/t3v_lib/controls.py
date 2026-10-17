"""The commands that change the fake server's world through the run's control routes, and how they
print what t3term should show after. Each prints the server's note, then `expect`, which is best
effort: worked out from what the server sent.
"""

from __future__ import annotations

from .runs import control_post, locate, post
from .text import die, safe
from .world import LABEL_PRESETS, RUN_STATUSES


def scenario_for(state: dict):
    from .scenarios import scenario_of  # here, as scenarios import this module
    return scenario_of(state)


def send(args, route: str, body: dict) -> dict:
    """POSTs to a control route of the run, prints its note and what t3term should show, and
    exits on a refusal."""
    root, state = locate(args.run)
    answer = post(root, route, body)
    if answer.get('note'):
        print(answer['note'])
    if answer.get('expect'):
        scenario_for(state).print_expect(answer['expect'])
    return answer


def live_state(root) -> dict:
    status, answer = control_post(root, '/__fixture/state', {})
    if status != 200:
        die(f"this run's server answered {status}: {answer.get('message')}; `t3v doctor` checks the run", 1)
    return answer


def print_world_expect(expect: dict):
    read = expect.get('labelRead')
    label = expect.get('label')
    print('  label read    ' + (f'the {read} preset, when t3term last started' if read
                                 else 'none yet: t3term has not read the descriptor'))
    print('  label shown   ' + (f'"{safe(label, 200)}" ({expect.get("labelColumns")} columns)' if label
                                 else 'none: the address stands in'))
    print(f'  address       {expect.get("address")}')
    identity = expect.get('identity')
    if identity:
        columns = expect.get('identityColumns', 0)
        print(f'  identity      "{safe(identity, 240)}" ({columns} columns, so whole in a sidebar of '
              f'{columns + 3} columns or more, which `t3v drag {columns + 2}` gives where the screen allows)')
    else:
        print('  identity      nothing: the dot shows alone')
    print(f'  feed          {expect.get("feed")}: {expect.get("dot")}')
    watches = expect.get('watches') or {}
    print('  thread watch  ' + (', '.join(f'{tid} {state}' for tid, state in watches.items())
                                or 'none open; the status line names the feed')
          + ('; new subscribeThread requests are refused' if expect.get('watchRefusing') else ''))
    if read and expect.get('labelServed') != read:
        print(f'  served now    the {expect.get("labelServed")} preset, which t3term shows only after `t3v restart`')
    shell = expect.get('shell') or {}
    print(f'  shell         {shell.get("open")} open, {shell.get("held")} held, hold {shell.get("holdSecondsLeft")} s '
          f'left, refusing {"yes" if shell.get("refusing") else "no"}, marker {"on" if shell.get("marker") else "off"}'
          f', {shell.get("withoutMarker")} stream(s) without it')
    print('  (best effort: worked out from what the server sent)')


def print_threads(entries: list):
    for entry in entries:
        line = (f'  {entry["threadId"]:<13} {safe(entry["title"], 24):<24} {entry["run"]}; '
                f'{entry["streams"]} open and {entry.get("held", 0)} held stream(s)')
        if entry.get('watch'):
            line += f', watch {entry["watch"]}'
        if not entry.get('listed', True):
            line += ', left off the shell'
        print(line)
        for key, name in (('shell', "shell's copy"), ('own', 'own copy')):
            if key in entry:
                print(f'      {name:<13} {entry[key]}')


def cmd_expect(args) -> int:
    root, state = locate(args.run)
    scenario_for(state).print_expect(live_state(root))
    return 0


def cmd_threads(args) -> int:
    root, state = locate(args.run)
    answer = live_state(root)
    print_threads(answer.get('threads', []))
    print()
    scenario_for(state).print_expect(answer)
    return 0


def cmd_label(args) -> int:
    send(args, '/__fixture/label', {'preset': args.preset})
    return 0


def cmd_shell(args) -> int:
    body = {}
    if args.action == 'hold':
        body = {'seconds': args.seconds, 'keep': args.keep}
    elif args.action in ('refuse', 'marker'):
        body = {'on': args.change == 'on'}
    send(args, f'/__fixture/shell/{args.action}', body)
    return 0


def cmd_pulse(args) -> int:
    send(args, '/__fixture/thread/pulse', {'threadId': args.thread, 'count': args.count, 'gap': args.gap})
    return 0


def cmd_run(args) -> int:
    send(args, '/__fixture/thread/run', {'threadId': args.thread, 'status': args.status})
    return 0


def cmd_close(args) -> int:
    send(args, '/__fixture/thread/close', {'threadId': args.thread})
    return 0


def cmd_thread_action(args) -> int:
    """gone, back and the scenario's own, such as late."""
    send(args, f'/__fixture/thread/{args.command}', {'threadId': args.thread})
    return 0


def cmd_watch(args) -> int:
    if args.action == 'refuse':
        if args.value not in ('on', 'off'):
            die('watch refuse takes on or off', 2)
        send(args, '/__fixture/watch/refuse', {'on': args.value == 'on'})
        return 0
    try:
        seconds = float(args.value)
    except ValueError:
        die('watch hold takes 0 to 120 seconds', 2)
    send(args, '/__fixture/watch/hold', {'seconds': seconds})
    return 0


def cmd_reconnect(args) -> int:
    send(args, '/__fixture/reconnect', {'hold': args.hold})
    return 0


def add_commands(commands, run_option):
    """The world's commands. `run_option` adds --run to a parser."""
    added = [commands.add_parser('expect', help='what t3term should show now, from what the server sent'),
             commands.add_parser('threads', help='the threads, their copies and streams, then expect')]
    added[0].set_defaults(func=cmd_expect)
    added[1].set_defaults(func=cmd_threads)

    label = commands.add_parser('label', help="serve another label; t3term shows it after restart")
    label.add_argument('preset', choices=LABEL_PRESETS)
    label.set_defaults(func=cmd_label)
    added.append(label)

    shell = commands.add_parser('shell', help='act on the shell subscription that feeds the sidebar')
    actions = shell.add_subparsers(dest='action', required=True, metavar='ACTION')
    actions.add_parser('end', help='end the open shell subscriptions with a success Exit')
    actions.add_parser('fail', help='fail the open shell subscriptions; t3term stops watching the feed')
    hold = actions.add_parser('hold', help='answer new subscribeShell requests only after S seconds')
    hold.add_argument('seconds', type=float)
    hold.add_argument('--keep', action='store_true', help='leave the open shell subscriptions open')
    for name, what in (('refuse', 'refuse new subscribeShell requests'),
                       ('marker', 'send or leave out the synchronized marker')):
        toggle = actions.add_parser(name, help=what)
        toggle.add_argument('change', choices=('on', 'off'))
    # On the parsers that run, not on `shell` itself, so a nested parser's default can't
    # overwrite a --run given before its action.
    for action in actions.choices.values():
        action.set_defaults(func=cmd_shell)
        added.append(action)

    pulse = commands.add_parser('pulse', help="assistant replies on the thread's own subscription")
    pulse.add_argument('thread')
    pulse.add_argument('--count', type=int, default=20, help='replies (default 20)')
    pulse.add_argument('--gap', type=float, default=0.5, help='seconds between them (default 0.5)')
    pulse.set_defaults(func=cmd_pulse)
    added.append(pulse)

    run = commands.add_parser('run', help="set the thread's latest run status")
    run.add_argument('thread')
    run.add_argument('status', choices=RUN_STATUSES)
    run.set_defaults(func=cmd_run)
    added.append(run)

    close = commands.add_parser('close', help="fail the thread's own subscribeThread streams, held ones too")
    close.add_argument('thread')
    close.set_defaults(func=cmd_close)
    added.append(close)

    for name, what in (('gone', 'a thread.removed on the shell'),
                       ('back', 'a thread.updated on the shell that lists the thread again')):
        action = commands.add_parser(name, help=what)
        action.add_argument('thread')
        action.set_defaults(func=cmd_thread_action)
        added.append(action)

    watch = commands.add_parser('watch', help='hold or refuse new subscribeThread requests')
    watch.add_argument('action', choices=('hold', 'refuse'))
    watch.add_argument('value', help='hold: 0 to 120 seconds, where 0 ends a hold; refuse: on or off')
    watch.set_defaults(func=cmd_watch)
    added.append(watch)

    reconnect = commands.add_parser('reconnect', help='drop every socket and refuse new ones for a while')
    reconnect.add_argument('--hold', type=float, default=3.0, help='seconds /ws answers 503 (default 3, at most 30)')
    reconnect.set_defaults(func=cmd_reconnect)
    added.append(reconnect)
    for parser in added:
        run_option(parser)
