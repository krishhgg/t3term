"""Run directories, evidence directories, the pids a run started and the control route.

A run directory, /tmp/t3v-run-XXXXXXXX from mkdtemp, holds only live state: the fake HOME, the
tmux socket, the pids, the control key, the `t3` shim and scratch files. Its marker file .t3v-run
holds a random nonce that run.json repeats, and every command that acts on a run checks the
directory's name, its parent and the nonce first. /tmp/t3v-current links to the newest run.

What a run proves goes to its evidence directory instead, /tmp/t3v-evidence/<run name>/ unless
`start --evidence` names another: the captures, raw.ansi, the asciicast recordings, events.jsonl
and summary.json. `cleanup` deletes the run directory and never the evidence directory. No
token, ticket, session secret or control key reaches the evidence directory: the server writes
`<redacted>` in their place.
"""

from __future__ import annotations

import hmac
import json
import os
import re
import secrets
import shlex
import signal
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

from .text import die, iso, now_ms

SCRIPT = Path(os.path.realpath(__file__)).parent.parent / 't3v'
SCRIPTS = SCRIPT.parent
TMP = Path(os.path.realpath('/tmp'))
RUN_PREFIX = 't3v-run-'
MARKER = '.t3v-run'
STATE = 'run.json'
CURRENT = Path('/tmp/t3v-current')
EVIDENCE_ROOT = Path('/tmp/t3v-evidence')
BIN_ROOT = Path('/tmp/t3v-bin')
SESSION = 't3v'
# Files that only a run writes into its evidence directory. A directory that holds one already
# has a run's evidence, and a second run would mix its bytes into the first's.
EVIDENCE_FILES = ('events.jsonl', 'raw.ansi', 'session.cast', 'summary.json')
SUBDIRS = ('home/.t3/userdata', 'bin', 'logs', 'run', 'tmp')


def write_json(path: Path, value, mode: int = 0o600):
    partial = path.with_name(path.name + '.partial')
    fd = os.open(partial, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, mode)
    with os.fdopen(fd, 'w') as handle:
        json.dump(value, handle, indent=1)
        handle.write('\n')
    os.replace(partial, path)


def read_json(path: Path):
    try:
        return json.loads(Path(path).read_text())
    except (OSError, ValueError):
        return None


def python_command() -> list:
    """How the run starts its own helpers: this Python, isolated, running the t3v entry point."""
    return [sys.executable, '-I', str(SCRIPT)]


def run_shaped(root: Path) -> bool:
    """Whether `root` has a run directory's name and parent."""
    return root.name.startswith(RUN_PREFIX) and root.parent == TMP


def owned(root: Path, state) -> bool:
    """Whether `root` is a run directory t3v made: its name, its parent, and a marker whose nonce
    matches run.json's."""
    try:
        marker = (root / MARKER).read_text().strip()
    except OSError:
        return False
    return (run_shaped(root) and isinstance(state, dict) and state.get('root') == str(root)
            and hmac.compare_digest(marker, str(state.get('nonce', ''))))


def inspect(path) -> tuple:
    """(root, state, problem) for a directory that should be a run. `problem` is None when it is
    one, else what is wrong and what to do."""
    root = Path(os.path.realpath(os.path.expanduser(str(path))))
    if not root.is_dir():
        return root, None, f'no run at {root}; start one with `t3v start`'
    state = read_json(root / STATE)
    if not run_shaped(root):
        return root, state, f'{root} is not a t3v run directory ({TMP}/{RUN_PREFIX}XXXXXXXX); pass --run with one'
    if not owned(root, state):
        return root, state, f"{root}'s marker doesn't match its run.json, so t3v leaves it alone"
    return root, state, None


def run_argument(arg):
    """The run a command acts on: --run (or --dir), then $T3V_RUN, then /tmp/t3v-current."""
    if arg:
        return arg, '--run'
    if os.environ.get('T3V_RUN'):
        return os.environ['T3V_RUN'], '$T3V_RUN'
    if CURRENT.is_symlink():
        return os.readlink(CURRENT), str(CURRENT)
    die(f'no run at {CURRENT}; start one with `t3v start`, or pass --run DIR', 2)


def locate(arg) -> tuple:
    """(root, state) of the run a command acts on, or exits saying what to do."""
    path, source = run_argument(arg)
    root, state, problem = inspect(path)
    if problem:
        if source == str(CURRENT) and not root.exists():
            die(f'no run at {CURRENT}: the run it named is gone; start one with `t3v start`', 2)
        die(problem if source == '--run' else f'{problem} (from {source})', 2)
    return root, state


def evidence_of(state: dict) -> Path:
    return Path(state['evidence'])


def save_state(root: Path, state: dict):
    write_json(root / STATE, state)


def make_run() -> tuple:
    """A new run directory with its marker, control key and subdirectories. (root, nonce)."""
    root = Path(os.path.realpath(tempfile.mkdtemp(prefix=RUN_PREFIX, dir='/tmp')))
    if any(c.isspace() for c in str(root)):
        die(f'{root} has whitespace, which T3TERM_T3_COMMAND cannot carry', 1)
    nonce = secrets.token_hex(16)
    for sub in SUBDIRS:
        (root / sub).mkdir(parents=True, mode=0o700, exist_ok=True)
    (root / MARKER).write_text(nonce + '\n')
    fd = os.open(root / 'control.key', os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, 'w') as handle:
        handle.write(secrets.token_hex(32) + '\n')
    shim_path = root / 'bin' / 't3-shim'
    shim_path.write_text('#!/bin/sh\n' + ' '.join(shlex.quote(x) for x in (
        'exec', *python_command(), 't3-shim', '--run', str(root))) + ' "$@"\n')
    shim_path.chmod(0o700)
    return root, nonce


