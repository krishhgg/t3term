"""Binaries: which t3term a run starts, `t3v build` for any commit, and `t3v render`'s renderer.

Builds go to /tmp/t3v-bin/<sha>/t3term with a built.json beside it, from a temporary detached
worktree, so a build never touches your checkout or its branch. Every build shares
CARGO_TARGET_DIR=/tmp/t3v-target, so the second build of a nearby commit reuses the first's
dependencies. The renderer is render.swift beside the t3v entry point, built once per version of
its source into /tmp/t3v-bin/render-<12 hex of its sha256>.

t3v runs what is in /tmp/t3v-bin and builds in /tmp/t3v-target, so it uses either only when this
account owns it and no other can write to it (private_dir). Another account that made one first
can't hand it a binary.
"""

from __future__ import annotations

import fcntl
import hashlib
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from .runs import BIN_ROOT, SCRIPTS, private_dir, read_json, write_json
from .text import die, iso, now_ms

TARGET = Path('/tmp/t3v-target')
SHA = re.compile(r'^[0-9a-f]{40}$')


def git(repo, *args, check: bool = True) -> str:
    result = subprocess.run(['git', '-C', str(repo), *args], capture_output=True, text=True)
    if check and result.returncode != 0:
        die(f'git {args[0]} failed in {repo}: {result.stderr.strip()}', 1)
    return result.stdout.strip() if result.returncode == 0 else ''


def toplevel(path='.'):
    """The git toplevel of `path`, or None outside a checkout."""
    result = subprocess.run(['git', '-C', str(path), 'rev-parse', '--show-toplevel'], capture_output=True, text=True)
    return Path(result.stdout.strip()) if result.returncode == 0 and result.stdout.strip() else None


def version_of(binary: Path) -> str:
    """`t3term --version`, run with an empty environment and a HOME it can't write."""
    try:
        result = subprocess.run(['/usr/bin/env', '-i', 'HOME=/var/empty', 'T3CODE_HOME=/var/empty/.t3',
                                 'T3TERM_NO_SAVED_LOGIN=1', 'PATH=/usr/bin:/bin', str(binary), '--version'],
                                capture_output=True, text=True, timeout=10, stdin=subprocess.DEVNULL)
    except (OSError, subprocess.TimeoutExpired) as error:
        return f'unknown ({error})'
    return (result.stdout or result.stderr).strip() or f'unknown (exit {result.returncode})'


def image_uuid(binary: Path):
    """The binary's LC_UUID, which ld makes from a hash of the linked image. Two builds of the same
    sources in /tmp/t3v-target share it, and a binary copied from another commit's build carries
    that build's. None when otool can't read it."""
    try:
        result = subprocess.run(['otool', '-l', str(binary)], capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.TimeoutExpired):
        return None
    found = re.search(r'cmd LC_UUID\s+cmdsize \d+\s+uuid ([0-9A-F-]{36})', result.stdout)
    return found.group(1) if found else None


