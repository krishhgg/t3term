"""The `links` scenario: threads whose linked pull requests hold each kind of link the contract
allows, for the details' Pull requests tab and a link's page.

What t3term reads. The details' Pull requests tab lists the open thread's links from the record the
details already hold: the thread's own subscription's copy once it has sent a snapshot, while it
stays open, and the shell's copy before that and after it closes. The record's `pullRequests` holds
the links (packages/contracts/src/threadPullRequest.ts at f570bd2). A record without that field
falls back to its legacy `linkedPullRequest`, and an empty list means no links. t3term hides
`stack-dismissed` links, shows each host, repository and number once, reads the first 1,000 and
counts a link it can't read as unreadable. The environment must report `threadPullRequests: true`,
which t3term reads from the config subscription's descriptor, or from the descriptor it read at
start until the config arrives. t3term works the list out again only after a snapshot, a `thread.*`
event or the caught-up marker on the thread's subscription, or a shell event for the open thread,
and asks T3 for nothing to show it. So opening the tab, selecting, scrolling and resizing should
leave rpcRequestsByTag as it was.

A link's page reads the host through the fake: `pullRequests.detail` answers 1.5 s late, from the
link the request names (#42 as a host without a GitHub credential, #62 as one that can't find it);
`pullRequests.activity` 3 s late (#43 times out); `pullRequests.stack` 0.8 s late; the diff route
POST /api/pull-requests/diff at once, in two slices. `refresh` raises the refresh number
`pullRequests.subscribeRefreshes` sends, as T3 does after a turn, and `revise` changes what the next
read gets without one. `pullRequests.invalidate` raises it too, then answers at once, and the event
log keeps the link its reference names as `pr.invalidate`. A file of these names in the run directory changes the reads: stack-fail,
stack-slow (8 s), comments-fail, contents-fail, viewed-fail, viewed-short, diff-slow (4 s a slice),
diff-fail, diff-fail-rest, diff-ts (a TypeScript diff) and diff-threads (review threads on the
diff's lines).

The project:

  proj-links    Linked Desk    /fixture/linked-desk

The threads, in the shell's order. Each is in proj-links, with no worktree and the branch main.

  thr-mixed     Mixed: every state       the `mixed` links: a GitHub stack of 3 sent out of order,
                                         3 links chained by branch, open, draft, merged, closed,
                                         waiting, watched, conflicting and partial ones, a
                                         tombstone and a duplicate in another case
  thr-single    Single: one open link    the `open` preset, for `links`, `unlink` and `sync`
  thr-empty     Empty: list cleared      an empty list beside a legacy link, so no links
  thr-legacy    Legacy: one old link     no `pullRequests`, only a `linkedPullRequest`
  thr-none      None: no field           neither field
  thr-many      Many: 1,005 links        the `huge` preset
  thr-hostile   Hostile: text and sizes  the `hostile` preset
  thr-malformed Malformed: not a list    an object where `pullRequests` goes

thr-legacy's last reply, after its second turn's, is Markdown with code fences for
features/transcript.md: a four-backtick fence that holds a three-backtick one, a tilde fence, a
backtick run with text after it inside a fence, backticks that are inline code, and a run followed
by U+00A0 inside a fence.

The presets `t3v links THREAD PRESET` sets:

  mixed       what thr-mixed starts with
  open        #7, open, checks passing, approved, +42 -7
  draft       #7, a draft, checks pending, awaiting review
  merged      #7, merged
  closed      #7, closed
  waiting     #7 with a null snapshot, which the host hasn't synced
  watch       #7, open, which T3 watches for the agent
  partial     #7 without any optional field, #8 whose snapshot lacks its title, #9 without a
              number and #11 with 0 additions and no deletions
  native      a GitHub stack of #21, #22 and #23, sent top first
  derived     #31, #32 and #33 chained by branch, #34 and #35 sharing a head, #36 on that shared
              head, #37 and #38 whose branches make a cycle, and #39 on #37's head
  tombstone   #44, and #45 as stack-dismissed
  duplicates  #46 three times, its host and repository in other cases and with spaces, and #46 in
              another repository
  hostile     tabs, bells, CR LF, an SGR, an OSC 52 clipboard write, a C1 CSI and NEL, wide
              characters, a combining accent and a joined emoji, and a title, URL, repository,
              branch and author past their caps, then a link whose host is over 1,024 bytes and
              one whose stack id is
  huge        1,005 links, #1001 to #2005
  legacy      no list, one linkedPullRequest whose URL has a port
  empty       an empty list beside a linkedPullRequest
  none        neither field
  malformed   an object where the list goes

`--via` sends a change on the thread's own subscription (thread), as a thread.updated on the
shell's (shell), or on both (the default).

The scenario serves `orchestration.launchThread` as T3 does (launches.py), so `t3term launch` makes a
thread in Linked Desk with its first message and a run in `preparing`, and the sidebar and `t3term
threads` list it. `t3v launches refuse` fails later launches with T3's launch error, `t3v launches
drop` makes the thread and closes the socket without an answer, and `t3v launches accept` goes back.

Where things are. The details' button ` ◨ ` takes columns COLS-4 to COLS-2 of the top row, with ◨
at COLS-3, whenever the conversation's header has room for it. The details open as a 36-column pane
when the main column, less a column at each side, has 97 columns (60 for the conversation and one
of gap) and the screen has 15 rows, and as a dialog otherwise. The sidebar takes a quarter of the
screen, from 26 to 34 columns. So with the sidebar shown, 200x50 gives a pane, and 120x36, 80x24 and
40x12 give a dialog. A dialog sits at the top right under the title row, up to 56 columns wide.
Under the details' title, the tabs row reads "Thread  Pull requests 11", or "Thread  PRs 11" on a
card too narrow for that. A link's details show in the list's place under a row that reads
"← List" at the left and "4 of 11" at the right.

thr-none's last reply, after its second turn's, is a list for features/transcript.md whose rows
alternate emoji that end in U+FE0F with text. At 200x30 the thread runs past the transcript's top,
and a scroll of one row puts each emoji row where a row of text was.
"""

