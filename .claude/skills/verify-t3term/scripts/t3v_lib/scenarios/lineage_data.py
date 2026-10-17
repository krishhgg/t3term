"""The `lineage` scenario's world: threads that fork, start agents and transfer context, in T3's V2
shapes at f570bd2. lineage.py describes it and serves it.
"""

from __future__ import annotations

import copy

from ..text import DAY, HOUR, MINUTE, SECOND, iso
from ..world import ACTIVE, INSTANCE, MODEL, Thread

PROJECT_ID = 'proj-lineage'
# ORCHESTRATION_V2_PROJECTION_SCHEMA_VERSION at f570bd2, which the shell snapshot carries.
SCHEMA_VERSION = 2

# A Subagent's status, a ContextTransfer's, and the run statuses T3 forks from
# (isForkableSourceRunStatus).
TASK_STATUSES = ('idle', 'pending', 'running', 'waiting', 'completed', 'failed', 'cancelled', 'interrupted')
TASK_ENDS = ('completed', 'failed', 'cancelled', 'interrupted')
TRANSFER_STATUSES = ('pending', 'resolved_native', 'resolved_portable', 'failed', 'consumed', 'superseded')
FORKABLE = ('completed', 'waiting', 'failed', 'interrupted', 'cancelled')
# Where `t3v agent` sends a change: the agent's task, its thread's run, or both.
AGENT_VIA = ('task', 'thread', 'both')

# ---- titles ----

TITLE_PRESETS = ('start', 'hostile', 'long', 'blank', 'null')
# What JavaScript's String.prototype.trim takes off, which T3's TrimmedNonEmptyString uses. It
# differs from Rust's: NEL (\x85) stays and U+FEFF goes.
JS_WHITESPACE = ('\t\n\x0b\x0c\r \xa0\u1680\u2000\u2001\u2002\u2003\u2004\u2005\u2006'
                 '\u2007\u2008\u2009\u200a\u2028\u2029\u202f\u205f\u3000\ufeff')
# A joined emoji without U+1F469 in it, so labelSplitEmoji counts only the hostile label's.
JOINED_SAFE = '\U0001f9d1\u200d\U0001f4bb'  # 🧑 ZWJ 💻
HOSTILE_TITLE = ('\t\x07Lineage \x1b[31mred \x1b]52;c;eA==\x07osc \x9b2J \x07bel \x85漢字 cafe\u0301 '
                 + JOINED_SAFE + '\r\nsecond line ' + 'w' * 260 + ' TITLE-PAST-CAP')
LONG_TITLE = 'Long ' + 'T' * 300 + ' TITLE-PAST-CAP ' + 'u' * 2000
BLANK_TITLE = ' \t\x07\r\n '
AGENT_RESULT = 'Invented result. Nothing here came from a real thread.'
# The Previous agents of thr-many that failed.
MANY_FAILED = (3, 21, 39, 57, 75, 93, 111)

# Text that only a title t3term should have cut could put in its output. rules.py adds the
# markers for control characters, which the hostile preset carries too.
LEAK_MARKERS = {'titlePastTheCap': b'TITLE-PAST-CAP'}
# It is plain text, which a screen capture can show.
SCREEN_MARKERS = tuple(LEAK_MARKERS)


def js_trim(text: str) -> str:
    """JavaScript's trim, which T3's TrimmedNonEmptyString applies to a thread's title."""
    return text.strip(JS_WHITESPACE)


def native_resolution(short: str) -> dict:
    """A native fork's resolution, with the strong provider thread ref T3 needs to resolve one."""
    return {'strategy': 'native_fork',
            'providerThreadRef': {'driver': 'codex', 'nativeId': f'pthr-{short}', 'strength': 'strong'}}


# ---- one thread ----


