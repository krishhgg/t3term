"""The `lineage` scenario: threads that fork, start agents and transfer context, for the Lineage
section at the end of the thread details (src/tui/lineage.rs and src/tui/details.rs).

What t3term reads. The details end with Lineage, after the facts and one blank row: the threads
related to the open one. A thread's parent comes from the run it was forked from, else its lineage.
Its forks and agents come from the shell's threads whose parent it is. Its agents also come from
its projection's subagents, and other related threads from its projection's context transfers,
which run from it or to it. The first edge to a thread speaks for it, so an agent whose thread the
shell lists shows its thread's run, and `t3v agent ID STATUS` changes its word only with --via
thread or both. Rows go in three groups: related threads, agents at work, and Previous agents,
which starts closed. The parent comes first, then the thread a fork merges back to, then the
newest. Each group shows 6 rows, then 12 more for each Show more. A row opens while the shell lists
the thread unarchived, and otherwise its title is muted and Enter says the thread is unavailable.
The heading counts agents running, and those without a thread yet while the watch can still hear
them end. Once the watch closes, an agent or transfer it last heard at work reads as unknown, and
the shell's copy speaks for each thread it lists. t3term asks T3 for nothing to show Lineage, so
choosing rows, paging and opening Previous agents leave rpcRequestsByTag as it was. Opening a
related thread adds one subscribeThread.

Where each change goes, as T3 at f570bd2 sends it. An agent's task goes on its parent's
subscription, as a subagent.updated. A run goes on its thread's own subscription, as run.created
or run.updated with no thread event, and the record on the shell. A context transfer goes on its
target's subscription only, so its source sees the change the next time t3term opens it. A
thread's record goes on the shell, as a thread.updated. The active shell leaves archived threads
out: `archive` sends a thread.archived on the thread's subscription, then a thread.removed on the
shell. The shell's snapshot carries schemaVersion 2 and an empty archivedThreads.

The project:

  proj-lineage      Lineage Desk      /fixture/lineage-desk

The threads, in the shell's order, with the agents and transfers that matter:

  thr-root          Root: where it began         2 runs done; thr-main forked from it
  thr-archived      Archived: an old parent      archived; thr-orphan forked from it
  thr-main          Main: open me first          forked from thr-root; 3 runs done; open it first
  thr-merged        Merged: an archived fork     forked from thr-main, archived; its merge back to
                                                 thr-main is pending, and never shows, as the
                                                 fork's transfer comes first
  thr-fork-a        Fork A: try another parser   forked from thr-main; 2 runs done
  thr-fork-b        Fork B: still running        forked from thr-main; run 1 running
  thr-dropped       Dropped: forked, then        forked from thr-main and archived before it ran;
                    archived                     its transfer xfer-main-dropped is pending
  thr-orphan        Orphan: parent archived      forked from thr-archived
  thr-many          Many: 134 agents             8 at work, 124 done of which 7 failed, and 2
                                                 running with no thread yet
  thr-solo          Solo: no relations           nothing related

thr-main's agents, each with its thread unless it says otherwise:

  ag-live           Subagent: Read the docs          running
  ag-wait           /root/main/wait_for_review       waiting, which reads Wait For Review
  ag-resumed        Subagent: Resume the parser fix  completed, but its thread runs again
  ag-done           /root/main/write_the_tests       completed
  ag-failed         Subagent: Run the benchmark      failed
  ag-stopped        Subagent: Draft the changelog    cancelled
  ag-archived       Subagent: Archived helper        completed; its thread is archived
  ag-nothread       Subagent: Not started yet        running, with no thread yet

Each agent's thread is thr-agent- and the rest of its id. thr-many's agents: ag-many-a1 to a8
(thr-many-a1 to a8), five running, two waiting and one pending; ag-many-p001 to p124, one every ten
minutes, the odd ones /root/many/check_part_N and the even ones Subagent: Review chunk N; and
ag-many-n1 and n2, running with no thread yet.

The transfers, in the order T3 made them: xfer-archived-orphan, xfer-root-main, xfer-main-merged
and xfer-main-fork-a (forks, consumed), xfer-merged-main (merge back, pending), xfer-main-fork-b
(consumed) and xfer-main-dropped (pending).

The sidebar lists neither archived threads nor agent threads (src/tui/sidebar.rs), so it shows Root,
Main, Fork A, Fork B, Orphan, Many and Solo. `t3v start` waits for Main's title.

What to look for. Open Main and its details: "Lineage · 3 running", then ↑ Root: where it began
(Done), ↳ Fork B: still running (Running), ↳ Fork A: try another parser (Done), ↳ thr-dropped
(Queued, title muted), ↳ thr-merged (Consumed, title muted), ◆ Read the docs (Running), ◆ Wait For
Review (Waiting), ◆ Resume the parser fix (Running), and "› Previous agents (4)" with a rule and "1
failed". The three running are Read the docs, Resume the parser fix and Not started yet, which has
no row. Many reads "Lineage · 7 running", Active part 1 to 6, "+ Show 2 more" and "› Previous agents
(124)" with "7 failed". Orphan reads "Lineage" over ↑ thr-archived (Unavailable, title muted), and
Solo has no Lineage. `t3v expect` prints the rows for the open thread, and `t3v lineage` prints them
with Previous agents open or more pages shown.

The title presets of `t3v title`: start (the world's title), hostile (a tab, bells, CR LF, an SGR,
an OSC 52 clipboard write, a C1 CSI and NEL, wide characters, a combining accent, a joined emoji and
text past the 256-character cut), long (over 2,000 characters), blank (white space around a bell)
and null (for agents). A thread's title is trimmed as T3's TrimmedNonEmptyString trims one and goes
to both copies, with a thread.metadata-updated on its subscription. One that trims to nothing goes to
the shell's copy alone, since T3 refuses it. An agent's goes as it is, null too, with a
subagent.updated. A listed thread's row takes the shell's title, so an agent's own title shows only
where the shell doesn't list its thread, as for Archived helper.

Where things are. Alt+B (tmux key M-b) opens and closes the details. They open as a 36-column pane
at the right when the main column has room, which 200x50 gives with the sidebar shown, so the pane
starts near column 163 and `click --from-col 160` keeps a click in it. Otherwise they open as a
dialog at the top right, up to 56 columns wide with a column showing at each side: at column 63 on
120x36, 23 on 80x24 and 1 on 40x12. A dialog takes the keyboard. In a pane, Escape gives the
transcript the keyboard and Tab moves it on to the pane. With the details' keyboard, Down chooses
the first Lineage row in view, then the next, and Up the one before. Enter acts on the chosen row:
it opens the thread, shows more of a group, or opens and closes Previous agents. A click on a row
chooses it and acts. The status line leads with what Enter does (lineage_keys in details.rs):
"Enter open parent thread", "Enter open fork", "Enter open subagent", "Context transfer
unavailable" and so on. A related thread opens in place of the open one, its title shows in the
breadcrumb, and the details keep the keyboard, with nothing chosen and Previous agents closed. A row
too narrow for its mark, 4 columns of title, a space and its word drops the word.
"""

