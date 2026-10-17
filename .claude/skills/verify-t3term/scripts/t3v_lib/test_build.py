"""Tests for `t3v build`: it checks out a commit only once it holds the build lock, and it caches a
binary only when cargo built it. A stand-in `cargo` on PATH copies the checked-out commit's
`marker` file to the binary's place and prints cargo's JSON messages, so no test runs the real
cargo or touches /tmp/t3v-bin or /tmp/t3v-target.

From the repository's root:
python3 -I -B -m unittest discover -s .claude/skills/verify-t3term/scripts/t3v_lib -t .claude/skills/verify-t3term/scripts
"""

import fcntl
import os
import shutil
import subprocess
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

from . import build

CARGO = '''#!/bin/sh
# Stands in for `cargo build`: links the tree's marker as t3term, or with FAKE_CARGO_FRESH=1 calls
# the binary already in the target directory fresh. FAKE_CARGO_STDERR is what it shows a person.
exe="$CARGO_TARGET_DIR/debug/t3term"
mkdir -p "$CARGO_TARGET_DIR/debug"
printf '%b' "${FAKE_CARGO_STDERR:-}" >&2
if [ "${FAKE_CARGO_FRESH:-0}" = 1 ]; then fresh=true; else fresh=false; cp marker "$exe"; chmod 755 "$exe"; fi
printf '{"reason":"compiler-artifact","target":{"kind":["lib"],"name":"t3term"},"fresh":%s,"executable":null}\\n' "$fresh"
printf '{"reason":"compiler-artifact","target":{"kind":["bin"],"name":"t3term"},"fresh":%s,"executable":"%s"}\\n' "$fresh" "$exe"
printf '{"reason":"build-finished","success":true}\\n'
'''

# What cargo shows a person as it compiles t3term with term.color = "always".
COLORED = '\\033[1m\\033[92m   Compiling\\033[0m t3term v0.1.0 (/tmp/tree)\\n'


def git(repo, *args):
    return subprocess.run(['git', '-C', str(repo), *args], check=True, capture_output=True, text=True).stdout.strip()


class BuildTest(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix='t3v-build-test-'))
        self.addCleanup(shutil.rmtree, self.root, ignore_errors=True)
        self.repo = self.root / 'repo'
        self.repo.mkdir()
        git(self.repo, 'init', '--quiet')
        self.commits = {}
        for name in ('first', 'second'):
            (self.repo / 'marker').write_text(f'{name}\n')
            git(self.repo, 'add', 'marker')
            git(self.repo, '-c', 'user.name=t3v', '-c', 'user.email=t3v@example.invalid', 'commit', '--quiet', '-m', name)
            self.commits[name] = git(self.repo, 'rev-parse', 'HEAD')
        tools = self.root / 'tools'
        tools.mkdir()
        (tools / 'cargo').write_text(CARGO)
        (tools / 'rustc').write_text('#!/bin/sh\necho "rustc 1.95.0 (stand-in)"\n')
        for tool in ('cargo', 'rustc'):
            os.chmod(tools / tool, 0o755)
        self.target = self.root / 'target'
        self.bins = self.root / 'bin'
        for patch in (mock.patch.object(build, 'TARGET', self.target),
                      mock.patch.object(build, 'BIN_ROOT', self.bins),
                      mock.patch.dict(os.environ, {'PATH': f'{tools}{os.pathsep}{os.environ["PATH"]}'})):
            patch.start()
            self.addCleanup(patch.stop)
        os.environ.pop('FAKE_CARGO_FRESH', None)
        os.environ.pop('FAKE_CARGO_STDERR', None)

    def cached(self, name):
        return self.bins / self.commits[name] / 't3term'

    def test_caches_the_binary_cargo_built_whatever_it_shows_a_person(self):
        # Quiet prints no Compiling line, and color wraps it in escapes.
        for shown in ('', COLORED):
            with self.subTest(stderr=shown):
                shutil.rmtree(self.bins, ignore_errors=True)
                os.environ['FAKE_CARGO_STDERR'] = shown
                out = build.build(self.commits['second'], self.repo)
                self.assertEqual(out, self.cached('second'))
                self.assertEqual(out.read_text(), 'second\n')
                self.assertTrue((out.parent / 'built.json').is_file())

    def test_caches_nothing_when_cargo_calls_the_binary_fresh(self):
        build.build(self.commits['first'], self.repo)
        os.environ['FAKE_CARGO_FRESH'] = '1'
        with self.assertRaises(SystemExit) as stopped:
            build.build(self.commits['second'], self.repo)
        self.assertEqual(stopped.exception.code, 1)
        self.assertFalse(self.cached('second').parent.exists())

    def test_checks_out_the_commit_only_once_it_holds_the_lock(self):
        self.target.mkdir(mode=0o700)
        lock = open(self.target / '.t3v-build.lock', 'w')
        self.addCleanup(lock.close)
        fcntl.flock(lock, fcntl.LOCK_EX)
        outcome = {}

        def run():
            try:
                outcome['out'] = build.build(self.commits['second'], self.repo)
            except BaseException as error:  # die() raises SystemExit, which a thread would drop
                outcome['error'] = error

        builder = threading.Thread(target=run)
        builder.start()
        trees = []
        deadline = time.monotonic() + 1.5
        while time.monotonic() < deadline:
            trees.append(git(self.repo, 'worktree', 'list', '--porcelain').count('worktree '))
            time.sleep(0.05)
        fcntl.flock(lock, fcntl.LOCK_UN)
        builder.join(30)
        self.assertFalse(builder.is_alive())
        self.assertEqual(max(trees), 1, 'a checkout appeared while another build held the lock')
        self.assertNotIn('error', outcome)
        self.assertEqual(outcome['out'].read_text(), 'second\n')


if __name__ == '__main__':
    unittest.main()
