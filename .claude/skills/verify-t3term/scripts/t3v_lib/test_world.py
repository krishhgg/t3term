"""Tests for the fake T3's world: the worktree setup snapshot has every field T3's contract has, so
t3term reads it as it would read T3's.

From the repository's root:
python3 -I -B -m unittest discover -s .claude/skills/verify-t3term/scripts/t3v_lib -t .claude/skills/verify-t3term/scripts
"""

import unittest
from types import SimpleNamespace

from .world import setup_snapshot

# The fields of WorktreeSetupSnapshot, its setupScript and WorktreeSetupStage in T3's
# packages/contracts/src/worktreeSetup.ts at f570bd2. Each is required; a nullable one is null.
SNAPSHOT = {'threadId', 'phase', 'startedAt', 'endedAt', 'branch', 'baseRef', 'worktreePath',
            'setupScript', 'stages', 'error', 'sequence'}
SCRIPT = {'name', 'command', 'terminalId'}
STAGE = {'id', 'status', 'startedAt', 'endedAt', 'percent', 'detail', 'tail'}
STAGE_IDS = ['fetch', 'checkout', 'submodules', 'setup-script', 'agent']
STATUSES = {'pending', 'running', 'done', 'skipped', 'warning', 'failed'}


class SetupSnapshotTest(unittest.TestCase):
    def snapshot(self, phase: str) -> dict:
        thread = SimpleNamespace(tid='thread-1', watched={'branch': 't3code/fix', 'worktreePath': '/w/fix'})
        return setup_snapshot(thread, phase, 3, 1_700_000_000_000)

    def test_each_phase_has_every_field_of_the_contract(self):
        for phase in ('running', 'done', 'failed'):
            with self.subTest(phase=phase):
                snapshot = self.snapshot(phase)
                self.assertEqual(set(snapshot), SNAPSHOT)
                self.assertEqual(set(snapshot['setupScript']), SCRIPT)
                self.assertTrue(all(snapshot['setupScript'].values()))
                self.assertEqual([stage['id'] for stage in snapshot['stages']], STAGE_IDS)
                for stage in snapshot['stages']:
                    self.assertEqual(set(stage), STAGE, stage['id'])
                    self.assertIn(stage['status'], STATUSES)
                    self.assertIsInstance(stage['tail'], list)
                    self.assertTrue(all(isinstance(line, str) and len(line) <= 400 for line in stage['tail']))
                    self.assertIsNone(stage['percent'])
                    self.assertIsNone(stage['detail'])
                self.assertEqual(snapshot['sequence'], 3)

    def test_only_a_settled_setup_ends_and_only_a_failed_one_says_why(self):
        running, done, failed = (self.snapshot(phase) for phase in ('running', 'done', 'failed'))
        self.assertIsNone(running['endedAt'])
        self.assertIsNone(running['error'])
        self.assertIsNotNone(done['endedAt'])
        self.assertIsNone(done['error'])
        self.assertIsNotNone(failed['endedAt'])
        self.assertTrue(failed['error'])
        self.assertLessEqual(len(failed['error']), 1000)

    def test_a_stage_that_hasnt_started_has_null_times(self):
        stages = {stage['id']: stage for stage in self.snapshot('running')['stages']}
        for stage_id in ('submodules', 'agent'):
            self.assertIsNone(stages[stage_id]['startedAt'], stage_id)
            self.assertIsNone(stages[stage_id]['endedAt'], stage_id)
        self.assertIsNone(stages['setup-script']['endedAt'])
        self.assertTrue(stages['setup-script']['tail'])


if __name__ == '__main__':
    unittest.main()