from __future__ import annotations

import copy
from collections import Counter

from .. import controls
from ..orchestration import Orchestration, is_sequence
from ..runs import locate, post
from ..text import MISSING, die, field_summary, iso, now_ms, safe
from ..world import ACTIVE, RUN_STATUSES
from .lineage_data import (AGENT_RESULT, AGENT_VIA, BLANK_TITLE, FORKABLE, HOSTILE_TITLE, LEAK_MARKERS, LONG_TITLE,
                           SCHEMA_VERSION, SCREEN_MARKERS, TASK_ENDS, TASK_STATUSES, TITLE_PRESETS, TRANSFER_STATUSES,
                           LineageThread, build_world, js_trim, native_resolution)
from .lineage_expect import (GROUPS, PAGE, default_paging, lineage_of, print_agents, print_lineage, print_lineages,
                             print_thread_list, upsert)

NAME = 'lineage'
# The thread to open first, in the sidebar: `t3v start` returns once it shows.
READY = 'Main: open me first'
# What `t3v check` looks for on top of rules.py's markers.
LEAKS, SCREEN = LEAK_MARKERS, SCREEN_MARKERS


class LineageFixture(Orchestration):
    """The Lineage world on the orchestration feeds, with the controls that move its agents,
    transfers, forks, archives and titles."""

    def __init__(self, root, run):
        super().__init__(root, run)
        self.transfers, self.agents, self.seed_titles = [], {}, {}  # the world's, once it is built
        self.made = Counter()  # thread id -> the agents and forks `agent-add` and `fork` made of it
        # What t3term should show, worked out from what reached it.
        self.seen = {}  # thread id -> what its current watch holds, as far as Lineage reads it
        self.shell_view = None  # the shell's threads as t3term holds them, once a snapshot reached it
        self.shell_sequence = 0
        self.controls.update({
            '/__fixture/agent': lambda request, _: self.agent_action(request),
            '/__fixture/agent/add': lambda request, _: self.agent_add(request),
            '/__fixture/agent/link': lambda request, _: self.agent_link(request),
            '/__fixture/transfer': lambda request, _: self.transfer_action(request),
            '/__fixture/fork': lambda request, _: self.fork_action(request),
            '/__fixture/title': lambda request, _: self.title_action(request),
            '/__fixture/lineage': lambda request, _: self.lineage_read(request),
            '/__fixture/agents': lambda request, _: self.agents_read(request),
        })

    # -- world --

    def build_world(self, t0: int) -> dict:
        return build_world(t0)

    def world_built(self, world: dict):
        self.projects, self.threads, self.order = world['projects'], world['threads'], world['order']
        # `removed` holds the archived threads too, which the active shell leaves out.
        self.transfers, self.agents, self.removed = world['transfers'], world['agents'], world['removed']
        self.seed_titles = {tid: thread.record['title'] for tid, thread in self.threads.items()}
        self.seed_titles.update({aid: self.agent(aid)[1]['title'] for aid in self.agents})
        self.log('world.built', scenario=self.run.get('scenario'), threads=len(self.order), agents=len(self.agents),
                 transfers=len(self.transfers), **self.world_fields())

    def shell_threads(self) -> list:
        """The shell's threads: all but the archived and those `gone` took out."""
        return [self.threads[tid].shell() for tid in self.order if tid not in self.removed]

    def shell_snapshot(self) -> dict:
        with self.lock:
            self.ensure_world()
            # The active shell, which leaves archived threads to the archive's subscription.
            return {'schemaVersion': SCHEMA_VERSION, 'snapshotSequence': self.sequence,
                    'projects': copy.deepcopy(self.projects), 'threads': self.shell_threads(), 'archivedThreads': []}

    def push_shell(self, thread) -> int:
        """The sidebar's copy of the thread, as a thread.updated on the shell."""
        with self.lock:
            thread.record['updatedAt'] = iso(now_ms())
            return self.push_shell_event({'kind': 'thread.updated', 'location': 'active', 'thread': thread.shell()})

    def transfers_for(self, tid: str) -> list:
        """The transfers a thread's projection carries: those from it and those to it, in the
        order they were made."""
        return [copy.deepcopy(x) for x in self.transfers if tid in (x['sourceThreadId'], x['targetThreadId'])]

    def links(self, tid: str) -> dict:
        """What Lineage reads of thread `tid`'s projection, as the fake has it now."""
        thread = self.threads[tid]
        return {'thread': copy.deepcopy(thread.watched), 'subagents': copy.deepcopy(thread.subagents),
                'contextTransfers': self.transfers_for(tid)}

    def thread_snapshot(self, tid: str):
        with self.lock:
            self.ensure_world()
            thread = self.threads.get(tid)
            if thread is None:
                return None
            return {'snapshotSequence': self.sequence, 'projection': thread.projection(self.transfers_for(tid))}

    def agent(self, aid):
        """Agent `aid` and the thread that started it, or (None, None)."""
        parent = self.threads.get(self.agents.get(aid)) if isinstance(aid, str) else None
        found = next((a for a in parent.subagents if a['id'] == aid), None) if parent else None
        return (parent, found) if found else (None, None)

    def agent_or_refusal(self, aid):
        self.ensure_world()
        parent, agent = self.agent(aid)
        if agent is None:
            return None, None, f'no fixture agent {safe(aid, 60)}; `t3v agents THREAD` lists them'
        return parent, agent, None

    # -- what t3term should show --

    def expect(self) -> dict:
        with self.lock:
            return dict(super().expect(),
                        lineage=({tid: self.expected_lineage(tid) for tid in self.watches if tid in self.threads}
                                 if self.threads else {}))

    def expected_lineage(self, tid: str, paging=None) -> dict:
        """What the details' Lineage should show for thread `tid` while it is open, as Lineage::of
        works it out from what reached t3term: the shell's threads it holds and what its watch
        of `tid` holds. `paging` is how far each group was paged, from Paging::default, which
        each thread t3term opens starts with."""
        with self.lock:
            notes = []
            if self.shell_view is not None:
                threads = self.shell_view
            else:
                threads = self.shell_threads()
                notes.append("no shell snapshot has reached t3term, so this uses the shell's threads as the "
                             'fake has them now')
            watch = self.watches.get(tid)
            if watch is None:
                projection, closed = self.links(tid), False
                notes.append('t3term has no watch of this thread, so this is what it shows once it opens the '
                             'thread and the snapshot comes')
            else:
                projection, closed = self.seen.get(tid), watch == 'closed'
                if projection is None:
                    notes.append('no snapshot of the thread has reached t3term, so only the shell speaks for it')
                if closed:
                    notes.append('its watch closed, so an agent or transfer it last heard at work no longer '
                                 'counts as at work, and the shell speaks for each thread it lists')
            result = lineage_of(tid, threads, projection, closed, paging or default_paging())
            result['notes'] = notes
            return result

    def thread_entry(self, thread) -> dict:
        lineage = thread.record['lineage']
        entry = dict(super().thread_entry(thread), title=field_summary(thread.record.get('title', MISSING)),
                     parent=lineage['parentThreadId'], relationship=lineage['relationshipToParent'],
                     agents=len(thread.subagents), archived=thread.record['archivedAt'] is not None)
        if thread.watched.get('title') != thread.record.get('title'):
            entry['ownTitle'] = field_summary(thread.watched.get('title', MISSING))
        return entry

    def transfer_list(self) -> list:
        with self.lock:
            self.ensure_world()
            return [{'id': x['id'], 'type': x['type'], 'source': x['sourceThreadId'], 'target': x['targetThreadId'],
                     'status': x['status'], 'resolution': (x['resolution'] or {}).get('strategy')}
                    for x in self.transfers]

    def state(self) -> dict:
        with self.lock:
            return dict(super().state(), transfers=self.transfer_list())

    # -- threads --

    def set_run(self, thread, status):
        """Moves the thread's newest run to `status`, or asks for its first run in that status:
        the run's events on the thread's own subscription, the record on the shell. Returns a
        note, or None and why not."""
        if status not in RUN_STATUSES:
            return None, f'a run takes {", ".join(RUN_STATUSES)}'
        stamp = iso(now_ms())
        latest = thread.latest_run()
        if latest is None:
            ordinal = 1
            events = thread.prompt(1, stamp, status, None if status == 'starting' else stamp,
                                   None if status in ACTIVE else stamp)
        else:
            run = dict(latest, status=status)
            if status in ACTIVE:
                run['completedAt'] = None
                if status in ('running', 'waiting') and not run['startedAt']:
                    run['startedAt'] = stamp
            else:
                run['completedAt'] = stamp
                run['startedAt'] = run['startedAt'] or stamp
            ordinal = run['ordinal']
            events = thread.change('runs', run)
        thread.settle_record()
        # T3 sends no thread event for a run's change, only the run's.
        streams = self.publish(thread, events)
        listed = thread.tid not in self.removed
        if listed:
            self.push_shell(thread)
        return (f'{thread.tid} run {ordinal} is {status}, on {streams} thread stream(s)'
                + (' and the shell' if listed else '; the shell leaves the thread out')), None

    def thread_action(self, action: str, tid: str, request: dict):
        """`run` here starts a first run where the thread has none, and `back` unarchives. The
        rest are the shared actions and `archive`."""
        if action not in ('run', 'back'):
            return super().thread_action(action, tid, request)
        with self.lock:
            thread, refusal = self.thread_or_refusal(tid)
            if thread is None:
                return None, refusal
            if action == 'run':
                note, refusal = self.set_run(thread, request.get('status'))
                if note is None:
                    return None, refusal
            else:
                archived = thread.record['archivedAt'] is not None
                if tid not in self.removed and not archived:
                    return None, f'the shell already lists {tid}'
                note = ''
                if archived:
                    thread.set(archivedAt=None, updatedAt=iso(now_ms()))
                    streams = self.publish(thread, [('thread.unarchived', copy.deepcopy(thread.watched))])
                    note = f'unarchived {tid} on {streams} thread stream(s); '
                self.removed.discard(tid)
                shell = self.push_shell(thread)
                note += f'sent a thread.updated that lists {tid} on {shell} shell stream(s)'
            self.counts['thread.' + action] += 1
            self.log('thread.action', action=action, threadId=tid)
            return self.answer(note), None

    def other_thread_action(self, action: str, thread, request: dict):
        if action != 'archive':
            return None
        tid = thread.tid
        listed = tid not in self.removed
        if thread.record['archivedAt'] is not None:
            return None, f'{tid} is archived already; `t3v back {tid}` unarchives it'
        stale = request.get('staleCopy') is True
        if stale and not listed:
            return None, f'the shell leaves {tid} out, so it has no copy to leave stale'
        stamp = iso(now_ms())
        thread.set(archivedAt=stamp, updatedAt=stamp)
        streams = self.publish(thread, [('thread.archived', copy.deepcopy(thread.watched))])
        note = f'archived {tid} on {streams} thread stream(s); '
        if stale:
            # Not what T3 does: its active shell drops an archived thread.
            shell = self.push_shell(thread)
            note += f'sent the archived record as a thread.updated on {shell} shell stream(s), which keep it listed'
        elif listed:
            self.removed.add(tid)
            shell = self.push_shell_event({'kind': 'thread.removed', 'location': 'active', 'threadId': tid})
            note += f'sent a thread.removed on {shell} shell stream(s), as the active shell does'
        else:
            note += 'the shell left it out already'
        return note, None

    # -- agents, transfers and forks --

    def agent_thread(self, parent, child_id: str, title: str, status: str, stamp: str):
        """The thread of a new agent of `parent`, with a run when the agent's status is a run's:
        its creation on any stream already open for it, then the thread on the shell."""
        child = LineageThread(child_id, {'id': parent.record['projectId']}, title, stamp, None, 'agent', 'server')
        child.descend(parent, 'subagent')
        events = [('thread.created', copy.deepcopy(child.watched))]
        if status in RUN_STATUSES:
            events += child.prompt(1, stamp, status, None if status == 'starting' else stamp,
                                   None if status in ACTIVE else stamp)
            child.settle_record()
        self.threads[child_id] = child
        self.order.append(child_id)
        self.seed_titles[child_id] = title
        self.publish(child, events)
        self.push_shell(child)
        return child

    def agent_action(self, request: dict):
        """Moves agent `agentId` to `status`: its task on its parent's subscription, its thread's
        run on the thread's own subscription and the shell, or both."""
        with self.lock:
            parent, agent, refusal = self.agent_or_refusal(request.get('agentId'))
            if agent is None:
                return None, refusal
            aid, status, via = agent['id'], request.get('status'), request.get('via') or 'task'
            if via not in AGENT_VIA:
                return None, f'--via takes {", ".join(AGENT_VIA)}'
            if via != 'thread' and status not in TASK_STATUSES:
                return None, f'a task takes {", ".join(TASK_STATUSES)}'
            if via != 'task' and status not in RUN_STATUSES:
                return None, f'a run takes {", ".join(RUN_STATUSES)}'
            child = self.threads.get(agent['childThreadId']) if agent['childThreadId'] else None
            if via != 'task' and child is None:
                return None, f'{aid} has no thread yet; `t3v agent-link {aid}` gives it one'
            notes = []
            if via != 'thread':
                stamp = iso(now_ms())
                at_work = status not in ('idle', 'pending')
                changed = dict(agent, status=status, updatedAt=stamp,
                               startedAt=(agent['startedAt'] or stamp) if at_work else agent['startedAt'],
                               completedAt=stamp if status in TASK_ENDS else None,
                               result=AGENT_RESULT if status == 'completed' else None)
                streams = self.publish(parent, parent.change('subagents', changed))
                notes.append(f'{aid} is {status} on {streams} stream(s) of {parent.tid}')
            if via != 'task':
                note, _ = self.set_run(child, status)
                notes.append(note)
            self.counts['agent.change'] += 1
            self.log('agent.change', agentId=aid, threadId=parent.tid, status=status, via=via)
            return self.answer('; '.join(notes)), None

    def agent_add(self, request: dict):
        """A new agent of thread `threadId`, as T3 adds an app-owned task: its thread unless
        `noThread`, then the task on the parent's subscription."""
        with self.lock:
            parent, refusal = self.thread_or_refusal(request.get('threadId'))
            if parent is None:
                return None, refusal
            status = request.get('status') or 'pending'
            if status not in TASK_STATUSES:
                return None, f'an agent takes {", ".join(TASK_STATUSES)}'
            title = request.get('title')
            if title is not None and (not isinstance(title, str) or len(title) > 50000):
                return None, 'an agent title takes up to 50,000 characters'
            while True:
                self.made[parent.tid] += 1
                n = self.made[parent.tid]
                aid, child_id = f'ag-{parent.short}-new{n}', f'thr-{parent.short}-new{n}'
                if aid not in self.agents and child_id not in self.threads:
                    break
            if title is None:
                title = f'Subagent: Fixture helper {n}'
            stamp = iso(now_ms())
            child = None
            if request.get('noThread') is not True:
                child = self.agent_thread(parent, child_id, js_trim(title) or f'Fixture helper {n}', status, stamp)
            at_work = status not in ('idle', 'pending')
            events = parent.delegated(aid, stamp, title, status, child.tid if child else None,
                                      stamp if at_work else None, stamp if status in TASK_ENDS else None)
            self.agents[aid] = parent.tid
            self.seed_titles[aid] = title
            streams = self.publish(parent, events)
            self.counts['agent.add'] += 1
            self.log('agent.add', agentId=aid, threadId=parent.tid, status=status, child=child.tid if child else None,
                     titleLength=len(title))
            note = (f'{aid} of {parent.tid} is {status}, on {streams} stream(s) of {parent.tid}; '
                    + (f'its thread {child.tid} is on the shell' if child else 'it has no thread yet'))
            return self.answer(note), None

    def agent_link(self, request: dict):
        """Gives agent `agentId`, which has no thread yet, its thread: the thread on the shell,
        then the task that names it on the parent's subscription."""
        with self.lock:
            parent, agent, refusal = self.agent_or_refusal(request.get('agentId'))
            if agent is None:
                return None, refusal
            aid = agent['id']
            if agent['childThreadId'] is not None:
                return None, f'{aid} has the thread {agent["childThreadId"]} already'
            child_id = 'thr-' + aid[len('ag-'):]
            if child_id in self.threads:
                return None, f'{child_id} exists already'
            stamp = iso(now_ms())
            title = js_trim(agent['title'] or '') or f'Fixture helper for {aid}'
            child = self.agent_thread(parent, child_id, title, agent['status'], stamp)
            changed = dict(agent, childThreadId=child_id, providerThreadId=f'pthr-{child.short}', updatedAt=stamp)
            streams = self.publish(parent, parent.change('subagents', changed))
            self.counts['agent.link'] += 1
            self.log('agent.link', agentId=aid, threadId=parent.tid, child=child_id)
            return self.answer(f'{aid} has the thread {child_id}, on the shell and on {streams} stream(s) of '
                               f'{parent.tid}'), None

    def transfer_action(self, request: dict):
        """Moves transfer `transferId` to `status` on its target's subscription, the only one T3
        sends a transfer's events to. Its source sees the change when it subscribes again."""
        with self.lock:
            self.ensure_world()
            xid, status = request.get('transferId'), request.get('status')
            transfer = next((x for x in self.transfers if x['id'] == xid), None) if isinstance(xid, str) else None
            if transfer is None:
                return None, f'no fixture transfer {safe(xid, 60)}; `t3v threads` lists them'
            if status not in TRANSFER_STATUSES:
                return None, f'a transfer takes {", ".join(TRANSFER_STATUSES)}'
            stamp = iso(now_ms())
            short = transfer['targetThreadId'][len('thr-'):]
            resolution, error = None, None
            if status == 'resolved_native':
                resolution = native_resolution(short)
            elif status == 'resolved_portable':
                # The handoff is invented: the fake has no context handoffs.
                resolution = {'strategy': 'portable_context', 'contextHandoffId': f'handoff-{xid}'}
            elif status == 'consumed':
                resolution = transfer['resolution'] or native_resolution(short)
            elif status == 'failed':
                error = 'The fixture failed this transfer. Nothing real was transferred.'
            transfer.update(status=status, resolution=resolution, error=error, updatedAt=stamp,
                            consumedAt=(transfer['consumedAt'] or stamp) if status == 'consumed' else None)
            target = self.threads[transfer['targetThreadId']]
            streams = self.publish(target, [('context-transfer.updated', copy.deepcopy(transfer))])
            self.counts['transfer.change'] += 1
            self.log('transfer.change', transferId=xid, status=status, threadId=target.tid, streams=streams)
            return self.answer(f'{xid} is {status} on {streams} stream(s) of {target.tid}; '
                               f'{transfer["sourceThreadId"]} sees it when t3term next opens it'), None

    def fork_action(self, request: dict):
        """Forks thread `threadId` from its newest run that can fork, as ThreadForkService plans
        it: the fork on the shell, then its pending transfer on the fork's own subscription."""
        with self.lock:
            source, refusal = self.thread_or_refusal(request.get('threadId'))
            if source is None:
                return None, refusal
            run = max((r for r in source.runs if r['status'] in FORKABLE), key=lambda r: r['ordinal'], default=None)
            if run is None:
                return None, f'{source.tid} has no run to fork from; a run that is {", ".join(FORKABLE)} can fork'
            title = request.get('title')
            if title is not None and (not isinstance(title, str) or len(title) > 50000 or not js_trim(title)):
                return None, 'a fork title takes 1 to 50,000 characters, not all white space'
            while True:
                self.made[source.tid] += 1
                n = self.made[source.tid]
                tid, xid = f'thr-{source.short}-fork{n}', f'xfer-{source.short}-fork{n}'
                if tid not in self.threads:
                    break
            stamp = iso(now_ms())
            name = js_trim(title) if title is not None else f'{source.watched["title"]} fork'
            fork = LineageThread(tid, {'id': source.record['projectId']}, name, stamp, None)
            fork.set(**{key: source.watched[key] for key in ('branch', 'worktreePath', 'providerInstanceId',
                                                              'modelSelection', 'runtimeMode', 'interactionMode')})
            fork.set(activeProviderThreadId=None)
            fork.descend(source, 'fork', run)
            self.threads[tid] = fork
            self.order.append(tid)
            self.seed_titles[tid] = fork.record['title']
            self.publish(fork, [('thread.created', copy.deepcopy(fork.watched))])
            shell = self.push_shell(fork)
            transfer = {'id': xid, 'type': 'fork', 'sourceThreadId': source.tid, 'targetThreadId': tid,
                        'sourcePoint': {'threadId': source.tid, 'runId': run['id']}, 'basePoint': None,
                        'sourceProviderInstanceId': run['providerInstanceId'], 'targetProviderInstanceId': None,
                        'targetRunId': None, 'status': 'pending', 'resolution': None, 'createdBy': 'user',
                        'error': None, 'createdAt': stamp, 'updatedAt': stamp, 'consumedAt': None}
            self.transfers.append(transfer)
            self.publish(fork, [('context-transfer.created', copy.deepcopy(transfer))])
            self.counts['fork.made'] += 1
            self.log('fork.made', threadId=tid, source=source.tid, runId=run['id'], transferId=xid)
            return self.answer(f'forked {source.tid} run {run["ordinal"]} as {tid}, on {shell} shell stream(s), '
                               f'with the pending transfer {xid}; {source.tid} sees the fork through the shell'), None

    def title_action(self, request: dict):
        """A new title for a thread or an agent. A thread's goes to both copies of its record,
        trimmed as T3 trims one, with a thread.metadata-updated on its own subscription. One that
        trims to nothing T3 refuses, so it goes to the shell's copy alone. An agent's goes as it
        is, null too, with a subagent.updated on its parent's subscription."""
        with self.lock:
            self.ensure_world()
            target, preset = request.get('target'), request.get('preset')
            if preset is not None and preset not in TITLE_PRESETS:
                return None, f'--preset takes {", ".join(TITLE_PRESETS)}'
            if preset is None and (not isinstance(request.get('text'), str) or len(request['text']) > 50000):
                return None, 'title takes a text of up to 50,000 characters, or --preset'
            presets = {'hostile': HOSTILE_TITLE, 'long': LONG_TITLE, 'blank': BLANK_TITLE, 'null': None,
                       'start': self.seed_titles.get(target) if isinstance(target, str) else None}
            value = presets[preset] if preset is not None else request['text']
            what = f'the {preset} title' if preset else f'a title of {len(value)} characters'
            parent, agent = self.agent(target)
            if agent is not None:
                changed = dict(agent, title=value, updatedAt=iso(now_ms()))
                streams = self.publish(parent, parent.change('subagents', changed))
                note = f'{target} has {what}, on {streams} stream(s) of {parent.tid}'
            else:
                thread = self.threads.get(target) if isinstance(target, str) else None
                if thread is None:
                    return None, (f'no fixture thread or agent {safe(target, 60)}; `t3v threads` and `t3v agents` '
                                  f'list them')
                if value is None:
                    return None, 'a thread title is a string in T3 and on the shell; only an agent takes null'
                listed = thread.tid not in self.removed
                trimmed = js_trim(value)
                if trimmed:
                    thread.set(title=trimmed)
                    streams = self.publish(thread, [('thread.metadata-updated', copy.deepcopy(thread.watched))])
                    shell = self.push_shell(thread) if listed else 0
                    note = (f'{thread.tid} has {what}, on {streams} thread stream(s) and '
                            + (f'{shell} shell stream(s)' if listed else 'not the shell, which leaves it out'))
                else:
                    if not listed:
                        return None, f'the shell leaves {thread.tid} out, so a blank title has nowhere to go'
                    thread.record['title'] = value
                    shell = self.push_shell(thread)
                    note = (f'{thread.tid} has {what} on {shell} shell stream(s) alone. T3 refuses a '
                            f'title that trims to nothing, so this tries a shell that sends one anyway')
            self.counts['title.change'] += 1
            self.log('title.change', target=safe(target, 60), preset=preset,
                     length=len(value) if isinstance(value, str) else None)
            return self.answer(note), None

    def lineage_read(self, request: dict):
        """What a thread's Lineage should show with Previous agents open or closed and `more`
        pages of the groups it names. With no thread, the one t3term has open."""
        with self.lock:
            self.ensure_world()
            tid = request.get('threadId')
            if tid is None:
                tid = next((t for t in self.watches if t in self.threads), None)
                if tid is None:
                    return None, 'no thread is open in t3term, so name one'
            thread, refusal = self.thread_or_refusal(tid)
            if thread is None:
                return None, refusal
            more = request.get('more') or []
            if not isinstance(more, list) or any(group not in GROUPS for group in more):
                return None, f'--more takes {", ".join(GROUPS)}'
            paging = default_paging()
            paging['previous_open'] = request.get('previous') is True
            for group in more:
                paging['shown'][group] += PAGE
            return {'threadId': tid, 'lineage': self.expected_lineage(tid, paging)}, None

    def agents_read(self, request: dict):
        """The agents of thread `threadId`, without their prompts."""
        with self.lock:
            thread, refusal = self.thread_or_refusal(request.get('threadId'))
            if thread is None:
                return None, refusal
            out = []
            for agent in thread.subagents:
                child = self.threads.get(agent['childThreadId']) if agent['childThreadId'] else None
                run = child.latest_run() if child else None
                out.append({'agentId': agent['id'], 'status': agent['status'],
                            'title': field_summary(agent['title']), 'child': agent['childThreadId'],
                            'listed': child is not None and child.tid not in self.removed,
                            'childRun': f'run {run["ordinal"]} {run["status"]}' if run else None})
            return {'threadId': thread.tid, 'agents': out}, None

    # -- streams --

    def forget_watches(self):
        super().forget_watches()
        self.seen, self.shell_view, self.shell_sequence = {}, None, 0

    def subscribe_thread(self, connection, request_id: str, payload: dict):
        tid = str(payload.get('threadId'))
        with self.lock:
            self.ensure_world()
            if tid in self.threads and not is_sequence(payload.get('afterSequence')):
                # A new watch holds nothing yet, and t3term let go of any watch the fake closed,
                # as the shared code says.
                self.seen.pop(tid, None)
                for other, state in self.watches.items():
                    if other != tid and state == 'closed':
                        self.seen.pop(other, None)
            super().subscribe_thread(connection, request_id, payload)

    def on_chunk(self, connection, request_id: str, stream: str, values: list, delivered: bool, thread_id=None):
        with self.lock:
            if delivered and stream == 'shell':
                self.note_shell_view(values)
            elif delivered and stream == 'thread':
                self.note_seen(thread_id, values)
            super().on_chunk(connection, request_id, stream, values, delivered, thread_id)

    def note_shell_view(self, values: list):
        """Follows the shell's threads as t3term's ShellState holds them, from what reached it."""
        for value in values:
            if not isinstance(value, dict):
                continue
            kind = value.get('kind')
            if kind == 'snapshot':
                snapshot = value.get('snapshot')
                # An enrichment snapshot leaves the threads as they were.
                if isinstance(snapshot, dict) and 'resolvedRepositoryIdentityRoots' not in value:
                    threads = snapshot.get('threads')
                    self.shell_view = copy.deepcopy(threads) if isinstance(threads, list) else []
                    self.shell_sequence = snapshot.get('snapshotSequence') or 0
                continue
            if kind == 'synchronized' or self.shell_view is None:
                continue
            sequence = value.get('sequence') or 0
            if sequence <= self.shell_sequence:
                continue
            self.shell_sequence = sequence
            thread = value.get('thread')
            if kind == 'thread.updated' and thread is not None:
                if value.get('location') == 'archive':
                    gone = thread.get('id') if isinstance(thread, dict) else None
                    self.shell_view = [t for t in self.shell_view if t.get('id') != gone]
                else:
                    upsert(self.shell_view, copy.deepcopy(thread))
            elif kind == 'thread.removed':
                self.shell_view = [t for t in self.shell_view if t.get('id') != value.get('threadId')]

    def note_seen(self, tid: str, values: list):
        """Follows what t3term's ThreadState holds for thread `tid`, as far as Lineage reads it,
        from what reached it."""
        for value in values:
            if not isinstance(value, dict):
                continue
            kind = value.get('kind')
            if kind == 'snapshot':
                projection = value.get('projection')
                thread = projection.get('thread') if isinstance(projection, dict) else None
                if isinstance(thread, dict) and isinstance(thread.get('id'), str):
                    self.seen[tid] = {'sequence': value.get('snapshotSequence') or 0,
                                      'thread': copy.deepcopy(thread),
                                      'subagents': copy.deepcopy(projection.get('subagents')),
                                      'contextTransfers': copy.deepcopy(projection.get('contextTransfers'))}
                continue
            held = self.seen.get(tid)
            if kind != 'event' or held is None:
                continue
            sequence = value.get('sequence') or 0
            if sequence <= held['sequence']:
                continue
            held['sequence'] = sequence
            event = value.get('event')
            if not isinstance(event, dict) or 'payload' not in event:
                continue
            event_type, payload = str(event.get('type') or ''), copy.deepcopy(event['payload'])
            key = {'subagent.updated': 'subagents', 'context-transfer.created': 'contextTransfers',
                   'context-transfer.updated': 'contextTransfers'}.get(event_type)
            if event_type.startswith('thread.'):
                held['thread'] = payload
            elif key is not None:
                if not isinstance(held.get(key), list):
                    held[key] = []
                upsert(held[key], payload)

    def drop_streams(self, connection, request_id=None):
        with self.lock:
            if request_id is not None:
                # t3term let the thread go, on a switch, and what it held of it.
                streams = list(self.thread_streams) + [(c, r, t) for c, r, _, t in self.held_threads]
                for c, r, tid in streams:
                    if c is connection and r == request_id:
                        self.seen.pop(tid, None)
            super().drop_streams(connection, request_id)


