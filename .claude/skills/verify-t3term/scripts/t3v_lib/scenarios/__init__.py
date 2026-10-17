"""The worlds a run can serve. Each scenario module provides:

  NAME, READY        its name, and the text `t3v start` waits for before it returns
  LEAKS, SCREEN      leak markers that rules.py adds to its own for `t3v check`
  LinksFixture       or another Orchestration subclass: the server's world and methods
  add_start_options  `t3v start` options of its own, and options_of to read them
  add_commands       its own commands
  print_expect       how `t3v expect` prints what t3term should show
  status_fields      its part of `t3v status`
"""

from __future__ import annotations

from . import links

SCENARIOS = {links.NAME: links}
DEFAULT = links.NAME
FIXTURES = {links.NAME: links.LinksFixture}


def scenario_of(state: dict):
    return SCENARIOS[state.get('scenario') or DEFAULT]


def fixture_of(state: dict):
    return FIXTURES[state.get('scenario') or DEFAULT]