class LineageThread(Thread):
    """A thread with what Lineage reads on top of world.Thread's: who made it, its lineage, the
    agents it started and the context transfers that name it. Each builder changes the thread and
    returns the (event type, entity) pairs that carry the change, as T3 sends them."""

    def __init__(self, tid: str, project: dict, title: str, created: str, visited, by: str = 'user',
                 source: str = 'web'):
        super().__init__(tid, project, title, created, visited)
        # The text of prompts, replies and pulses names the thread by its id, so a title shows
        # only where t3term puts it, and `click --text` or `wait --text` on one finds no prompt.
        self.title = tid
        self.created = created
        self.subagents = []
        # The fields a ThreadShell and an app thread carry beyond world.Thread's.
        self.record.update(createdBy=by, creationSource=source, branch=None, latestVisibleMessage=None,
                           hasActionableProposedPlan=False, deletedAt=None)
        # The thread subscription's copy. It starts as the shell's, and a blank title from
        # `title` lets the two differ.
        self.watched = copy.deepcopy(self.record)

    def set(self, **fields) -> 'LineageThread':
        """Sets fields on both copies of the record."""
        for target in (self.record, self.watched):
            target.update(copy.deepcopy(fields))
        return self

    def shell(self) -> dict:
        """The shell's copy, with the item counts a ThreadShell carries."""
        return dict(copy.deepcopy(self.record), itemCount=len(self.items), visibleItemCount=len(self.items))

    def change(self, key: str, entity: dict) -> list:
        if key != 'subagents':
            return super().change(key, entity)
        index = next((i for i, e in enumerate(self.subagents) if e['id'] == entity['id']), None)
        if index is None:
            self.subagents.append(entity)
        else:
            self.subagents[index] = entity
        return [('subagent.updated', copy.deepcopy(entity))]

    def run_before(self, stamp: str):
        """The newest run asked for at or before `stamp`."""
        runs = [run for run in self.runs if run['requestedAt'] <= stamp]
        return max(runs, key=lambda run: run['ordinal'], default=None)

    def descend(self, parent: 'LineageThread', relationship: str, run=None) -> 'LineageThread':
        """Makes the thread a fork or an agent of `parent`, with the lineage T3 records. A fork
        names the run it came from: `run`, else the parent's newest run before the fork."""
        forked = None
        if relationship == 'fork':
            run = run or parent.run_before(self.created)
            if run is None:
                raise ValueError(f'{parent.tid} has no run before {self.tid}')
            forked = {'type': 'run', 'threadId': parent.tid, 'runId': run['id']}
        return self.set(lineage={'parentThreadId': parent.tid, 'relationshipToParent': relationship,
                                 'rootThreadId': parent.record['lineage']['rootThreadId']},
                        forkedFrom=forked)

    def delegated(self, aid: str, created: str, title, status: str, child, started=None, completed=None) -> list:
        """Agent `aid` of this thread, as the Subagent T3 records for an app-owned task. `child`
        is the agent's thread, or None before it has one."""
        run = self.run_before(created)
        agent = {'id': aid, 'threadId': self.tid, 'runId': run['id'] if run else None,
                 'parentNodeId': self.root(run['ordinal'] if run else 1), 'origin': 'app_owned',
                 'createdBy': 'agent', 'driver': 'codex', 'providerInstanceId': INSTANCE,
                 'providerThreadId': f'pthr-{child[len("thr-"):]}' if child else None,
                 'childThreadId': child, 'nativeTaskRef': None,
                 'prompt': (f'Fixture task {aid}: invented work for the Lineage fixture, and nothing here '
                            f'came from a real thread.'),
                 'title': title, 'model': MODEL, 'status': status,
                 'result': AGENT_RESULT if status == 'completed' else None,
                 'startedAt': started, 'completedAt': completed, 'updatedAt': completed or started or created}
        return self.change('subagents', agent)

    def projection(self, transfers=()) -> dict:
        """The thread's projection, with its agents and the context transfers that name it."""
        return dict(super().projection(), subagents=copy.deepcopy(self.subagents),
                    contextTransfers=copy.deepcopy(list(transfers)), updatedAt=self.watched['updatedAt'])


# ---- the invented world ----