from __future__ import annotations

import copy
import json
import time

from .. import controls, launches
from ..launches import Launches
from ..orchestration import Orchestration
from ..text import MINUTE, iso, now_ms, safe
from ..world import INSTANCE
from .links_data import (CAPABILITIES, CAPABILITY_WORDS, FILE_TEXTS, FILES_VIEWED, HOST, LATE_TITLE, LEAK_MARKERS,
                         LINK_EVENTS, LINK_PRESETS, REPOSITORY, SCREEN_MARKERS, SYNC_STATES, VIA, activity_of,
                         build_world, detail_of, diff_of, link, link_preset, links_summary, person)
from .links_expect import expected_pulls, print_pulls

NAME = 'links'
# The first thread's title in the sidebar: `t3v start` returns once it shows.
READY = 'Mixed: every state'
# What `t3v check` looks for on top of rules.py's markers.
LEAKS, SCREEN = LEAK_MARKERS, SCREEN_MARKERS


def operation_error(operation: str, detail: str, **fields) -> dict:
    return dict({'_tag': 'PullRequestOperationError', 'operation': operation, 'detail': detail}, **fields)


class LinksFixture(Launches, Orchestration):
    """The linked pull requests world on the orchestration feeds, with the pull request reads and
    thread launches."""

    def __init__(self, root, run):
        super().__init__(root, run)
        self.init_launches()
        self.refresh_streams = []  # (connection, request id)
        # pullRequests.subscribeRefreshes's number. It starts at 1, as T3's does after its first
        # turn, so a subscription opens with it; at 0 T3 sends nothing until it changes.
        self.refresh_number = 1
        # Changes on the host that T3 heard nothing of, which only a read again sees.
        self.revision = 0
        self.capability = self.options.get('capability', 'on')  # what the descriptor and configs serve
        self.capability_read = None  # what the last descriptor read served
        self.capability_config = None  # what the last config t3term got served, None before any
        self.rpc.update({
            'pullRequests.detail': self.read_detail,
            'pullRequests.activity': self.read_activity,
            'pullRequests.stack': self.read_stack,
            'pullRequests.threadComments': self.read_thread_comments,
            'pullRequests.diffFileContents': self.read_file_contents,
            'pullRequests.filesViewed': self.read_files_viewed,
            'pullRequests.invalidate': self.invalidate,
            'pullRequests.subscribeRefreshes': self.subscribe_refreshes,
        })
        self.controls.update({
            '/__fixture/links/': lambda request, kind: self.links_action(kind, request.get('threadId'), request),
            '/__fixture/capability': lambda request, _: self.change_capability(str(request.get('value') or '')),
            '/__fixture/refresh': lambda request, _: self.refresh(),
            '/__fixture/revise': lambda request, _: self.revise(),
        })

    # -- world and descriptor --

    def build_world(self, t0: int) -> dict:
        return build_world(t0)

    def world_fields(self) -> dict:
        return dict(super().world_fields(), capability=self.capability)

    def capabilities(self) -> dict:
        capabilities = super().capabilities()
        if self.capability != 'missing':
            capabilities['threadPullRequests'] = self.capability == 'on'
            capabilities['pullRequests'] = self.capability == 'on'
        return capabilities

    def note_descriptor_read(self):
        with self.lock:
            self.capability_read = self.capability
            super().note_descriptor_read()

    def config_delivered(self):
        self.capability_config = self.capability

    def forget_watches(self):
        super().forget_watches()
        self.capability_config = None

    # -- what t3term should show --

    def capability_held(self):
        """The capability t3term should hold, and where from: the last config it got, else the
        descriptor it read at start, as App::links_supported reads them."""
        if self.capability_config is not None:
            return self.capability_config, 'the last config t3term got'
        if self.capability_read is not None:
            return self.capability_read, 'the descriptor t3term read at start, as no config has come'
        return None, None

    def expect(self) -> dict:
        with self.lock:
            held, held_from = self.capability_held()
            return dict(super().expect(), capabilityServed=self.capability, capabilityHeld=held,
                        capabilityFrom=held_from, launchMode=self.launch_mode,
                        pulls=({tid: self.pulls_for(tid, held == 'on') for tid in self.watches if tid in self.threads}
                               if self.threads else {}))

    def pulls_for(self, tid: str, supported: bool) -> dict:
        """What the Pull requests tab should show for thread `tid` while it is open, from the
        copy of its record t3term should use, as App::details_record picks it."""
        with self.lock:
            thread = self.threads[tid]
            closed = self.watches.get(tid) == 'closed'
            projection = tid in self.snapshots
            listed = tid not in self.removed
            if projection and not closed:
                record, source = thread.watched, "its own subscription's copy"
            elif listed:
                record = thread.record
                source = ("the shell's copy, as its own subscription closed" if closed
                          else "the shell's copy, until its own subscription sends a snapshot")
            elif projection:
                record, source = thread.watched, "the last copy its own subscription sent, as the shell left it out"
            else:
                why = 'its subscription ended first' if closed else 'nothing has come yet'
                if not supported:
                    return dict(expected_pulls({}, False, 0), source=f'neither copy, as {why}')
                word = 'Unavailable' if closed or tid in self.synced else 'Loading…'
                return {'tab': 'Pull requests', 'message': [word], 'source': f'neither copy, as {why}'}
            return dict(expected_pulls(record, supported, now_ms()), source=source)

    def thread_entry(self, thread) -> dict:
        return dict(super().thread_entry(thread), shell=links_summary(thread.record),
                    own=links_summary(thread.watched))

    # -- controls --

    def other_thread_action(self, action: str, thread, request: dict):
        if action != 'late':
            return None
        tid = thread.tid
        targets = [(c, r) for c, r, t in self.interrupted if t == tid and c.open]
        delivered = 0
        for connection, request_id in targets:
            self.sequence += 1
            record = copy.deepcopy(thread.watched)
            record['pullRequests'] = [link(7, lambda offset: iso(now_ms() + offset), LATE_TITLE)]
            value = {'kind': 'event', 'sequence': self.sequence, 'event': {
                'id': f'evt-fixture-late-{self.sequence}', 'threadId': tid,
                'providerInstanceId': INSTANCE, 'occurredAt': iso(now_ms()),
                'type': 'thread.metadata-updated', 'payload': record}}
            # Straight to the socket: neither the replay nor what t3term should show takes it in.
            if connection.send_json({'_tag': 'Chunk', 'requestId': request_id, 'values': [value]}):
                delivered += 1
        self.counts['late.sent'] += delivered
        self.log('late.sent', threadId=tid, streams=len(targets), delivered=delivered)
        note = (f'wrote a thread.metadata-updated whose one link is titled {LATE_TITLE} to {delivered} of '
                f'{len(targets)} interrupted subscribeThread stream(s) for {tid}'
                + ('' if targets else '; open another thread first, so t3term interrupts this one'))
        return note, None

    def links_action(self, kind: str, tid: str, request: dict):
        """Sets a thread's links to a preset, or unlinks or syncs one, on the copies `via` names,
        and sends each copy's change: a thread.metadata-updated, or the event asked for, on the
        thread's own subscription, and a thread.updated on the shell. A sync goes on both, as a
        thread.pull-request-synced."""
        with self.lock:
            thread, refusal = self.thread_or_refusal(tid)
            if thread is None:
                return None, refusal
            via = request.get('via') or 'both'
            if via not in VIA:
                return None, f'--via takes {", ".join(VIA)}'
            event = 'metadata-updated'
            if kind == 'set':
                preset = request.get('preset')
                if preset not in LINK_PRESETS:
                    return None, f'links takes {", ".join(LINK_PRESETS)}'
                event = request.get('event') or event
                if event not in LINK_EVENTS:
                    return None, f'--event takes {", ".join(LINK_EVENTS)}'
                start = now_ms()
                thread.set_fields(link_preset(preset, lambda offset: iso(start + offset)), via)
                change = f'links set to the {preset} preset'
            elif kind in ('unlink', 'sync'):
                number = request.get('number')
                if isinstance(number, bool) or not isinstance(number, int) or number < 1:
                    return None, f'{kind} takes a pull request number of 1 or more'
                if kind == 'unlink':
                    counts = thread.unlink(number, via)
                    change = f'unlinked #{number}'
                else:
                    state = request.get('state')
                    if state not in SYNC_STATES:
                        return None, f'sync takes {", ".join(SYNC_STATES)}'
                    via, event = 'both', 'pull-request-synced'
                    counts = thread.sync(number, state, iso(now_ms()))
                    change = f'synced #{number} as {state}'
                if not any(counts.values()):
                    return None, f'{tid} has no link #{number} on the copies {kind} changes; `t3v threads` lists them'
                change += ' (' + ', '.join(f'{n} on the {name} copy' for name, n in counts.items()) + ')'
            else:
                return None, f'no links action {safe(kind, 30)}'
            streams = 0
            if via != 'shell':
                streams = self.publish(thread, [(f'thread.{event}', copy.deepcopy(thread.watched))])
            listed = tid not in self.removed
            shell = 0
            if via != 'thread' and listed:
                shell = self.push_shell(thread)
            self.counts[f'links.{kind}'] += 1
            # `log` takes the record's name as `event`, so the thread stream's event type goes
            # in `threadEvent`.
            self.log('links.change', kind=kind, threadId=tid, via=via,
                     threadEvent=f'thread.{event}' if via != 'shell' else None,
                     shellEvent='thread.updated' if via != 'thread' and listed else None,
                     own=links_summary(thread.watched), shell=links_summary(thread.record),
                     threadStreams=streams, shellStreams=shell)
            sent = []
            if via != 'shell':
                sent.append(f'a thread.{event} on {streams} thread stream(s)')
            if via != 'thread':
                sent.append(f'a thread.updated on {shell} shell stream(s)' if listed
                            else 'not the shell, which leaves the thread out')
            return self.answer(f'{tid}: {change}, sent as {" and ".join(sent)}'), None

    def change_capability(self, value: str):
        """Serves threadPullRequests true, false or not at all, in the descriptor and in a config
        snapshot to each open config subscription."""
        with self.lock:
            if value not in CAPABILITIES:
                return None, f'capability takes {", ".join(CAPABILITIES)}'
            self.ensure_world()
            self.capability = value
            pushed = self.push_config()
            self.counts['capability.change'] += 1
            self.log('capability.change', value=value, configStreams=pushed)
            note = f'the descriptor and {pushed} config subscription(s) now carry {CAPABILITY_WORDS[value]}'
            if not pushed:
                note += '; t3term keeps what it holds until a config subscription or a restart brings it'
            return self.answer(note), None

    def refresh(self):
        with self.lock:
            self.refresh_number += 1
            for connection, request_id in list(self.refresh_streams):
                connection.chunk(request_id, 'refreshes', [self.refresh_number])
            sent = len(self.refresh_streams)
            return {'number': self.refresh_number, 'streams': sent,
                    'note': f'refresh number {self.refresh_number}, sent to {sent} stream(s)'}, None

    def revise(self):
        with self.lock:
            self.revision += 1
            return {'revision': self.revision, 'note': f'revision {self.revision}, no refresh number sent'}, None

    # -- streams --

    def on_chunk(self, connection, request_id: str, stream: str, values: list, delivered: bool, thread_id=None):
        if stream != 'refreshes':
            return super().on_chunk(connection, request_id, stream, values, delivered, thread_id)
        with self.lock:
            self.counts['pr.refresh_chunk'] += 1
            self.log('pr.refresh_chunk', conn=connection.id, requestId=request_id, values=values, delivered=delivered)

    def drop_streams(self, connection, request_id=None):
        with self.lock:
            self.refresh_streams = [(c, r) for c, r in self.refresh_streams
                                    if not (c is connection and (request_id is None or r == request_id))]
            super().drop_streams(connection, request_id)

    # -- pull request reads --

    def invalidate(self, connection, request_id: str, payload: dict):
        # As T3's handler does with notifyReaders: forget, raise the number on every
        # subscription, then answer (apps/server/src/ws.ts:2506).
        reference = payload.get('reference')
        reference = reference if isinstance(reference, dict) else {}
        self.log('pr.invalidate', conn=connection.id, requestId=request_id, number=reference.get('number'),
                 host=reference.get('host'), repository=str(reference.get('repository', '')).strip().lower(),
                 projectId=reference.get('projectId'))
        with self.lock:
            self.counts['pr.invalidate'] += 1
            self.refresh_number += 1
            for other, stream_id in list(self.refresh_streams):
                other.chunk(stream_id, 'refreshes', [self.refresh_number])
        connection.exit_success(request_id, None)

    def subscribe_refreshes(self, connection, request_id: str, payload: dict):
        with self.lock:
            connection.streams[request_id] = 'pullRequests.subscribeRefreshes'
            self.refresh_streams.append((connection, request_id))
            self.counts['pr.refresh_subscribe'] += 1
            if self.refresh_number > 0:
                connection.chunk(request_id, 'refreshes', [self.refresh_number])

    def flag(self, name: str) -> bool:
        """Whether the run directory holds the file `name`, which changes a read."""
        return (self.root / name).exists()

    def read_detail(self, connection, request_id: str, payload: dict):
        number = payload.get('number')
        repository = str(payload.get('repository', '')).strip().lower()
        with self.lock:
            self.counts['pr.detail'] += 1
        self.log('pr.detail', conn=connection.id, requestId=request_id, number=number,
                 host=payload.get('host'), repository=repository, projectId=payload.get('projectId'))

        def answer():
            with self.lock:
                found = None
                for thread in self.threads.values():
                    for one in thread.record.get('pullRequests') or []:
                        if (isinstance(one, dict) and one.get('number') == number
                                and str(one.get('repository', '')).strip().lower() == repository):
                            found = one
                            break
                    if found:
                        break
            if number == 42:
                connection.fail(request_id, 'pullRequests.detail', {
                    '_tag': 'PullRequestUnavailableError', 'reason': 'cli-missing', 'provider': 'github'})
                return
            if found is None or number == 62:
                connection.fail(request_id, 'pullRequests.detail', operation_error(
                    'detail', f'Pull request #{number} was not found.', reason='not-found'))
                return
            detail = detail_of(found, payload)
            detail['updatedAt'] = f'2026-10-14T08:{min(self.refresh_number + self.revision, 59):02d}:00Z'
            connection.exit_success(request_id, detail)

        self.later(1.5, answer)

    def read_stack(self, connection, request_id: str, payload: dict):
        """A PullRequestStack built from the link's saved native stack, with each layer's title and
        draft flag from the links' snapshots, or null for a link in none. The file `stack-fail` in the
        run directory makes it fail, as a host that times out does, and the file `stack-slow` makes
        it answer after 8 seconds."""
        number = payload.get('number')
        repository = str(payload.get('repository', '')).strip().lower()
        with self.lock:
            self.counts['pr.stack'] += 1
        self.log('pr.stack', conn=connection.id, requestId=request_id, number=number,
                 host=payload.get('host'), repository=repository, projectId=payload.get('projectId'))

        def answer():
            with self.lock:
                found, snapshots = None, {}
                for thread in self.threads.values():
                    for one in thread.record.get('pullRequests') or []:
                        if not isinstance(one, dict):
                            continue
                        if str(one.get('repository', '')).strip().lower() != repository:
                            continue
                        if isinstance(one.get('snapshot'), dict):
                            snapshots.setdefault(one.get('number'), one['snapshot'])
                        if found is None and one.get('number') == number:
                            found = one
            if self.flag('stack-fail'):
                connection.fail(request_id, 'pullRequests.stack', operation_error(
                    'stack', 'GitHub took too long to answer.', reason='unknown'))
                return
            stack = (found or {}).get('stack')
            if not isinstance(stack, dict) or stack.get('kind') != 'native':
                connection.exit_success(request_id, None)
                return
            layers = []
            for layer in stack['layers']:
                snapshot = snapshots.get(layer['number']) or {}
                value = {'number': layer['number'], 'headBranch': layer['headBranch'], 'state': layer['state'],
                         'headSha': '9dc565a0'}
                if snapshot.get('title') is not None:
                    value['title'] = snapshot['title']
                if 'isDraft' in snapshot:
                    value['isDraft'] = snapshot['isDraft']
                layers.append(value)
            connection.exit_success(request_id, {'id': stack['id'], 'number': stack['number'], 'url': stack['url'],
                                                 'base': stack['base'], 'layers': layers})

        self.later(8.0 if self.flag('stack-slow') else 0.8, answer)

    def read_thread_comments(self, connection, request_id: str, payload: dict):
        """`pullRequests.threadComments`: #7's d-check thread has one more page, its last two remarks,
        1.5 seconds late so `Loading more comments…` shows. The file `comments-fail` in the run
        directory fails it as a host that times out does."""
        thread, cursor = payload.get('threadId'), payload.get('cursor')
        with self.lock:
            self.counts['pr.threadComments'] += 1
        self.log('pr.threadComments', conn=connection.id, requestId=request_id, number=payload.get('number'),
                 thread=safe(thread), cursor=safe(cursor))

        def answer():
            if self.flag('comments-fail') or (thread, cursor) != ('d-check', 'd-check-p2'):
                detail = 'GitHub took too long to answer.'
                connection.fail(request_id, 'pullRequests.threadComments', operation_error(
                    'threadComments', detail, message=detail, reason='timeout'))
                return

            def comment(id_, who, minutes, body):
                return {'id': id_, 'author': person(who), 'body': body,
                        'createdAt': iso(now_ms() - int(minutes * MINUTE)),
                        'url': f'https://{HOST}/{REPOSITORY}/pull/7#{id_}'}
            # d3 repeats the activity's last remark, which t3term shows once.
            comments = [comment('d3', 'fixture-dev', 60, 'It does now: `reload` goes through `load`.'),
                        comment('d6', 'fixture-reviewer', 50, 'Thanks. Could the test cover a reload too?'),
                        comment('d7', 'fixture-dev', 40, 'Added `a_reload_checks_the_schema`.')]
            connection.exit_success(request_id, {'comments': comments, 'nextCursor': None})

        self.later(1.5, answer)

    def read_file_contents(self, connection, request_id: str, payload: dict):
        """`pullRequests.diffFileContents`: the whole text of a file of #7's diff, 1.5 seconds late
        so `Reading…` shows. A file the fixture has no text for, or every file while
        `contents-fail` is in the run directory, fails as a host that can't read it does."""
        path = payload.get('newPath')
        with self.lock:
            self.counts['pr.diffFileContents'] += 1
        self.log('pr.diffFileContents', conn=connection.id, requestId=request_id, number=payload.get('number'),
                 commit=safe(payload.get('commit')), changeType=safe(payload.get('changeType')),
                 oldPath=safe(payload.get('oldPath')), newPath=safe(path))

        def answer():
            text = FILE_TEXTS.get(path)
            if self.flag('contents-fail') or text is None:
                detail = 'This host cannot expand unchanged pull request lines.'
                connection.fail(request_id, 'pullRequests.diffFileContents', operation_error(
                    'diffFileContents', detail, message=detail))
                return
            connection.exit_success(request_id, {'oldContents': '', 'newContents': text})

        self.later(1.5, answer)

    def read_files_viewed(self, connection, request_id: str, payload: dict):
        """`pullRequests.filesViewed`: the reader's marks on #7's files, a second late. With
        `viewed-fail` in the run directory the read fails, and with `viewed-short` it says the host
        had more files than it read."""
        with self.lock:
            self.counts['pr.filesViewed'] += 1
        self.log('pr.filesViewed', conn=connection.id, requestId=request_id, number=payload.get('number'),
                 host=payload.get('host'), projectId=payload.get('projectId'))

        def answer():
            if self.flag('viewed-fail'):
                detail = 'GitHub took too long to answer.'
                connection.fail(request_id, 'pullRequests.filesViewed', operation_error(
                    'filesViewed', detail, message=detail, reason='timeout'))
                return
            files = FILES_VIEWED if payload.get('number') == 7 else []
            connection.exit_success(request_id, {'files': copy.deepcopy(files), 'truncated': self.flag('viewed-short')})

        self.later(1.0, answer)

    def read_activity(self, connection, request_id: str, payload: dict):
        number = payload.get('number')
        with self.lock:
            self.counts['pr.activity'] += 1
        self.log('pr.activity', conn=connection.id, requestId=request_id, number=number,
                 host=payload.get('host'), projectId=payload.get('projectId'))

        def answer():
            if number == 43:
                detail = 'GitHub took too long to answer.'
                connection.fail(request_id, 'pullRequests.activity', operation_error(
                    'activity', detail, message=detail, reason='timeout'))
                return
            connection.exit_success(request_id, activity_of(number, self.refresh_number - 1 + self.revision,
                                                            self.flag('diff-threads')))

        self.later(3.0, answer)

    # -- HTTP --

    def http(self, handler, method: str, path: str, body: bytes, session: dict, auth: str) -> bool:
        if path == '/api/pull-requests/diff' and method == 'POST':
            self.read_diff(handler, method, path, body, auth)
            return True
        return super().http(handler, method, path, body, session, auth)

    def read_diff(self, handler, method: str, path: str, body: bytes, auth: str):
        """The diff route. The file `diff-slow` in the run directory holds each slice 4 seconds,
        `diff-fail` fails the first slice and `diff-fail-rest` the second, as a host that times out
        does."""
        try:
            payload = json.loads(body or b'{}')
        except ValueError:
            payload = {}
        if not isinstance(payload, dict):
            payload = {}
        cursor, commit = payload.get('cursor'), payload.get('commit')
        with self.lock:
            self.counts['pr.diff'] += 1
        self.log('pr.diff', number=payload.get('number'), host=payload.get('host'),
                 repository=payload.get('repository'), projectId=payload.get('projectId'), cursor=cursor,
                 commit=commit)
        if self.flag('diff-slow'):
            time.sleep(4)  # the host's delay, in the request's own thread
        if self.flag('diff-fail' if cursor is None else 'diff-fail-rest'):
            handler.reply(method, path, 502, operation_error('diff', 'GitHub took too long to answer.',
                                                             reason='unknown'), auth)
            return
        handler.reply(method, path, 200, diff_of(cursor, self.flag('diff-ts'), commit), auth)


