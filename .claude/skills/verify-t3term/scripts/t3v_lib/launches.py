"""T3's `orchestration.launchThread` for a scenario that serves it, so `t3term launch` makes a thread.

It answers as T3 Code does at f570bd21: the handler in apps/server/src/ws.ts:1934-2012, the launch
in apps/server/src/orchestration-v2/ThreadLaunchService.ts:684-906, and the input and result in
packages/contracts/src/orchestrationV2.ts:3177-3205. docs/thread-launch.md records the contract
t3term follows.

- The session needs orchestration:operate (RpcAuthorization.ts:53), which auth.py's SCOPE_FOR
  checks before the handler runs.
- A project the world doesn't have fails with OrchestrationV2ThreadLaunchError, "Failed to launch
  thread", before anything is made, as T3's resolve-project step does.
- A command id the server accepted before gets the thread that launch made, with `resumed: true`.
  T3 doesn't compare the payloads, and neither does this.
- A new command id with a thread id the world already has fails the same way, as T3's
  create-thread step does.
- Otherwise it makes the thread with the title, model, modes and the branch or worktree path the
  workspace names, then the first message and a run for it in `preparing`, and answers
  `{threadId, projection, resumed: false}`. The shell gets a thread.updated, so the sidebar and
  `t3term threads` list it. The fixture prepares no workspace and starts no agent, so the run stays
  `preparing` until `t3v run THREAD STATUS` moves it.
- A payload without a field T3's input requires fails as a defect, and `check` counts it in
  launchesUnreadable, because T3 would refuse it before the launch runs.

`t3v launches MODE` sets how later launches are answered: `accept` as above; `refuse` fails each
one with T3's launch error before it makes anything, as T3 does while it clones the project
(ProjectCloneTracker.rejectCommandsDuringClone); `drop` makes the thread, then closes the socket
without an answer, as a connection lost after T3 launched.

The log keeps the ids, the model, the modes, the workspace's kind, and the lengths and a hash of
the title, the message, the base ref and the branch, never their text.
"""

from __future__ import annotations

import copy

from .server import DESCRIBERS
from .text import iso, now_ms, safe, sha
from .world import Thread

LAUNCH = 'orchestration.launchThread'
LAUNCH_ERROR = 'OrchestrationV2ThreadLaunchError'
LAUNCH_MODES = ('accept', 'refuse', 'drop')
WORKSPACES = ('root', 'existing_worktree', 'worktree')


def measured(value, digest: bool = True):
    """The length of a string and the first 12 hex digits of its hash, or its JSON type."""
    if not isinstance(value, str):
        return None if value is None else type(value).__name__
    return {'chars': len(value), 'sha': sha(value)[:12]} if digest else len(value)


def describe_launch(payload: dict) -> dict:
    """What the log keeps of a launch: ids, the model, the modes and the workspace's kind, and
    only the lengths and hashes of what was typed."""
    def text(key, source=payload):
        value = source.get(key)
        return safe(value, 80) if isinstance(value, str) else None

    selection = payload.get('modelSelection') if isinstance(payload.get('modelSelection'), dict) else {}
    workspace = payload.get('workspaceStrategy') if isinstance(payload.get('workspaceStrategy'), dict) else {}
    message = payload.get('initialMessage') if isinstance(payload.get('initialMessage'), dict) else None
    options = selection.get('options')
    described = {
        'commandId': text('commandId'), 'threadId': text('threadId'), 'projectId': text('projectId'),
        'title': measured(payload.get('title')), 'generateTitle': payload.get('generateTitle'),
        'instanceId': text('instanceId', selection), 'model': text('model', selection),
        'modelOptions': len(options) if isinstance(options, list) else None,
        'runtimeMode': text('runtimeMode'), 'interactionMode': text('interactionMode'),
        'workspace': text('type', workspace), 'baseRef': measured(workspace.get('baseRef')),
        'branch': measured(workspace.get('branch')), 'worktreePath': measured(workspace.get('worktreePath')),
        'startFromOrigin': workspace.get('startFromOrigin'), 'creationSource': text('creationSource'),
        'reuseExistingThread': payload.get('reuseExistingThread'),
    }
    if message is not None:
        attachments = message.get('attachments')
        described.update(messageId=text('messageId', message), text=measured(message.get('text')),
                         attachments=len(attachments) if isinstance(attachments, list) else None,
                         context='context' in message)
    return {key: value for key, value in described.items() if value is not None}