def build_world(t0: int) -> dict:
    """Threads that fork, start agents and transfer context, in one project. Every time is an
    offset from t0, when the world was built."""
    def at(offset: int) -> str:
        return iso(t0 + offset)

    project = {'id': PROJECT_ID, 'title': 'Lineage Desk', 'workspaceRoot': '/fixture/lineage-desk',
               'repositoryIdentity': None, 'createdAt': at(-20 * DAY), 'updatedAt': at(-DAY), 'deletedAt': None}
    threads, order, agents, transfers, removed = {}, [], {}, [], set()

    def make(tid, title, created, by='user', source='web') -> LineageThread:
        thread = LineageThread(tid, project, title, at(created), at(0), by, source)
        threads[tid] = thread
        order.append(tid)
        return thread

    root = make('thr-root', 'Root: where it began', -6 * DAY)
    root.history(at, 2, -5 * DAY)
    archived = make('thr-archived', 'Archived: an old parent', -8 * DAY)
    archived.history(at, 1, -7 * DAY)
    main = make('thr-main', 'Main: open me first', -2 * DAY).descend(root, 'fork')
    main.history(at, 3, -40 * MINUTE, 10 * HOUR)
    merged = make('thr-merged', 'Merged: an archived fork', -12 * HOUR).descend(main, 'fork')
    merged.history(at, 1, -11 * HOUR)
    fork_a = make('thr-fork-a', 'Fork A: try another parser', -10 * HOUR).descend(main, 'fork')
    fork_a.history(at, 2, -8 * HOUR, HOUR)
    fork_b = make('thr-fork-b', 'Fork B: still running', -3 * HOUR).descend(main, 'fork')
    fork_b.prompt(1, at(-170 * MINUTE), 'running', started=at(-170 * MINUTE + SECOND))
    for k in range(1, 7):
        fork_b.reply(1, f'Fixture reply {k} on thr-fork-b: the invented agent is still working. '
                        f'Nothing here came from a real thread.', at(-170 * MINUTE + k * 5 * MINUTE))
    # Forked an hour ago and archived before its first run, so its transfer stays pending.
    dropped = make('thr-dropped', 'Dropped: forked, then archived', -HOUR).descend(main, 'fork')
    dropped.set(activeProviderThreadId=None)
    orphan = make('thr-orphan', 'Orphan: parent archived', -4 * DAY).descend(archived, 'fork')
    orphan.history(at, 1, -4 * DAY + HOUR)
    many = make('thr-many', 'Many: 134 agents', -5 * DAY)
    many.history(at, 2, -22 * HOUR, 2 * HOUR)
    make('thr-solo', 'Solo: no relations', -7 * DAY).history(at, 1, -6 * DAY)

    def delegate(parent: LineageThread, aid: str, child_id, title: str, minutes: int, status: str,
                 child_runs=()):
        """Agent `aid` of `parent`, started `minutes` ago, and its thread with a run for each
        status in `child_runs`, ten minutes apart."""
        created = -minutes * MINUTE
        started = None if status in ('idle', 'pending') else created + SECOND
        completed = started + 5 * MINUTE if status in TASK_ENDS else None
        if child_id is not None:
            child = make(child_id, title, created, 'agent', 'server').descend(parent, 'subagent')
            for n, run_status in enumerate(child_runs, 1):
                asked = created + (n - 1) * 10 * MINUTE + SECOND
                done = None if run_status in ACTIVE else at(asked + SECOND + 5 * MINUTE)
                child.prompt(n, at(asked), run_status, at(asked + SECOND), done)
                child.reply(n, f'Fixture reply {n} on {child_id}: invented agent work. Nothing here came from '
                               f'a real thread.', at(asked + 2 * SECOND))
        parent.delegated(aid, at(created), title, status, child_id, None if started is None else at(started),
                         None if completed is None else at(completed))
        agents[aid] = parent.tid

    delegate(main, 'ag-live', 'thr-agent-live', 'Subagent: Read the docs', 30, 'running', ('running',))
    delegate(main, 'ag-wait', 'thr-agent-wait', '/root/main/wait_for_review', 50, 'waiting', ('waiting',))
    # Its task ended, but its thread took more work.
    delegate(main, 'ag-resumed', 'thr-agent-resumed', 'Subagent: Resume the parser fix', 120, 'completed',
             ('completed', 'running'))
    delegate(main, 'ag-done', 'thr-agent-done', '/root/main/write_the_tests', 300, 'completed', ('completed',))
    delegate(main, 'ag-failed', 'thr-agent-failed', 'Subagent: Run the benchmark', 360, 'failed', ('failed',))
    delegate(main, 'ag-stopped', 'thr-agent-stopped', 'Subagent: Draft the changelog', 420, 'cancelled',
             ('cancelled',))
    delegate(main, 'ag-archived', 'thr-agent-archived', 'Subagent: Archived helper', 1200, 'completed',
             ('completed',))
    delegate(main, 'ag-nothread', None, 'Subagent: Not started yet', 5, 'running')
    for i in range(1, 9):
        status = 'running' if i <= 5 else 'waiting' if i <= 7 else 'pending'
        delegate(many, f'ag-many-a{i}', f'thr-many-a{i}', f'Subagent: Active part {i}', i, status,
                 () if status == 'pending' else (status,))
    for i in range(1, 125):
        status = 'failed' if i in MANY_FAILED else 'completed'
        title = f'/root/many/check_part_{i}' if i % 2 else f'Subagent: Review chunk {i}'
        delegate(many, f'ag-many-p{i:03d}', f'thr-many-p{i:03d}', title, i * 10, status, (status,))
    for i in (1, 2):
        delegate(many, f'ag-many-n{i}', None, f'Subagent: Waiting for a thread {i}', 3 + i, 'running')

    for thread in threads.values():
        thread.settle_record()
    # The active shell leaves an archived thread out.
    for tid, offset in (('thr-archived', -3 * DAY), ('thr-merged', -8 * HOUR - 50 * MINUTE),
                        ('thr-dropped', -50 * MINUTE), ('thr-agent-archived', -19 * HOUR)):
        threads[tid].set(archivedAt=at(offset), updatedAt=at(offset))
        removed.add(tid)

    def fork_transfer(xid: str, tid: str, status: str):
        """The transfer that made fork `tid`, as ThreadForkService plans it, then moved on to
        `status`."""
        target = threads[tid]
        forked = target.record['forkedFrom']
        transfers.append({
            'id': xid, 'type': 'fork', 'sourceThreadId': forked['threadId'], 'targetThreadId': tid,
            'sourcePoint': {'threadId': forked['threadId'], 'runId': forked['runId']}, 'basePoint': None,
            'sourceProviderInstanceId': INSTANCE, 'targetProviderInstanceId': None, 'targetRunId': None,
            'status': status, 'resolution': None if status == 'pending' else native_resolution(target.short),
            'createdBy': 'user', 'error': None, 'createdAt': target.created, 'updatedAt': target.created,
            'consumedAt': target.created if status == 'consumed' else None})

    # In rowid order, which the projection keeps.
    fork_transfer('xfer-archived-orphan', 'thr-orphan', 'consumed')
    fork_transfer('xfer-root-main', 'thr-main', 'consumed')
    fork_transfer('xfer-main-merged', 'thr-merged', 'consumed')
    fork_transfer('xfer-main-fork-a', 'thr-fork-a', 'consumed')
    # thr-merged merging back into thr-main, which never ran. Lineage shows the fork's transfer
    # for thr-merged, as the first edge to a thread speaks for it.
    transfers.append({
        'id': 'xfer-merged-main', 'type': 'merge_back', 'sourceThreadId': 'thr-merged', 'targetThreadId': 'thr-main',
        'sourcePoint': {'threadId': 'thr-merged', 'runId': merged.latest_run()['id']},
        'basePoint': copy.deepcopy(transfers[2]['sourcePoint']), 'sourceProviderInstanceId': INSTANCE,
        'targetProviderInstanceId': INSTANCE, 'targetRunId': None, 'status': 'pending', 'resolution': None,
        'createdBy': 'user', 'error': None, 'createdAt': at(-9 * HOUR), 'updatedAt': at(-9 * HOUR),
        'consumedAt': None})
    fork_transfer('xfer-main-fork-b', 'thr-fork-b', 'consumed')
    fork_transfer('xfer-main-dropped', 'thr-dropped', 'pending')
    return {'projects': [project], 'threads': threads, 'order': order, 'transfers': transfers, 'agents': agents,
            'removed': removed}