# ---- the command line ----


def add_start_options(start):
    start.add_argument('--capability', choices=CAPABILITIES, default='on',
                       help='links: serve threadPullRequests true (on), false (off) or not at all (missing)')


def options_of(args) -> dict:
    return {'capability': getattr(args, 'capability', 'on')}


def print_expect(expect: dict):
    print_pulls(expect)
    if expect.get('launchMode'):
        print(f'  launches      {expect["launchMode"]}: `t3v launches` changes how the server answers them')
    controls.print_world_expect(expect)


def status_fields(records: list, live) -> dict:
    """The links scenario's part of `t3v status`."""
    changes = [r for r in records if r.get('event') == 'links.change']
    return {
        'capabilityHeld': live.get('capabilityHeld') if live else None,
        'capabilityServed': live.get('capabilityServed') if live else None,
        'linkChanges': [f'{r.get("kind")}:{r.get("threadId")}:{r.get("via")}' for r in changes],
        # What each change sent: the event type on the thread's own streams and on the shell's,
        # with how many streams got it.
        'linkEventsSent': [f'{r.get("threadEvent") or "nothing"} to {r.get("threadStreams", 0)} thread '
                           f'stream(s), {r.get("shellEvent") or "nothing"} to {r.get("shellStreams", 0)} '
                           'shell stream(s)' for r in changes],
        'syncs': sum(1 for r in changes if r.get('kind') == 'sync'),
        'capabilityChanges': [r.get('value') for r in records if r.get('event') == 'capability.change'],
        'lateSent': sum(r.get('delivered', 0) for r in records if r.get('event') == 'late.sent'),
        **launches.status_fields(records, live),
    }