# ---- the command line ----


def add_start_options(start):
    """The lineage scenario takes no `t3v start` options of its own."""


def options_of(args) -> dict:
    return {}


def print_expect(expect: dict):
    print_lineages(expect)
    controls.print_world_expect(expect)


def print_threads(answer: dict, everything: bool):
    """`t3v threads`: each thread's title, runs, parent, agents and streams, then the transfers."""
    print_thread_list(answer.get('threads', []), answer.get('transfers', []), everything)


def status_fields(records: list, live) -> dict:
    """The lineage scenario's part of `t3v status`."""
    def of(event: str) -> list:
        return [r for r in records if r.get('event') == event]

    return {
        'agentChanges': [f'{r.get("agentId")}:{r.get("status")}:{r.get("via")}' for r in of('agent.change')],
        'agentsAdded': [f'{r.get("agentId")}:{r.get("status")}' for r in of('agent.add')],
        'agentsLinked': [f'{r.get("agentId")}:{r.get("child")}' for r in of('agent.link')],
        'transferChanges': [f'{r.get("transferId")}:{r.get("status")}' for r in of('transfer.change')],
        'forksMade': [f'{r.get("source")}:{r.get("threadId")}' for r in of('fork.made')],
        'titleChanges': [f'{r.get("target")}:{r.get("preset") or r.get("length")}' for r in of('title.change')],
    }


