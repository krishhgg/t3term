"""The shared world: one invented thread in T3's V2 shapes, the descriptor's label presets and the
provider t3term sees. A scenario (scenarios/) builds its threads with these. Every project, thread
and line of text is invented.
"""

from __future__ import annotations

import copy
from collections import Counter

from .text import MINUTE, MISSING, RUST_WHITESPACE, SECOND, bounded, without_controls

INSTANCE, MODEL = 'codex', 'gpt-6'
ACTIVE = ('preparing', 'starting', 'running', 'waiting')
# No `preparing`: a preparing run would make t3term subscribe to the thread's worktree setup,
# which the fake answers only with null.
RUN_STATUSES = ('starting', 'running', 'waiting', 'completed', 'interrupted', 'failed', 'cancelled')
EFFORT = {'id': 'effort', 'label': 'Effort', 'type': 'select',
          'options': [{'id': 'low', 'label': 'Low'},
                      {'id': 'medium', 'label': 'Medium', 'isDefault': True},
                      {'id': 'high', 'label': 'High'}]}

# What T3's shell subscription fails with here. The tag is invented; t3term reads the message.
SHELL_FAILURE = ('FixtureShellClosed', 'The fixture closed the thread list feed. Nothing real was closed.')
SHELL_REFUSAL = ('EnvironmentAuthorizationError',
                 'The authenticated token is missing required scope: orchestration:read.')

# What the fixture expects t3term's feed to be, from what it sent. `live` needs the marker.
NOT_RUNNING = 'not running'
DOTS = {
    'connecting': 'an amber ● with "connecting" before it where the row has room',
    'live': 'an emerald ● with no word',
    'reconnecting': 'an amber ● with "reconnecting" before it where the row has room',
    'closed': 'a red ● with "closed" before it where the row has room, until t3term restarts',
    NOT_RUNNING: 't3term is not running against this fixture',
}

# ---- the descriptor's label: src/tui/environment.rs and src/tui/setup.rs's bounded ----

LABEL_PRESETS = ('normal', 'absent', 'blank', 'hostile', 'huge')
# The most of a descriptor's body t3term reads (MAX_DESCRIPTOR_BYTES in src/discovery.rs). The
# huge preset's label alone is this long, so the descriptor passes it by its other fields.
DESCRIPTOR_CAP = 256 * 1024
LABEL_MAX = 64  # LABEL in src/tui/environment.rs; bounded reads at most four times as many
JOINED = '\U0001f469\u200d\U0001f4bb'  # 👩 ZWJ 💻
HOSTILE_HEAD = ('\t\x07 Fixture\r\nMac \x1b[31mred \x1b]52;c;eA==\x07osc \x9b2J \x07bel \x85'
                '漢字かな한글 cafe\u0301 ')


def hostile_label() -> str:
    """The hostile preset. The padding puts 👩‍💻 at character 62 of the cleaned label, so its
    three characters cross the 64-character cut and t3term must drop it whole."""
    before = len(without_controls(HOSTILE_HEAD).replace('\n', ' ').lstrip(RUST_WHITESPACE))
    pad = LABEL_MAX - 2 - before
    if pad < 1:
        raise ValueError('the hostile head is too long for its padding')
    label = HOSTILE_HEAD + 'z' * pad + JOINED + ' past the cut, must not show '
    return label + '-' * (LABEL_MAX * 4) + ' BEYOND-CAP'


def label_value(preset):
    """The descriptor's label for `preset`, or MISSING to leave the field out."""
    if preset == 'normal':
        return 'Fixture Mac'
    if preset == 'absent':
        return MISSING
    if preset == 'blank':
        return ' \t\x07\r\n\x1b '
    if preset == 'hostile':
        return hostile_label()
    if preset == 'huge':
        return 'Huge Mac ' + 'x' * (DESCRIPTOR_CAP - len('Huge Mac '))
    raise ValueError(f'no label preset {preset}')


def shown_label(value):
    """What Identity::new keeps of a descriptor label, as setup::bounded(label, 64) cleans and
    cuts it, or None when nothing is left."""
    if not isinstance(value, str):
        return None
    return bounded(value, LABEL_MAX) or None


def provider_entity(stamp: str) -> dict:
    """One ready, signed-in Codex, so the provider banner stays out of the captures."""
    return {'instanceId': INSTANCE, 'driver': 'codex', 'displayName': 'Codex', 'enabled': True, 'installed': True,
            'version': '1.2.0', 'status': 'ready', 'auth': {'status': 'authenticated'}, 'message': None,
            'checkedAt': stamp, 'setup': {'canAuthenticate': False, 'canInstall': False},
            'compatibilityAdvisory': None, 'supportedRuntimeModes': None,
            'models': [{'slug': MODEL, 'name': 'GPT-6', 'shortName': 'GPT-6', 'isCustom': False,
                        'capabilities': {'optionDescriptors': [copy.deepcopy(EFFORT)]}}]}


# ---- one thread ----