def cmd_links(args) -> int:
    controls.send(args, '/__fixture/links/set', {'threadId': args.thread, 'preset': args.preset, 'via': args.via,
                                                 'event': args.event})
    return 0


def cmd_unlink(args) -> int:
    controls.send(args, '/__fixture/links/unlink', {'threadId': args.thread, 'number': args.number, 'via': args.via})
    return 0


def cmd_sync(args) -> int:
    controls.send(args, '/__fixture/links/sync', {'threadId': args.thread, 'number': args.number, 'state': args.state})
    return 0


def cmd_capability(args) -> int:
    controls.send(args, '/__fixture/capability', {'value': args.value})
    return 0


def cmd_refresh(args) -> int:
    controls.send(args, '/__fixture/refresh', {})
    return 0


def cmd_revise(args) -> int:
    controls.send(args, '/__fixture/revise', {})
    return 0


def cmd_launches(args) -> int:
    controls.send(args, '/__fixture/launches', {'mode': args.mode})
    return 0


def add_commands(commands, run_option):
    """The links scenario's commands. `run_option` adds --run to a parser."""
    links = commands.add_parser('links', help="links: set a thread's links to a preset")
    links.add_argument('thread')
    links.add_argument('preset', choices=LINK_PRESETS)
    links.add_argument('--via', choices=VIA, default='both', help='the copies that change (default both)')
    links.add_argument('--event', choices=LINK_EVENTS, default='metadata-updated',
                       help="the thread.* event that carries the change on the thread's own subscription")
    links.set_defaults(func=cmd_links)

    unlink = commands.add_parser('unlink', help="links: unlink each of a thread's links with that number, as T3 does")
    unlink.add_argument('thread')
    unlink.add_argument('number', type=int)
    unlink.add_argument('--via', choices=VIA, default='both')
    unlink.set_defaults(func=cmd_unlink)

    sync = commands.add_parser('sync', help='links: give a link new host state, as a thread.pull-request-synced')
    sync.add_argument('thread')
    sync.add_argument('number', type=int)
    sync.add_argument('state', choices=SYNC_STATES)
    sync.set_defaults(func=cmd_sync)

    capability = commands.add_parser('capability', help='links: serve threadPullRequests true, false or not at all')
    capability.add_argument('value', choices=CAPABILITIES)
    capability.set_defaults(func=cmd_capability)

    late = commands.add_parser('late', help='links: late data on the thread streams t3term interrupted')
    late.add_argument('thread')
    late.set_defaults(func=controls.cmd_thread_action)

    refresh = commands.add_parser('refresh', help="links: raise pullRequests.subscribeRefreshes's number, as T3 "
                                                  'does after a turn')
    refresh.set_defaults(func=cmd_refresh)
    revise = commands.add_parser('revise', help="links: change what the next pull request read gets, without a "
                                                'refresh number')
    revise.set_defaults(func=cmd_revise)
    launch = commands.add_parser('launches', help='links: how the server answers orchestration.launchThread from '
                                                  'now on: accept, refuse with T3\'s launch error, or drop the '
                                                  'socket after it makes the thread')
    launch.add_argument('mode', choices=launches.LAUNCH_MODES)
    launch.set_defaults(func=cmd_launches)
    for parser in (links, unlink, sync, capability, late, refresh, revise, launch):
        run_option(parser)