DESCRIBERS[LAUNCH] = describe_launch


def unreadable(payload: dict):
    """The first field T3's input requires that the payload lacks or holds in the wrong type, or
    None."""
    def string(value) -> bool:
        return isinstance(value, str) and value.strip() != ''

    for key in ('commandId', 'projectId', 'title', 'runtimeMode', 'interactionMode'):
        if not string(payload.get(key)):
            return key
    if 'threadId' in payload and not string(payload['threadId']):
        return 'threadId'
    selection = payload.get('modelSelection')
    if not isinstance(selection, dict) or not string(selection.get('instanceId')) \
            or not string(selection.get('model')):
        return 'modelSelection'
    workspace = payload.get('workspaceStrategy')
    if not isinstance(workspace, dict) or workspace.get('type') not in WORKSPACES:
        return 'workspaceStrategy'
    need = {'worktree': 'baseRef', 'existing_worktree': 'worktreePath'}.get(workspace['type'])
    if need and not string(workspace.get(need)):
        return f'workspaceStrategy.{need}'
    if 'branch' in workspace and not string(workspace['branch']):
        return 'workspaceStrategy.branch'
    message = payload.get('initialMessage')
    if message is not None:
        if not isinstance(message, dict) or not isinstance(message.get('text'), str) \
                or not isinstance(message.get('attachments'), list):
            return 'initialMessage'
        if 'messageId' in message and not string(message['messageId']):
            return 'initialMessage.messageId'
    return None