class Thread:
    """One invented thread in the nightly's V2 shapes: its runs, messages and turn items. Each
    builder changes the thread and returns the (event type, entity) pairs that carry the change,
    as T3 sends them."""

    def __init__(self, tid: str, project: dict, title: str, created: str, visited: str):
        self.tid, self.title, self.short = tid, title, tid[len('thr-'):]
        self.runs, self.messages, self.items = [], [], []
        self.numbers = Counter()
        # The app thread and the shell thread in one record.
        self.record = {
            'id': tid, 'projectId': project['id'], 'title': title, 'branch': 'main',
            'worktreePath': None, 'providerInstanceId': INSTANCE,
            'modelSelection': {'instanceId': INSTANCE, 'model': MODEL},
            'runtimeMode': 'full-access', 'interactionMode': 'default',
            'lineage': {'parentThreadId': None, 'relationshipToParent': None, 'rootThreadId': tid},
            'forkedFrom': None, 'activeProviderThreadId': f'pthr-{self.short}',
            'latestRunId': None, 'activeRunId': None, 'status': 'idle', 'activityRunStatus': None,
            'pendingRuntimeRequest': None, 'pendingBackgroundTasks': [],
            'latestUserMessageAt': None, 'latestUserAuthoredMessageAt': None, 'latestRunRequestedAt': None,
            'latestRunStartedAt': None, 'latestRunCompletedAt': None, 'activityRunStartedAt': None,
            'lastError': None, 'lastErrorClass': None,
            'createdAt': created, 'updatedAt': created, 'archivedAt': None, 'pinnedAt': None,
            'pinOrderKey': None, 'activeOrderKey': None, 'settledOverride': None, 'settledAt': None,
            'unsettledAt': None, 'snoozedAt': None, 'snoozedUntil': None, 'lastVisitedAt': visited,
        }
        # The thread subscription's copy of the record. It starts as the shell's, and `--via`
        # lets the two differ.
        self.watched = copy.deepcopy(self.record)

    def copies(self, via: str) -> list:
        """The shell's copy, the thread subscription's or both, each with its name."""
        named = {'shell': self.record, 'thread': self.watched}
        return [(name, named[name]) for name in (('shell', 'thread') if via == 'both' else (via,))]

    def set_fields(self, fields: dict, via: str):
        """Sets fields on the shell's copy, the thread subscription's or both. MISSING leaves a
        field out of the record."""
        for _, target in self.copies(via):
            for key, value in fields.items():
                if value is MISSING:
                    target.pop(key, None)
                else:
                    target[key] = copy.deepcopy(value)

    def unlink(self, number: int, via: str) -> dict:
        """Unlinks each link numbered `number` on the copies `via` names, as Orchestrator.ts
        unlinks one: a member of a GitHub stack stays as a stack-dismissed tombstone without its
        watch, and any other link goes. Returns how many it unlinked on each copy."""
        def place(one: dict) -> tuple:
            return tuple(str(one.get(key, '')).strip(RUST_WHITESPACE).lower() for key in ('host', 'repository'))

        counts = {}
        for name, target in self.copies(via):
            links = target.get('pullRequests')
            if not isinstance(links, list):
                counts[name] = 0
                continue
            layered = {place(one) for one in links
                       if isinstance(one, dict) and isinstance(one.get('stack'), dict)
                       and any(isinstance(layer, dict) and layer.get('number') == number
                               for layer in one['stack'].get('layers') or [])}
            kept, count = [], 0
            for one in links:
                if not isinstance(one, dict) or one.get('number') != number or one.get('source') == 'stack-dismissed':
                    kept.append(one)
                    continue
                count += 1
                if one.get('source') == 'stack' or one.get('stack') is not None or place(one) in layered:
                    tombstone = {key: value for key, value in one.items() if key != 'watch'}
                    kept.append(dict(tombstone, source='stack-dismissed'))
            target['pullRequests'] = kept
            counts[name] = count
        return counts

    def sync(self, number: int, state: str, stamp: str) -> dict:
        """Gives each link numbered `number`, on both copies, the host state `state` synced at
        `stamp`, as the sync reactor does. A link waiting for its host gets its first snapshot.
        Returns how many it synced on each copy."""
        counts = {}
        for name, target in self.copies('both'):
            links = target.get('pullRequests')
            count = 0
            for one in links if isinstance(links, list) else []:
                if not isinstance(one, dict) or one.get('number') != number or one.get('source') == 'stack-dismissed':
                    continue
                snapshot = one.get('snapshot')
                if not isinstance(snapshot, dict):
                    author = {'login': 'fixture-dev', 'name': None, 'avatarUrl': None, 'isBot': False}
                    snapshot = {'title': f'Synced: #{number} reached its host', 'headBranch': f'fixture/pr-{number}',
                                'baseBranch': 'main', 'author': author}
                one['snapshot'] = dict(snapshot, state='open' if state == 'draft' else state,
                                       isDraft=state == 'draft', updatedAt=stamp, syncedAt=stamp)
                count += 1
            counts[name] = count
        return counts

    def run_id(self, n: int) -> str:
        return f'run-{self.short}-{n}'

    def root(self, n: int) -> str:
        return f'node-{self.short}-{n}-root'

    def change(self, key: str, entity: dict) -> list:
        collection = getattr(self, key)
        index = next((i for i, e in enumerate(collection) if e['id'] == entity['id']), None)
        if key == 'items':
            # A new item goes last; a changed one keeps its place.
            entity = dict(entity, ordinal=len(collection) if index is None else collection[index]['ordinal'])
        if index is None:
            collection.append(entity)
        else:
            collection[index] = entity
        kind = {'runs': 'run.created' if index is None else 'run.updated',
                'messages': 'message.updated', 'items': 'turn-item.updated'}[key]
        return [(kind, copy.deepcopy(entity))]

    def item(self, n: int, key: str, stamp: str, **fields) -> dict:
        shaped = {'id': f'item-{self.short}-{key}', 'threadId': self.tid, 'runId': self.run_id(n),
                  'nodeId': self.root(n), 'providerThreadId': None, 'providerTurnId': None, 'nativeItemRef': None,
                  'parentItemId': None, 'status': 'completed', 'title': None, 'startedAt': stamp,
                  'completedAt': stamp, 'updatedAt': stamp}
        shaped.update(fields)
        return shaped

    def prompt(self, n: int, asked: str, status: str, started=None, completed=None) -> list:
        """Run n with its status, the user message that asked for it and that message's item."""
        message_id = f'msg-{self.short}-{n}'
        text = f'Fixture prompt {n} for "{self.title}". Nothing here came from a real thread.'
        run = {'id': self.run_id(n), 'threadId': self.tid, 'ordinal': n, 'providerInstanceId': INSTANCE,
               'modelSelection': copy.deepcopy(self.record['modelSelection']), 'providerThreadId': None,
               'userMessageId': message_id, 'rootNodeId': self.root(n), 'activeAttemptId': None,
               'status': status, 'requestedAt': asked, 'startedAt': started, 'completedAt': completed,
               'checkpointId': None, 'contextHandoffId': None}
        message = {'id': message_id, 'role': 'user', 'runId': run['id'], 'text': text, 'createdAt': asked}
        item = self.item(n, f'prompt-{n}', asked, type='user_message', text=text)
        return self.change('runs', run) + self.change('messages', message) + self.change('items', item)

    def reply(self, n: int, text: str, stamp: str) -> list:
        self.numbers['reply'] += 1
        key = f'reply-{n}-{self.numbers["reply"]}'
        return self.change('items', self.item(n, key, stamp, type='assistant_message',
                                              messageId=f'msg-{self.short}-{key}', streaming=False, text=text))

    def history(self, at, count: int, last: int, step: int = 5 * MINUTE):
        """`count` finished turns, the last asked at offset `last`, one every `step`."""
        for n in range(1, count + 1):
            asked = last - (count - n) * step
            self.prompt(n, at(asked), 'completed', at(asked + SECOND), at(asked + 40 * SECOND))
            self.reply(n, f'Fixture reply {n} on "{self.title}": the invented work is done.\n\n'
                          f'It touched {n + 2} invented files, and nothing here came from a real thread.',
                       at(asked + 40 * SECOND))

    def latest_run(self):
        return max(self.runs, key=lambda run: run['ordinal'], default=None)

    def activity_run(self):
        """The newest run at work, else the newest run, as ThreadState::activity_run picks it."""
        active = [run for run in self.runs if run['status'] in ACTIVE]
        return max(active or self.runs, key=lambda run: run['ordinal'], default=None)

    def settle_record(self):
        """The run fields of both copies of the record, from the runs."""
        latest = self.latest_run()
        if latest is None:
            return
        activity = self.activity_run()
        active = activity if activity['status'] in ACTIVE else None
        asked = latest['requestedAt']
        fields = dict(
            latestRunId=latest['id'], activeRunId=active['id'] if active else None,
            status=activity['status'], activityRunStatus=active['status'] if active else None,
            latestUserMessageAt=asked, latestUserAuthoredMessageAt=asked, latestRunRequestedAt=asked,
            latestRunStartedAt=latest['startedAt'], latestRunCompletedAt=latest['completedAt'],
            activityRunStartedAt=(active['startedAt'] or active['requestedAt']) if active else latest['startedAt'],
            updatedAt=latest['completedAt'] or latest['startedAt'] or asked)
        self.record.update(fields)
        self.watched.update(fields)

    def projection(self) -> dict:
        own = copy.deepcopy(sorted(self.items, key=lambda item: item['ordinal']))
        visible = [{'position': i, 'visibility': 'local', 'sourceThreadId': self.tid, 'sourceItemId': item['id'],
                    'item': item} for i, item in enumerate(own)]
        return {'thread': copy.deepcopy(self.watched), 'runs': copy.deepcopy(self.runs),
                'messages': copy.deepcopy(self.messages), 'turnItems': own, 'visibleTurnItems': visible,
                'providerSessions': [], 'runtimeRequests': [], 'plans': [],
                'attempts': [], 'nodes': [], 'subagents': [], 'providerThreads': [], 'providerTurns': [],
                'checkpointScopes': [], 'checkpoints': [], 'contextHandoffs': [], 'contextTransfers': []}
