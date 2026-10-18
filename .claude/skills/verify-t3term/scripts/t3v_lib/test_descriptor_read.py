"""Tests for what `t3v expect` says the TUI read from the descriptor. Anyone may read the
descriptor without a session: the TUI as it starts, a CLI line, the Environments dialog's check.
t3term reads it just before it logs in, so the label and capability the TUI holds come from the
read a TUI login follows, and a CLI line's read or a check's changes neither.

From the repository's root:
python3 -I -B -m unittest discover -s .claude/skills/verify-t3term/scripts/t3v_lib -t .claude/skills/verify-t3term/scripts
"""

import contextlib
import io
import shutil
import tempfile
import unittest
from pathlib import Path

from .orchestration import Orchestration
from .scenarios.links import LinksFixture


class DescriptorReadTest(unittest.TestCase):
    fixture_class = Orchestration

    def setUp(self):
        root = Path(tempfile.mkdtemp(prefix='t3v-descriptor-test-'))
        self.addCleanup(shutil.rmtree, root, ignore_errors=True)
        (root / 'control.key').write_text('test-control-key\n')
        (root / 'evidence').mkdir()
        # The fixture logs each event to stderr as well as events.jsonl.
        quiet = contextlib.ExitStack()
        quiet.enter_context(contextlib.redirect_stderr(io.StringIO()))
        self.addCleanup(quiet.close)
        self.fixture = self.fixture_class(root, {'evidence': str(root / 'evidence'), 'options': {}})

    def log_in(self, client, scopes=()):
        """A read of the descriptor, then a session for `client`, as t3term connects."""
        self.fixture.note_descriptor_read()
        self.fixture.issue_session({'scopes': list(scopes), 'client': client})

    def label(self):
        expect = self.fixture.expect()
        return expect['labelRead'], expect['labelServed']

    def test_the_tui_shows_the_label_it_read_before_its_login(self):
        self.assertEqual(self.label(), (None, 'normal'))
        self.log_in('tui')
        self.assertEqual(self.label(), ('normal', 'normal'))

    def test_a_cli_lines_read_leaves_the_tuis_label(self):
        self.log_in('tui')
        self.fixture.change_label('huge')
        self.log_in('cli')
        self.assertEqual(self.label(), ('normal', 'huge'))

    def test_a_check_with_no_login_leaves_the_tuis_label(self):
        self.log_in('tui')
        self.fixture.change_label('hostile')
        self.fixture.note_descriptor_read()
        self.assertEqual(self.label(), ('normal', 'hostile'))

    def test_a_terminal_session_leaves_the_tuis_label(self):
        self.log_in('tui')
        self.fixture.change_label('blank')
        self.log_in('tui', scopes=['terminal:read'])
        self.assertEqual(self.label(), ('normal', 'blank'))

    def test_a_new_tui_shows_the_label_served_now(self):
        self.log_in('tui')
        self.fixture.change_label('absent')
        self.log_in('cli')
        self.log_in('tui')
        self.assertEqual(self.label(), ('absent', 'absent'))


class CapabilityReadTest(DescriptorReadTest):
    """The links scenario's capability comes from the same read until a config arrives."""

    fixture_class = LinksFixture

    def test_a_cli_lines_read_leaves_the_tuis_capability(self):
        self.log_in('tui')
        self.fixture.change_capability('off')
        self.log_in('cli')
        self.assertEqual(self.fixture.capability_held()[0], 'on')
        self.log_in('tui')
        self.assertEqual(self.fixture.capability_held()[0], 'off')


if __name__ == '__main__':
    unittest.main()
