"""The worlds a run can serve. Each scenario module provides:

  NAME, READY        its name, and the text `t3v start` waits for before it returns
  LEAKS, SCREEN      leak markers that rules.py adds to its own for `t3v check`
  LinksFixture       or another Orchestration subclass: the server's world and methods
  add_start_options  `t3v start` options of its own, and options_of to read them
  add_commands       its own commands
  print_expect       how `t3v expect` prints what t3term should show
  status_fields      its part of `t3v status`

and may provide:

  print_threads      how `t3v threads` lists its threads, given the state and --all
  run_fields         more of `t3v status`, from the server's numbers (live, or summary.json
                     after stop) and the screen now
"""

from __future__ import annotations

from . import lineage, links, terminal

SCENARIOS = {links.NAME: links, lineage.NAME: lineage, terminal.NAME: terminal}
DEFAULT = links.NAME
FIXTURES = {links.NAME: links.LinksFixture, lineage.NAME: lineage.LineageFixture,
            terminal.NAME: terminal.TerminalFixture}


def scenario_of(state: dict):
    return SCENARIOS[state.get('scenario') or DEFAULT]


def fixture_of(state: dict):
    return FIXTURES[state.get('scenario') or DEFAULT]