def describe_binary(path: Path) -> dict:
    """What run.json records of the binary a run starts."""
    stat = path.stat()
    value = {'path': str(path), 'size': stat.st_size, 'mtimeNs': stat.st_mtime_ns, 'mtime': iso(stat.st_mtime_ns // 10**6),
             'version': version_of(path), 'uuid': image_uuid(path), 'commit': None}
    built = read_json(path.parent / 'built.json')
    # By real path: --binary arrives resolved, and /tmp is a link to /private/tmp on macOS.
    in_bin_root = os.path.realpath(path.parent.parent) == os.path.realpath(BIN_ROOT)
    if in_bin_root and isinstance(built, dict) and SHA.match(str(built.get('sha'))):
        value['commit'] = built['sha']
        value['builtFrom'] = built.get('rev')
        return value
    top = toplevel(path.parent)
    if top is not None and (top / 'target') in path.parents:
        # A build in a checkout: its HEAD now, which the build matches only if nothing changed since.
        value['checkoutHead'] = git(top, 'rev-parse', 'HEAD', check=False) or None
        value['checkoutDirty'] = bool(git(top, 'status', '--porcelain', '--untracked-files=no', check=False))
    return value


def choose_binary(binary, rev, repo=None) -> Path:
    """The binary `t3v start` runs: --binary, the build of --rev (from `repo`), or
    target/debug/t3term under the git toplevel of the current directory."""
    if binary and rev:
        die('pass --binary or --rev, not both', 2)
    if rev:
        path = build(rev, repo)
    elif binary:
        path = Path(os.path.realpath(os.path.expanduser(binary)))
    else:
        top = toplevel()
        if top is None:
            die('no git checkout here to find target/debug/t3term in; pass --binary PATH or --rev REV', 2)
        path = top / 'target' / 'debug' / 't3term'
        if not path.exists():
            die(f'{path} is missing; run `cargo build` in {top}, or pass --binary PATH or --rev REV', 2)
    if not path.is_file() or not os.access(path, os.X_OK):
        die(f'{path} is not an executable t3term build; run `cargo build`, or pass --binary PATH', 2)
    return path


def build(rev: str, repo=None) -> Path:
    """Builds commit `rev` of `repo` once into /tmp/t3v-bin/<sha>/t3term and returns that path.
    Progress goes to stderr."""
    repo = Path(os.path.realpath(os.path.expanduser(repo))) if repo else toplevel()
    if repo is None or toplevel(repo) is None:
        die('no git checkout to build from; run this inside one or pass --repo PATH', 2)
    sha = git(repo, 'rev-parse', '--verify', '--end-of-options', f'{rev}^{{commit}}', check=False)
    if not SHA.match(sha):
        die(f'{rev} is not a commit in {repo}; `git -C {repo} fetch` it first', 2)
    out = private_dir(BIN_ROOT) / sha / 't3term'
    if out.is_file():
        return out
    if not shutil.which('cargo'):
        die('cargo is not on PATH; install Rust 1.95.0 (rustup) to build', 2)
    private_dir(TARGET)
    # One build at a time in the shared target directory, so a second build can't replace
    # target/debug/t3term before the first copies it. The lock comes before the checkout. Cargo
    # calls a crate fresh when its sources are older than its last build, and every build of
    # t3term shares one fingerprint here, so a tree checked out while another build ran would
    # look built already, and t3v would copy that other commit's binary.
    with open(TARGET / '.t3v-build.lock', 'w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if out.is_file():
            return out
        scratch = Path(os.path.realpath(tempfile.mkdtemp(prefix='t3v-build-', dir='/tmp')))
        tree = scratch / 'tree'
        print(f't3v: building {rev} ({sha[:12]}) in {tree}', file=sys.stderr)
        git(repo, 'worktree', 'add', '--detach', '--quiet', str(tree), sha)
        try:
            env = dict(os.environ, CARGO_TARGET_DIR=str(TARGET))
            cargo = subprocess.Popen(['cargo', 'build', '--locked'], cwd=tree, env=env, stdout=sys.stderr,
                                     stderr=subprocess.PIPE, stdin=subprocess.DEVNULL, text=True)
            compiled = False
            for line in cargo.stderr:
                sys.stderr.write(line)
                compiled = compiled or line.lstrip().startswith('Compiling t3term v')
            if cargo.wait() != 0:
                die(f'cargo build --locked failed at {sha[:12]}; its output is above', 1)
            if not compiled:
                die(f'cargo reused an earlier build instead of compiling {sha[:12]}, so t3v copied nothing; '
                    f'run `t3v build {sha[:12]}` again', 1)
            toolchain = subprocess.run(['rustc', '--version'], cwd=tree, capture_output=True, text=True)
            out.parent.mkdir(parents=True, exist_ok=True)
            partial = out.with_name(f't3term.partial-{os.getpid()}')
            shutil.copy2(TARGET / 'debug' / 't3term', partial)
            write_json(out.parent / 'built.json', {'sha': sha, 'rev': rev, 'toolchain': toolchain.stdout.strip(),
                                                   'builtAt': iso(now_ms()), 'repo': str(repo)}, mode=0o644)
            os.replace(partial, out)
        finally:
            subprocess.run(['git', '-C', str(repo), 'worktree', 'remove', '--force', str(tree)], capture_output=True)
            shutil.rmtree(scratch, ignore_errors=True)
    return out


def cmd_build(args) -> int:
    print(build(args.rev, args.repo))
    return 0


def renderer() -> Path:
    """render.swift's build for its current source, made once."""
    source = SCRIPTS / 'render.swift'
    try:
        digest = hashlib.sha256(source.read_bytes()).hexdigest()[:12]
    except OSError:
        die(f'{source} is missing; the renderer ships beside the t3v entry point', 1)
    out = private_dir(BIN_ROOT) / f'render-{digest}'
    if out.is_file():
        return out
    swiftc = shutil.which('swiftc')
    if swiftc is None:
        die('swiftc is not on PATH, so `t3v render` cannot build its renderer; install Xcode or its '
            'command line tools, or read the capture\'s .txt instead', 1)
    partial = out.with_name(f'{out.name}.partial-{os.getpid()}')
    print(f't3v: building the renderer into {out}', file=sys.stderr)
    # From /tmp/t3v-bin, so nothing swiftc leaves behind lands in the repository.
    result = subprocess.run([swiftc, '-O', '-o', str(partial), str(source)], cwd=BIN_ROOT, stdout=sys.stderr,
                            stdin=subprocess.DEVNULL)
    if result.returncode != 0:
        partial.unlink(missing_ok=True)
        die('swiftc could not build render.swift; its output is above', 1)
    os.replace(partial, out)
    return out