class Launches:
    """The launch half of a scenario. The class that mixes it in is an Orchestration, which
    provides the world, `lock`, `log`, `counts`, `rpc`, `controls` and `push_shell`, and calls
    `init_launches` from its __init__."""

    def init_launches(self):
        self.launch_mode = 'accept'
        self.launched = {}  # command id -> the thread id that launch made
        self.launch_numbers = 0  # for thread ids the fixture picks when the payload has none
        self.rpc[LAUNCH] = self.launch_thread
        self.controls['/__fixture/launches'] = lambda request, _: self.set_launch_mode(str(request.get('mode') or ''))

    def set_launch_mode(self, mode: str):
        with self.lock:
            if mode not in LAUNCH_MODES:
                return None, f'launches takes {", ".join(LAUNCH_MODES)}'
            self.ensure_world()
            self.launch_mode = mode
            self.log('launch.mode', mode=mode)
            note = {'accept': 'From now on orchestration.launchThread makes each new thread and answers with it',
                    'refuse': ("From now on orchestration.launchThread fails each launch with T3's launch error "
                               'before it makes anything, as T3 does while it clones the project'),
                    'drop': ('From now on orchestration.launchThread makes each new thread, then closes the '
                             'socket without an answer')}[mode]
            return self.answer(note), None

    def refuse_launch(self, connection, request_id: str, payload: dict, reason: str):
        with self.lock:
            self.counts['launch.refused'] += 1
        self.log('launch.refused', conn=connection.id, requestId=request_id, reason=reason)
        connection.fail(request_id, LAUNCH, {'_tag': LAUNCH_ERROR, 'commandId': payload.get('commandId'),
                                             'projectId': payload.get('projectId'),
                                             'message': 'Failed to launch thread'})

    def launch_thread(self, connection, request_id: str, payload: dict):
        with self.lock:
            self.ensure_world()
            missing = unreadable(payload)
            if missing is not None:
                self.counts['launch.unreadable'] += 1
                self.log('launch.unreadable', conn=connection.id, requestId=request_id, field=missing)
                connection.send_json({'_tag': 'Exit', 'requestId': request_id, 'exit': {'_tag': 'Failure', 'cause': [
                    {'_tag': 'Die', 'defect': f'The t3v fixture could not read the launch: {missing} is missing or '
                                              'not what T3 takes.'}]}})
                return
            command = payload['commandId']
            done = self.launched.get(command)
            if done is not None:
                thread = self.threads[done]
                self.counts['launch.resumed'] += 1
                self.log('launch.resumed', conn=connection.id, requestId=request_id, threadId=done)
                connection.exit_success(request_id, {'threadId': done, 'projection': thread.projection(),
                                                     'resumed': True})
                return
            if self.launch_mode == 'refuse':
                self.refuse_launch(connection, request_id, payload, 'refuse mode, as while T3 clones the project')
                return
            project = next((p for p in self.projects if p['id'] == payload['projectId']), None)
            if project is None:
                self.refuse_launch(connection, request_id, payload, 'no such project')
                return
            if payload.get('reuseExistingThread') is True:
                self.refuse_launch(connection, request_id, payload, 'the fixture reuses no thread')
                return
            tid = payload.get('threadId')
            if tid is None:
                self.launch_numbers += 1
                tid = f'thr-launch-{self.launch_numbers}'
            if tid in self.threads:
                self.refuse_launch(connection, request_id, payload, 'the thread id is taken')
                return
            thread = self.make_launched(tid, project, payload)
            self.launched[command] = tid
            self.threads[tid] = thread
            self.order.append(tid)
            shells = self.push_shell(thread)
            run = thread.latest_run()
            drop = self.launch_mode == 'drop'
            self.counts['launch.dropped' if drop else 'launch.accepted'] += 1
            self.log('launch.dropped' if drop else 'launch.accepted', conn=connection.id, requestId=request_id,
                     threadId=tid, runId=run['id'] if run else None, runStatus=run['status'] if run else None,
                     workspace=payload['workspaceStrategy']['type'], shellStreams=shells)
            if drop:
                connection.close()
                return
            connection.exit_success(request_id, {'threadId': tid, 'projection': thread.projection(), 'resumed': False})

    def make_launched(self, tid: str, project: dict, payload: dict) -> Thread:
        """The thread T3 makes for a launch: the thread itself, then the first message and the run
        for it in `preparing`, as ThreadLaunchService.launch dispatches them."""
        stamp = iso(now_ms())
        thread = Thread(tid, project, payload['title'].strip(), stamp, None)
        if not tid.startswith('thr-'):
            thread.short = tid
        workspace = payload['workspaceStrategy']
        selection = copy.deepcopy(payload['modelSelection'])
        thread.set_fields({
            'branch': workspace.get('branch'),
            'worktreePath': workspace.get('worktreePath') if workspace['type'] == 'existing_worktree' else None,
            'providerInstanceId': selection['instanceId'], 'modelSelection': selection,
            'runtimeMode': payload['runtimeMode'], 'interactionMode': payload['interactionMode'],
            'activeProviderThreadId': None,
        }, 'both')
        message = payload.get('initialMessage')
        if message is None:
            return thread
        message_id = message.get('messageId') or f'msg-{thread.short}-1'
        run = {'id': thread.run_id(1), 'threadId': tid, 'ordinal': 1, 'providerInstanceId': selection['instanceId'],
               'modelSelection': copy.deepcopy(selection), 'providerThreadId': None, 'userMessageId': message_id,
               'rootNodeId': thread.root(1), 'activeAttemptId': None, 'status': 'preparing', 'requestedAt': stamp,
               'startedAt': None, 'completedAt': None, 'checkpointId': None, 'contextHandoffId': None}
        thread.change('runs', run)
        thread.change('messages', {'id': message_id, 'role': 'user', 'runId': run['id'], 'text': message['text'],
                                   'createdAt': stamp})
        thread.change('items', thread.item(1, 'prompt-1', stamp, type='user_message', text=message['text']))
        thread.settle_record()
        return thread


def status_fields(records: list, live) -> dict:
    """The launches' part of `t3v status`."""
    def count(event: str) -> int:
        return sum(1 for r in records if r.get('event') == event)

    return {
        'launchMode': live.get('launchMode') if live else None,
        'launches': {'accepted': count('launch.accepted'), 'resumed': count('launch.resumed'),
                     'refused': count('launch.refused'), 'dropped': count('launch.dropped')},
        'launchesUnreadable': count('launch.unreadable'),
    }