def cmd_agent(args) -> int:
    controls.send(args, '/__fixture/agent', {'agentId': args.agent, 'status': args.status, 'via': args.via})
    return 0


def cmd_agent_add(args) -> int:
    controls.send(args, '/__fixture/agent/add', {'threadId': args.thread, 'status': args.status, 'title': args.title,
                                                 'noThread': args.no_thread})
    return 0


def cmd_agent_link(args) -> int:
    controls.send(args, '/__fixture/agent/link', {'agentId': args.agent})
    return 0


def cmd_transfer(args) -> int:
    controls.send(args, '/__fixture/transfer', {'transferId': args.transfer, 'status': args.status})
    return 0


def cmd_fork(args) -> int:
    controls.send(args, '/__fixture/fork', {'threadId': args.thread, 'title': args.title})
    return 0


def cmd_archive(args) -> int:
    controls.send(args, '/__fixture/thread/archive', {'threadId': args.thread, 'staleCopy': args.stale_copy})
    return 0


def cmd_title(args) -> int:
    if (args.text is None) == (args.preset is None):
        die('title takes exactly one of TEXT and --preset; put -- before a TEXT that starts with -', 2)
    controls.send(args, '/__fixture/title', {'target': args.target, 'text': args.text, 'preset': args.preset})
    return 0


