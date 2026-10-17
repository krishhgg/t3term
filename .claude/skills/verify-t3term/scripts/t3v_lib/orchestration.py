"""T3's orchestration feeds over the fake server: the shell subscription that fills the sidebar,
each thread's own subscription, the config and worktree setup streams, and the controls that hold,
end, fail, refuse or feed them. It also works out what t3term should show from what reached it:
the feed's dot, each thread watch and the descriptor's label.

A scenario subclasses Orchestration, builds its world in `build_world` and adds what it serves.
"""

from __future__ import annotations

import copy
import re
from collections import Counter

from .server import FixtureBase
from .text import cells, iso, now_ms, safe
from .world import (ACTIVE, DOTS, INSTANCE, LABEL_PRESETS, NOT_RUNNING, RUN_STATUSES, SHELL_FAILURE, SHELL_REFUSAL,
                    Thread, label_value, provider_entity, shown_label)

ORCHESTRATION_THREAD = re.compile(r'/api/orchestration/threads/([A-Za-z0-9_-]+)(/bounded)?')


def is_sequence(value) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


class Orchestration(FixtureBase):
    """The world, its streams and what t3term should show. Every change holds `lock`."""

    def __init__(self, root, run):
        super().__init__(root, run)
        self.shell_streams = []  # (connection, request id)
        self.held_shell = []  # (connection, request id, payload), answered when the hold ends
        self.unmarked = []  # (connection, request id) of shell streams sent no marker
        self.thread_streams = []  # (connection, request id, thread id)
        self.held_threads = []  # (connection, request id, payload, thread id), answered when the hold ends
        self.interrupted = []  # (connection, request id, thread id) of thread streams t3term let go
        self.config_streams = []  # (connection, request id)
        self.setup_streams = []  # (connection, request id, thread id)
        self.history = []  # shell events after the snapshot, for resubscribes
        self.thread_history = []  # (thread id, thread stream item), for resubscribes
        self.shell_hold_until = 0  # subscribeShell waits until this wall-clock millisecond
        self.watch_hold_until = 0  # subscribeThread waits until this wall-clock millisecond
        self.shell_refuse = False
        self.shell_marker = True
        self.projects = self.threads = self.order = None
        self.removed = set()  # threads the shell leaves out after `gone`
        self.label_preset = self.options.get('label', 'normal')
        self.label_read = None  # the preset the last descriptor read served, which t3term shows
        self.watch_refusing = False
        self.pulses = Counter()  # thread id -> bumped by each pulse, so a new one stops the last
        # What t3term should show, worked out from what reached it.
        self.feed = NOT_RUNNING
        self.watches = {}  # thread id -> its watch's state
        self.snapshots = set()  # threads whose current watch got a snapshot
        self.synced = set()  # threads whose current watch got the caught-up marker
        hold = float(self.options.get('shellHold') or 0)
        if hold > 0:
            self.shell_hold_until = now_ms() + int(hold * 1000)
        self.rpc.update({
            'orchestration.subscribeShell': self.subscribe_shell,
            'orchestration.subscribeThread': self.subscribe_thread,
            'subscribeWorktreeSetup': self.subscribe_setup,
            'subscribeServerConfig': lambda c, r, _: self.subscribe_config(c, r),
            'server.getConfig': lambda c, r, _: self.get_config(c, r),
        })
        self.controls.update({
            '/__fixture/label': lambda request, _: self.change_label(str(request.get('preset') or '')),
            '/__fixture/shell/': lambda request, action: self.shell_action(action, request),
            '/__fixture/thread/': lambda request, action: self.thread_action(action, request.get('threadId'), request),
            '/__fixture/watch/hold': lambda request, _: self.watch_hold(request),
            '/__fixture/watch/refuse': lambda request, _: self.watch_refuse(request),
            '/__fixture/reconnect': lambda request, _: self.reconnect_control(request),
        })

    # -- world --

    def world_fields(self) -> dict:
        """What the world.built and descriptor.read records carry."""
        return {'label': self.label_preset}

    def world_built(self, world: dict):
        self.projects, self.threads, self.order = world['projects'], world['threads'], world['order']
        self.log('world.built', scenario=self.run.get('scenario'), threads=len(self.order), **self.world_fields())

    def descriptor_label(self):
        return label_value(self.label_preset)

    def shell_snapshot(self) -> dict:
        with self.lock:
            self.ensure_world()
            return {'snapshotSequence': self.sequence, 'projects': copy.deepcopy(self.projects),
                    'threads': [copy.deepcopy(self.threads[tid].record) for tid in self.order
                                if tid not in self.removed]}

    def thread_snapshot(self, tid: str):
        with self.lock:
            self.ensure_world()
            thread = self.threads.get(tid)
            if thread is None:
                return None
            return {'snapshotSequence': self.sequence, 'projection': thread.projection()}

    def publish(self, thread: Thread, events: list) -> int:
        """Thread events, each with its own sequence, in one chunk to each open subscribeThread
        stream for the thread and kept for resubscribes. Returns how many streams got them."""
        with self.lock:
            stamp = iso(now_ms())
            values = []
            for event_type, payload in events:
                self.sequence += 1
                value = {'kind': 'event', 'sequence': self.sequence, 'event': {
                    'id': f'evt-fixture-{self.sequence}', 'threadId': thread.tid,
                    'providerInstanceId': INSTANCE, 'occurredAt': stamp, 'type': event_type,
                    'payload': copy.deepcopy(payload)}}
                self.thread_history.append((thread.tid, value))
                values.append(value)
            streams = [(c, r) for c, r, t in self.thread_streams if t == thread.tid]
            for connection, request_id in streams:
                connection.chunk(request_id, 'thread', values, thread.tid)
            return len(streams)

    def push_shell_event(self, event: dict) -> int:
        """A shell event with the next sequence, on every open shell stream, kept for the
        resubscribes of held and later ones. Returns how many streams got it."""
        with self.lock:
            self.sequence += 1
            event = dict(event, sequence=self.sequence)
            self.history.append(event)
            streams = list(self.shell_streams)
            for connection, request_id in streams:
                connection.chunk(request_id, 'shell', [event])
            return len(streams)

    def push_shell(self, thread: Thread) -> int:
        """The sidebar's copy of the thread, as a thread.updated on the shell."""
        with self.lock:
            thread.record['updatedAt'] = iso(now_ms())
            return self.push_shell_event({'kind': 'thread.updated', 'location': 'active',
                                          'thread': copy.deepcopy(thread.record)})

    def config_value(self) -> dict:
        return {'environment': self.descriptor(), 'providers': [provider_entity(iso(now_ms()))], 'settings': {}}

    def push_config(self) -> int:
        """A config snapshot to each open config subscription. Returns how many got it."""
        streams = list(self.config_streams)
        for connection, request_id in streams:
            connection.chunk(request_id, 'config', [{'version': 1, 'type': 'snapshot', 'config': self.config_value()}])
        return len(streams)

    def thread_or_refusal(self, tid):
        self.ensure_world()
        thread = self.threads.get(tid) if isinstance(tid, str) else None
        if thread is None:
            return None, f'no fixture thread {safe(tid, 60)}; `t3v threads` lists them'
        return thread, None

    # -- what t3term should show --

    def feed_to(self, state: str, why: str):
        """Moves the expected feed state. A failed feed stays failed and a quit t3term shows
        nothing, until the next session starts a new t3term."""
        with self.lock:
            if self.feed in ('closed', NOT_RUNNING):
                return
            if self.feed != state:
                self.log('feed.expected', state=state, why=why)
            self.feed = state

    def note_descriptor_read(self):
        with self.lock:
            self.label_read = self.label_preset
            self.counts['descriptor.read'] += 1
            self.log('descriptor.read', **self.world_fields())

    def forget_watches(self):
        """A new or quit t3term holds no watch and no config."""
        self.watches = {}
        self.snapshots, self.synced, self.interrupted = set(), set(), []

    def session_started(self, session: dict):
        # A new login session is a new t3term, which starts its feed over.
        if session['kind'] != 'login':
            return
        self.feed = 'connecting'
        self.forget_watches()
        self.log('feed.expected', state='connecting', why='a new t3term session')

    def session_ended(self, session: dict):
        # t3term revokes its login session as it quits.
        if session['kind'] != 'login':
            return
        self.feed = NOT_RUNNING
        self.forget_watches()

    def expect(self) -> dict:
        with self.lock:
            read = self.label_read
            label = shown_label(label_value(read)) if read is not None else None
            address = f'127.0.0.1:{self.port}' if self.port else None
            identity = ' · '.join(part for part in (label, address) if part)
            hold = max(0.0, (self.shell_hold_until - now_ms()) / 1000)
            return {
                'labelRead': read, 'label': label, 'labelColumns': cells(label) if label else 0,
                'address': address, 'identity': identity, 'identityColumns': cells(identity),
                'labelServed': self.label_preset,
                'feed': self.feed, 'dot': DOTS.get(self.feed, self.feed),
                'watches': dict(self.watches), 'watchRefusing': self.watch_refusing,
                'shell': {'open': len(self.shell_streams), 'held': len(self.held_shell),
                          'holdSecondsLeft': round(hold, 1), 'refusing': self.shell_refuse,
                          'marker': self.shell_marker, 'withoutMarker': len(self.unmarked)},
            }

    def thread_entry(self, thread: Thread) -> dict:
        """One thread for `t3v threads`. A scenario adds what its threads hold."""
        tid = thread.tid
        run = thread.latest_run()
        return {'threadId': tid, 'title': thread.title,
                'run': f'run {run["ordinal"]} {run["status"]}' if run else 'no runs',
                'streams': sum(1 for _, _, t in self.thread_streams if t == tid),
                'held': sum(1 for h in self.held_threads if h[3] == tid),
                'watch': self.watches.get(tid), 'listed': tid not in self.removed}

    def thread_list(self) -> list:
        with self.lock:
            self.ensure_world()
            return [self.thread_entry(self.threads[tid]) for tid in self.order]

    def state(self) -> dict:
        with self.lock:
            return dict(self.expect(), threads=self.thread_list(), server=self.report())

    def answer(self, note: str) -> dict:
        return {'note': note, 'expect': self.expect()}

    # -- the shell subscription --

    def take_shell_streams(self) -> list:
        """Every open and held shell stream, forgotten here and on its connection."""
        with self.lock:
            streams = list(self.shell_streams) + [(c, r) for c, r, _ in self.held_shell]
            self.shell_streams, self.held_shell, self.unmarked = [], [], []
            for connection, request_id in streams:
                connection.streams.pop(request_id, None)
            return streams

    def shell_end(self) -> int:
        with self.lock:
            streams = self.take_shell_streams()
            delivered = sum(1 for c, r in streams if c.exit_success(r, None))
            self.counts['shell.ended'] += len(streams)
            self.log('shell.end', streams=len(streams), delivered=delivered)
            if delivered:
                self.feed_to('reconnecting', 'T3 ended the shell subscription')
            return len(streams)

    def shell_fail(self) -> int:
        with self.lock:
            streams = self.take_shell_streams()
            delivered = 0
            for connection, request_id in streams:
                if connection.exit_failure(request_id, 'orchestration.subscribeShell', *SHELL_FAILURE):
                    delivered += 1
            self.counts['shell.failed'] += len(streams)
            self.log('shell.fail', streams=len(streams), delivered=delivered)
            if delivered:
                self.feed_to('closed', 'T3 failed the shell subscription')
            return len(streams)

    def shell_action(self, action: str, request: dict):
        with self.lock:
            self.ensure_world()
            if action == 'end':
                count = self.shell_end()
                note = (f'ended {count} shell subscription(s) with a success Exit; t3term subscribes again '
                        f'after its backoff, about 250 ms')
            elif action == 'fail':
                count = self.shell_fail()
                note = (f'failed {count} shell subscription(s) with {SHELL_FAILURE[0]}; t3term stops '
                        f'watching the feed until it restarts')
            elif action == 'hold':
                try:
                    seconds = float(request.get('seconds'))
                except (TypeError, ValueError):
                    seconds = -1.0
                if not 0 < seconds <= 120:
                    return None, 'hold takes 0 to 120 seconds'
                keep = request.get('keep') is True
                self.shell_hold_until = now_ms() + int(seconds * 1000)
                self.log('shell.hold', seconds=seconds, keep=keep)
                ended = 0 if keep else self.shell_end()
                note = (f'subscribeShell waits {seconds:g} s from now'
                        + ('' if keep else f'; ended {ended} open shell subscription(s)'))
            elif action == 'refuse':
                self.shell_refuse = request.get('on') is True
                self.log('shell.refusal', on=self.shell_refuse)
                note = f'subscribeShell is {"refused" if self.shell_refuse else "served"} from now on'
            elif action == 'marker':
                self.shell_marker = request.get('on') is True
                sent = 0
                if self.shell_marker:
                    for connection, request_id in list(self.unmarked):
                        connection.chunk(request_id, 'shell', [{'kind': 'synchronized'}])
                        sent += 1
                    self.unmarked = []
                self.log('shell.marker', on=self.shell_marker, sent=sent)
                note = (f'subscribeShell sends its synchronized marker from now on; sent it to {sent} stream(s)'
                        if self.shell_marker else 'subscribeShell leaves out its synchronized marker from now on')
            else:
                return None, f'no shell action {safe(action, 30)}'
            return self.answer(note), None

    # -- threads --

    def other_thread_action(self, action: str, thread: Thread, request: dict):
        """A scenario's own thread actions: (note, None), (None, refusal), or None for no such
        action."""
        return None

    def thread_action(self, action: str, tid: str, request: dict):
        with self.lock:
            thread, refusal = self.thread_or_refusal(tid)
            if thread is None:
                return None, refusal
            if action == 'run':
                status = request.get('status')
                if status not in RUN_STATUSES:
                    return None, f'run takes {", ".join(RUN_STATUSES)}'
                latest = thread.latest_run()
                if latest is None:
                    return None, f'{tid} has no run'
                stamp = iso(now_ms())
                run = dict(latest, status=status)
                if status in ACTIVE:
                    run['completedAt'] = None
                    if status in ('running', 'waiting') and not run['startedAt']:
                        run['startedAt'] = stamp
                else:
                    run['completedAt'] = stamp
                    run['startedAt'] = run['startedAt'] or stamp
                events = thread.change('runs', run)
                thread.settle_record()
                streams = self.publish(thread, events + [('thread.updated', copy.deepcopy(thread.watched))])
                listed = tid not in self.removed
                if listed:
                    self.push_shell(thread)
                note = (f'{tid} run {run["ordinal"]} is {status}, on {streams} thread stream(s)'
                        + (' and the shell' if listed else '; the shell leaves the thread out after `gone`'))
            elif action == 'pulse':
                try:
                    count, gap = int(request.get('count', 20)), float(request.get('gap', 0.5))
                except (TypeError, ValueError):
                    count, gap = -1, -1.0
                if not 1 <= count <= 200 or not 0.05 <= gap <= 30:
                    return None, 'pulse takes a count of 1 to 200 and a gap of 0.05 to 30 seconds'
                if thread.latest_run() is None:
                    return None, f'{tid} has no run to reply on'
                self.pulses[tid] += 1
                generation = self.pulses[tid]
                for k in range(1, count + 1):
                    self.later((k - 1) * gap, lambda k=k: self.pulse_step(thread, generation, k, count))
                streams = sum(1 for _, _, t in self.thread_streams if t == tid)
                note = f'{count} replies on {tid}, {gap:g} s apart, to its {streams} open thread stream(s)'
            elif action == 'close':
                streams = [(c, r) for c, r, t in self.thread_streams if t == tid]
                held = [(c, r) for c, r, _, t in self.held_threads if t == tid]
                self.thread_streams = [(c, r, t) for c, r, t in self.thread_streams if t != tid]
                self.held_threads = [h for h in self.held_threads if h[3] != tid]
                delivered = 0
                for connection, request_id in streams + held:
                    connection.streams.pop(request_id, None)
                    if connection.exit_failure(request_id, 'orchestration.subscribeThread',
                                               'OrchestrationV2ThreadNotFoundError',
                                               f'The fixture closed thread {tid}.'):
                        delivered += 1
                if delivered:
                    self.watches[tid] = 'closed'
                note = (f'failed {len(streams)} open and {len(held)} held subscribeThread stream(s) for {tid}, '
                        f'{delivered} of them on an open socket')
            elif action == 'gone':
                if tid in self.removed:
                    return None, f'the shell already leaves {tid} out; `t3v back {tid}` lists it again'
                self.removed.add(tid)
                streams = self.push_shell_event({'kind': 'thread.removed', 'location': 'active', 'threadId': tid})
                note = (f'sent a thread.removed for {tid} on {streams} shell stream(s); its own subscription '
                        f'stays as it was')
            elif action == 'back':
                if tid not in self.removed:
                    return None, f'the shell already lists {tid}'
                self.removed.discard(tid)
                streams = self.push_shell(thread)
                note = f'sent a thread.updated that lists {tid} again on {streams} shell stream(s)'
            else:
                other = self.other_thread_action(action, thread, request)
                if other is None:
                    return None, f'no thread action {safe(action, 30)}'
                note, refusal = other
                if note is None:
                    return None, refusal
            self.counts['thread.' + action] += 1
            self.log('thread.action', action=action, threadId=tid)
            return self.answer(note), None

    def watch_refuse(self, request: dict):
        with self.lock:
            self.ensure_world()
            self.watch_refusing = request.get('on') is True
            self.log('watch.refusal', on=self.watch_refusing)
            note = (f'subscribeThread is {"refused" if self.watch_refusing else "served"} from now on; open thread '
                    f'streams stay as they are')
            return self.answer(note), None

    def watch_hold(self, request: dict):
        with self.lock:
            self.ensure_world()
            try:
                seconds = float(request.get('seconds'))
            except (TypeError, ValueError):
                seconds = -1.0
            if not 0 <= seconds <= 120:
                return None, 'watch hold takes 0 to 120 seconds; 0 ends a hold'
            self.watch_hold_until = now_ms() + int(seconds * 1000)
            held = len(self.held_threads)
            if seconds == 0:
                for connection, request_id, _, _ in list(self.held_threads):
                    self.later(0, lambda c=connection, r=request_id: self.release_thread(c, r))
            self.log('watch.hold', seconds=seconds, held=held)
            if seconds == 0:
                note = f'ended the hold; answering {held} held subscribeThread stream(s) now'
            else:
                note = (f'each new subscribeThread waits until {seconds:g} s from now, and so do the {held} '
                        f'already held; open thread streams stay as they are')
            return self.answer(note), None

    def pulse_step(self, thread: Thread, generation: int, k: int, count: int):
        with self.lock:
            if self.pulses[thread.tid] != generation:
                return
            latest = thread.latest_run()
            if latest is None:
                return
            stamp = iso(now_ms())
            events = thread.reply(latest['ordinal'], f'Pulse reply {k} of {count} on "{thread.title}" at {stamp}: '
                                                     f'invented text, one more row in the transcript.', stamp)
            streams = self.publish(thread, events)
            self.counts['pulse.reply'] += 1
            self.log('pulse.reply', threadId=thread.tid, n=k, of=count, streams=streams)

    # -- the label and the socket --

    def change_label(self, preset: str):
        with self.lock:
            if preset not in LABEL_PRESETS:
                return None, f'label takes {", ".join(LABEL_PRESETS)}'
            self.ensure_world()
            self.label_preset = preset
            pushed = self.push_config()
            self.counts['label.change'] += 1
            self.log('label.change', preset=preset, configStreams=pushed)
            shown = self.label_read
            note = (f'the descriptor and {pushed} config subscription(s) now carry the {preset} label; t3term '
                    f'should keep the {shown or "first"} label it read until `t3v restart`')
            return self.answer(note), None

    def reconnect_control(self, request: dict):
        try:
            hold = float(request.get('hold') or 3)
        except (TypeError, ValueError):
            hold = -1
        if not 0 < hold <= 30:
            return None, 'reconnect takes a hold of 0 to 30 seconds'
        return self.reconnect(hold)

    def reconnect(self, hold: float):
        """Closes every WebSocket and answers /ws with 503 for `hold` seconds."""
        with self.lock:
            self.ensure_world()
            self.counts['reconnect'] += 1
            self.log('reconnect', holdSeconds=hold, sockets=len(self.connections))
        closed = self.drop_sockets(hold)
        return self.answer(f'closed {closed} socket(s); /ws answers 503 for {hold:g} s'), None

    # -- streams --

    def on_chunk(self, connection, request_id: str, stream: str, values: list, delivered: bool, thread_id=None):
        with self.lock:
            marker = any(isinstance(v, dict) and v.get('kind') == 'synchronized' for v in values)
            if stream == 'shell':
                self.counts['shell.chunk'] += 1
                self.log('shell.chunk', conn=connection.id, requestId=request_id,
                         kinds=[v.get('kind') for v in values], delivered=delivered)
                if delivered and marker:
                    self.feed_to('live', 'T3 sent the synchronized marker')
            elif stream == 'config':
                self.counts['config.chunk'] += 1
                self.log('config.chunk', conn=connection.id, requestId=request_id,
                         types=[v.get('type') for v in values], delivered=delivered, **self.world_fields())
                if delivered:
                    self.config_delivered()
            elif stream == 'setup':
                self.counts['setup.chunk'] += 1
                self.log('setup.chunk', conn=connection.id, requestId=request_id, threadId=thread_id,
                         delivered=delivered)
            elif stream == 'thread':
                events = [v.get('event') or {} for v in values if v.get('kind') == 'event']
                types = [event.get('type') for event in events]
                self.counts['thread.event'] += len(types)
                self.log('thread.chunk', conn=connection.id, requestId=request_id, threadId=thread_id,
                         kinds=[v.get('kind') for v in values], eventTypes=types or None, delivered=delivered)
                if delivered and any(v.get('kind') == 'snapshot' for v in values):
                    self.snapshots.add(thread_id)
                if delivered and marker:
                    self.synced.add(thread_id)
                if delivered and marker and self.watches.get(thread_id) != 'closed':
                    self.watches[thread_id] = 'live'

    def config_delivered(self):
        """A config reached t3term, by subscription or by server.getConfig."""

    def on_interrupt(self, connection, request_id: str, tag):
        self.drop_streams(connection, request_id)

    def on_close(self, connection):
        self.drop_streams(connection)

    def drop_streams(self, connection, request_id=None):
        """Forgets a connection's streams after its socket closed, or one of them after an
        interrupt, and moves what t3term should show."""
        def kept(c, r) -> bool:
            return not (c is connection and (request_id is None or r == request_id))

        with self.lock:
            shell = (any(not kept(c, r) for c, r in self.shell_streams)
                     or any(not kept(c, r) for c, r, _ in self.held_shell))
            dropped = [(c, r, t) for c, r, t in self.thread_streams if not kept(c, r)]
            dropped += [(c, r, t) for c, r, _, t in self.held_threads if not kept(c, r)]
            threads = [t for _, _, t in dropped]
            self.shell_streams = [(c, r) for c, r in self.shell_streams if kept(c, r)]
            self.held_shell = [(c, r, p) for c, r, p in self.held_shell if kept(c, r)]
            self.unmarked = [(c, r) for c, r in self.unmarked if kept(c, r)]
            self.thread_streams = [(c, r, t) for c, r, t in self.thread_streams if kept(c, r)]
            self.held_threads = [h for h in self.held_threads if kept(h[0], h[1])]
            self.config_streams = [(c, r) for c, r in self.config_streams if kept(c, r)]
            self.setup_streams = [(c, r, t) for c, r, t in self.setup_streams if kept(c, r)]
            if request_id is None:
                self.interrupted = [(c, r, t) for c, r, t in self.interrupted if c is not connection]
                if shell:
                    self.feed_to('reconnecting', 'the socket closed')
                for tid in threads:
                    if self.watches.get(tid) not in (None, 'closed'):
                        self.watches[tid] = 'reconnecting'
            else:
                # t3term let the thread go, on a switch. `late` writes to these streams.
                self.interrupted += dropped
                for tid in threads:
                    self.watches.pop(tid, None)
                    self.snapshots.discard(tid)
                    self.synced.discard(tid)

    # -- the RPC methods --

    def subscribe_shell(self, connection, request_id: str, payload: dict):
        """The sidebar's feed: refused, held until the hold ends, or answered at once."""
        with self.lock:
            self.ensure_world()
            after = payload.get('afterSequence')
            resume = is_sequence(after)
            self.log('shell.subscribe', conn=connection.id, requestId=request_id, resume=resume,
                     afterSequence=after if resume else None)
            if self.shell_refuse:
                self.counts['shell.refused'] += 1
                self.log('shell.refused', conn=connection.id, requestId=request_id)
                if connection.exit_failure(request_id, 'orchestration.subscribeShell', *SHELL_REFUSAL):
                    self.feed_to('closed', 'T3 refused the shell subscription')
                return
            connection.streams[request_id] = 'orchestration.subscribeShell'
            wait = (self.shell_hold_until - now_ms()) / 1000
            if wait > 0:
                self.held_shell.append((connection, request_id, payload))
                self.counts['shell.held'] += 1
                self.log('shell.held', conn=connection.id, requestId=request_id, seconds=round(wait, 1))
                self.later(wait, lambda: self.release_shell(connection, request_id))
                return
            self.answer_shell(connection, request_id, payload)

    def release_shell(self, connection, request_id: str):
        with self.lock:
            entry = next((h for h in self.held_shell if h[0] is connection and h[1] == request_id), None)
            if entry is None:
                # Ended, failed, interrupted or its socket closed during the hold.
                return
            wait = (self.shell_hold_until - now_ms()) / 1000
            if wait > 0:
                # A newer hold.
                self.later(wait, lambda: self.release_shell(connection, request_id))
                return
            self.held_shell.remove(entry)
            self.log('shell.released', conn=connection.id, requestId=request_id)
            self.answer_shell(connection, request_id, entry[2])

    def answer_shell(self, connection, request_id: str, payload: dict):
        """Registers the stream, then sends the replay after its sequence or a snapshot, and the
        marker when asked for and not withheld. One lock hold covers it all, so an event can't
        land between them and be missed."""
        with self.lock:
            self.shell_streams.append((connection, request_id))
            after = payload.get('afterSequence')
            if is_sequence(after):
                replay = [e for e in self.history if e['sequence'] > after]
                if replay:
                    connection.chunk(request_id, 'shell', replay)
            else:
                connection.chunk(request_id, 'shell', [{'kind': 'snapshot', 'snapshot': self.shell_snapshot()}])
            if payload.get('requestCompletionMarker'):
                if self.shell_marker:
                    connection.chunk(request_id, 'shell', [{'kind': 'synchronized'}])
                else:
                    self.unmarked.append((connection, request_id))
                    self.counts['shell.marker_withheld'] += 1
                    self.log('shell.marker_withheld', conn=connection.id, requestId=request_id)

    def subscribe_thread(self, connection, request_id: str, payload: dict):
        """A thread's own subscription: refused for an unknown thread or while `watch refuse` is
        on, held until the hold ends, or answered at once."""
        tid = str(payload.get('threadId'))
        resume = is_sequence(payload.get('afterSequence'))
        with self.lock:
            self.ensure_world()
            found = tid in self.threads
            self.log('thread.subscribe', conn=connection.id, requestId=request_id, threadId=safe(tid, 60),
                     resume=resume, found=found)
            if not found:
                connection.exit_failure(request_id, 'orchestration.subscribeThread',
                                        'OrchestrationV2ThreadNotFoundError', f'No fixture thread has id {tid}.')
                return
            if self.watches.get(tid) != 'reconnecting':
                self.watches[tid] = 'connecting'
            if not resume:
                # A new watch starts with nothing from T3. t3term watches one thread, so a
                # fresh subscribe also means it moved on from any watch the fixture closed,
                # which sends no interrupt.
                self.snapshots.discard(tid)
                self.synced.discard(tid)
                for other in [t for t, state in self.watches.items() if t != tid and state == 'closed']:
                    del self.watches[other]
                    self.snapshots.discard(other)
                    self.synced.discard(other)
            if self.watch_refusing:
                self.counts['thread.refused'] += 1
                self.log('thread.refused', conn=connection.id, requestId=request_id, threadId=tid)
                if connection.exit_failure(request_id, 'orchestration.subscribeThread',
                                           'OrchestrationV2ThreadNotFoundError',
                                           "The fixture refused this thread's subscription."):
                    self.watches[tid] = 'closed'
                return
            connection.streams[request_id] = 'orchestration.subscribeThread'
            wait = (self.watch_hold_until - now_ms()) / 1000
            if wait > 0:
                self.held_threads.append((connection, request_id, payload, tid))
                self.counts['thread.held'] += 1
                self.log('thread.held', conn=connection.id, requestId=request_id, threadId=tid,
                         seconds=round(wait, 1))
                self.later(wait, lambda: self.release_thread(connection, request_id))
                return
            self.answer_thread(connection, request_id, payload, tid)

    def release_thread(self, connection, request_id: str):
        with self.lock:
            entry = next((h for h in self.held_threads if h[0] is connection and h[1] == request_id), None)
            if entry is None:
                # Closed, interrupted, answered or its socket closed during the hold.
                return
            wait = (self.watch_hold_until - now_ms()) / 1000
            if wait > 0:
                # A newer hold.
                self.later(wait, lambda: self.release_thread(connection, request_id))
                return
            self.held_threads.remove(entry)
            self.counts['thread.released'] += 1
            self.log('thread.released', conn=connection.id, requestId=request_id, threadId=entry[3])
            self.answer_thread(connection, request_id, entry[2], entry[3])

    def answer_thread(self, connection, request_id: str, payload: dict, tid: str):
        """Registers the stream, then sends the replay after its sequence or a snapshot, and the
        marker when asked for. One lock hold covers it all, so an event can't land between the
        snapshot and the registration and be missed."""
        with self.lock:
            self.thread_streams.append((connection, request_id, tid))
            after = payload.get('afterSequence')
            if is_sequence(after):
                replay = [item for t, item in self.thread_history if t == tid and item['sequence'] > after]
                if replay:
                    connection.chunk(request_id, 'thread', replay, tid)
            else:
                connection.chunk(request_id, 'thread', [dict(self.thread_snapshot(tid), kind='snapshot')], tid)
            if payload.get('requestCompletionMarker'):
                connection.chunk(request_id, 'thread', [{'kind': 'synchronized'}], tid)

    def subscribe_setup(self, connection, request_id: str, payload: dict):
        """t3term asks for a thread whose worktree path is a string. T3's answer for a thread
        with no setup is null, and the stream stays open."""
        tid = payload.get('threadId')
        with self.lock:
            self.counts['setup.request'] += 1
            self.log('setup.request', conn=connection.id, requestId=request_id,
                     threadId=tid if isinstance(tid, str) else None)
            connection.streams[request_id] = 'subscribeWorktreeSetup'
            self.setup_streams.append((connection, request_id, str(tid)))
            connection.chunk(request_id, 'setup', [None], str(tid))

    def subscribe_config(self, connection, request_id: str):
        """T3's config subscription: a snapshot of the whole config, and another after `label`."""
        with self.lock:
            self.ensure_world()
            connection.streams[request_id] = 'subscribeServerConfig'
            self.config_streams.append((connection, request_id))
            connection.chunk(request_id, 'config', [{'version': 1, 'type': 'snapshot', 'config': self.config_value()}])

    def get_config(self, connection, request_id: str):
        with self.lock:
            if connection.exit_success(request_id, self.config_value()):
                self.config_delivered()
            self.counts['config.read'] += 1

    # -- HTTP --

    def http(self, handler, method: str, path: str, body: bytes, session: dict, auth: str) -> bool:
        """The orchestration routes, past the server's scope and protocol checks."""
        if method != 'GET':
            return False
        if path == '/api/orchestration/shell':
            handler.reply(method, path, 200, self.shell_snapshot(), auth)
            return True
        match = ORCHESTRATION_THREAD.fullmatch(path)
        if match:
            snapshot = self.thread_snapshot(match.group(1))
            if snapshot is not None:
                handler.reply(method, path, 200, snapshot, auth)
                return True
        return False