def make_evidence(path, root: Path) -> Path:
    """The evidence directory for run `root`, made if need be. It may exist, but not hold another
    run's evidence."""
    evidence = Path(os.path.realpath(os.path.expanduser(str(path)))) if path else EVIDENCE_ROOT / root.name
    if evidence == root or root in evidence.parents:
        die('--evidence must be outside the run directory, which cleanup deletes', 2)
    evidence.mkdir(parents=True, mode=0o700, exist_ok=True)
    held = [name for name in EVIDENCE_FILES if (evidence / name).exists()]
    if held:
        die(f'{evidence} already holds a run\'s evidence ({", ".join(held)}); pass another --evidence', 2)
    return evidence


def point_current(root: Path) -> str:
    """Links /tmp/t3v-current to `root`. Only a link to a run directory gives way, so a start
    never removes anything else. Returns a note when the link stays as it was."""
    if CURRENT.is_symlink():
        previous = Path(os.path.realpath(CURRENT))
        if not run_shaped(previous):
            return f'{CURRENT} links to {previous}, not to a run, so it stays; pass --run {root}'
        CURRENT.unlink()
    elif os.path.lexists(CURRENT):
        return f'{CURRENT} is not a link, so it stays; pass --run {root}'
    CURRENT.symlink_to(root)
    return ''


def forget_current(root: Path):
    if CURRENT.is_symlink() and os.path.realpath(CURRENT) == str(root):
        CURRENT.unlink()


def mark(state: dict, event: str, **fields):
    """Appends a helper event to the run's events.jsonl, in the server's line format."""
    stamp = now_ms()
    record = {'wall': iso(stamp), 'wallMs': stamp, 'event': event, 'by': 'helper'}
    world = read_json(Path(state['root']) / 'world.json')
    if world:
        record['sinceWorldMs'] = stamp - world['t0Ms']
    record.update({k: v for k, v in fields.items() if v is not None})
    fd = os.open(evidence_of(state) / 'events.jsonl', os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
    try:
        os.write(fd, (json.dumps(record) + '\n').encode())
    finally:
        os.close(fd)


def read_log(state: dict) -> list:
    records = []
    try:
        with open(evidence_of(state) / 'events.jsonl') as handle:
            for line in handle:
                try:
                    records.append(json.loads(line))
                except ValueError:
                    pass
    except OSError:
        pass
    return records


def command_of(pid) -> str:
    if not pid:
        return ''
    result = subprocess.run(['ps', '-o', 'command=', '-p', str(pid)], capture_output=True, text=True)
    return result.stdout.strip() if result.returncode == 0 else ''


def alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True


def server_needles(root: Path) -> list:
    """What the server's command line holds, so a pid that now runs something else is left alone."""
    return [str(SCRIPT), 'serve', '--run', str(root)]


def runs_command(pid, needles: list) -> bool:
    command = command_of(pid)
    return bool(command) and all(n in command for n in needles)


def terminate(pid, needles: list, label: str) -> str:
    """SIGTERM, then SIGKILL, a pid this run recorded, if it still runs what it ran."""
    if not pid:
        return f'{label}: no pid recorded'
    command = command_of(pid)
    if not command:
        return f'{label}: pid {pid} already gone'
    if not all(n in command for n in needles):
        return f'{label}: pid {pid} now runs something else, left alone'
    os.kill(pid, signal.SIGTERM)
    deadline = time.monotonic() + 6
    while time.monotonic() < deadline and alive(pid):
        time.sleep(0.05)
    if alive(pid) and all(n in command_of(pid) for n in needles):
        os.kill(pid, signal.SIGKILL)
        return f'{label}: pid {pid} needed SIGKILL'
    return f'{label}: pid {pid} stopped'


def runtime_file(root: Path) -> Path:
    return root / 'home' / '.t3' / 'userdata' / 'server-runtime.json'


def origin_of(root: Path):
    """The server's origin from the runtime file it writes once it listens, or None."""
    origin = str((read_json(runtime_file(root)) or {}).get('origin', ''))
    return origin if re.fullmatch(r'http://127\.0\.0\.1:\d+', origin) else None


def opener():
    # No proxy: the server is on loopback, and a proxy from the environment must not see it.
    return urllib.request.build_opener(urllib.request.ProxyHandler({}))


def control_post(root: Path, route: str, body: dict, quiet: bool = False):
    """POSTs to one of this run's control routes with its key, as the t3 shim does. Returns the
    status and the JSON answer. The key never reaches the output or the log. With `quiet`, a
    server that isn't there gives (None, {}) instead of exiting."""
    origin = origin_of(root)
    if origin is None:
        if quiet:
            return None, {}
        die("this run's server is not running; `t3v status` says why, `t3v start` starts a new run", 1)
    key = (root / 'control.key').read_text().strip()
    request = urllib.request.Request(origin + route, data=json.dumps(body).encode(), method='POST',
                                     headers={'Content-Type': 'application/json', 'X-Fixture-Control': key})
    try:
        with opener().open(request, timeout=10) as response:
            status, data = response.status, response.read()
    except urllib.error.HTTPError as error:
        status, data = error.code, error.read()
    except (urllib.error.URLError, OSError) as error:
        if quiet:
            return None, {}
        die(f"this run's server did not answer: {error}; `t3v doctor` checks it", 1)
    try:
        return status, json.loads(data or b'{}')
    except ValueError:
        return status, {}


def post(root: Path, route: str, body: dict) -> dict:
    """POSTs to a control route and returns its answer, or exits on a refusal."""
    status, answer = control_post(root, route, body)
    if status == 404:
        die(f"this run's scenario has no such control ({route}); `t3v --help` lists the commands", 1)
    if status != 200:
        die(f'refused: {answer.get("message")}', 1)
    return answer