def cmd_lineage(args) -> int:
    root, _ = locate(args.run)
    answer = post(root, '/__fixture/lineage', {'threadId': args.thread, 'previous': args.previous,
                                               'more': args.more or []})
    print_lineage(answer.get('lineage') or {}, str(answer.get('threadId')))
    return 0


def cmd_agents(args) -> int:
    root, _ = locate(args.run)
    print_agents(post(root, '/__fixture/agents', {'threadId': args.thread}))
    return 0


def add_commands(commands, run_option):
    """The lineage scenario's commands. `run_option` adds --run to a parser."""
    agent = commands.add_parser('agent', help="lineage: move an agent's task, its thread's run or both to a status")
    agent.add_argument('agent', help='an agent id such as ag-live')
    agent.add_argument('status', choices=sorted(set(TASK_STATUSES) | set(RUN_STATUSES)))
    agent.add_argument('--via', choices=AGENT_VIA, default='task',
                       help="task: a subagent.updated on the parent's subscription; thread: the agent thread's "
                            'run and the shell; both (default task)')
    agent.set_defaults(func=cmd_agent)

    agent_add = commands.add_parser('agent-add', help='lineage: a new agent of the thread, with its own thread')
    agent_add.add_argument('thread')
    agent_add.add_argument('--status', choices=TASK_STATUSES, default='pending')
    agent_add.add_argument('--title', help="the agent's title (default Subagent: Fixture helper N)")
    agent_add.add_argument('--no-thread', action='store_true', help='no thread yet; `t3v agent-link` gives it one')
    agent_add.set_defaults(func=cmd_agent_add)

    agent_link = commands.add_parser('agent-link', help='lineage: give an agent with no thread its thread')
    agent_link.add_argument('agent')
    agent_link.set_defaults(func=cmd_agent_link)

    transfer = commands.add_parser('transfer', help="lineage: move a context transfer to a status, on its target's "
                                                    'subscription only')
    transfer.add_argument('transfer', help='a transfer id such as xfer-main-dropped')
    transfer.add_argument('status', choices=TRANSFER_STATUSES)
    transfer.set_defaults(func=cmd_transfer)

    fork = commands.add_parser('fork', help='lineage: fork the thread from its newest run that can fork')
    fork.add_argument('thread')
    fork.add_argument('--title', help="the fork's title (default the thread's title and \" fork\")")
    fork.set_defaults(func=cmd_fork)

    archive = commands.add_parser('archive', help='lineage: archive the thread, with a thread.archived and then '
                                                  'a thread.removed on the shell')
    archive.add_argument('thread')
    archive.add_argument('--stale-copy', action='store_true',
                         help='send the archived record on the shell instead, which keeps it listed (not T3)')
    archive.set_defaults(func=cmd_archive)

    title = commands.add_parser('title', help='lineage: give a thread or an agent a new title')
    title.add_argument('target', help='a thread or agent id')
    title.add_argument('text', nargs='?', help='put -- before a text that starts with a dash')
    title.add_argument('--preset', choices=TITLE_PRESETS)
    title.set_defaults(func=cmd_title)

    lineage = commands.add_parser('lineage', help="lineage: what a thread's Lineage should show with some paging")
    lineage.add_argument('thread', nargs='?', help='default: the thread t3term has open')
    lineage.add_argument('--previous', action='store_true', help='with Previous agents open')
    lineage.add_argument('--more', action='append', choices=GROUPS,
                         help='one Show more on this group; give it again for each')
    lineage.set_defaults(func=cmd_lineage)

    agents = commands.add_parser('agents', help="lineage: the thread's agents and their threads")
    agents.add_argument('thread')
    agents.set_defaults(func=cmd_agents)
    for parser in (agent, agent_add, agent_link, transfer, fork, archive, title, lineage, agents):
        run_option(parser)
