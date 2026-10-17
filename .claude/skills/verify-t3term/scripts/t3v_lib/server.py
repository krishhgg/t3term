"""The fake T3 server: HTTP routes, WebSocket framing and Effect RPC, the way src/rpc.rs speaks it.

It listens on 127.0.0.1 alone, at a port the OS picks. It serves the environment descriptor, the
auth routes t3term calls, the orchestration HTTP routes and Effect RPC over /ws. It runs no
command, no git, no gh and no shell, and it refuses every orchestration command. A scenario
(scenarios/) adds its world, its RPC methods (`rpc`), its control routes (`controls`) and its
HTTP routes (`http`) to the base here. An RPC method nothing registered fails with
FixtureUnknownTag and counts in unknownRpcs.
"""

from __future__ import annotations

import base64
import copy
import hashlib
import hmac
import http.server
import itertools
import json
import os
import platform
import queue
import signal
import socket
import socketserver
import struct
import sys
import threading
import time
import urllib.parse
from collections import Counter
from pathlib import Path

from .auth import SCOPE_FOR, Auth, ticket_answer
from .runs import read_json, runtime_file, write_json
from .text import MISSING, encode, iso, now_ms, safe

WS_GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11'
MAX_FRAME = 8 * 1024 * 1024
SERVER_VERSION = '0.0.46-t3v'
FIRST_SEQUENCE = 100


class FixtureBase(Auth):
    """State shared by every connection: sessions, tickets, counts and the event log. Every
    change holds `lock`. A scenario subclasses it, builds its world in `build_world` and
    registers what it serves in `rpc`, `controls` and `http`."""

    def __init__(self, root: Path, run: dict):
        self.root = root
        self.run = run
        self.options = run.get('options') or {}
        self.evidence = Path(run['evidence'])
        self.lock = threading.RLock()
        self.control_key = (root / 'control.key').read_text().strip()
        self.secret_values = {self.control_key}
        self.init_auth()
        self.counts = Counter()
        self.maxima = Counter()
        self.rpc_tags = Counter()
        self.connections = set()
        self.connection_ids = itertools.count(1)
        self.ws_hold_until = 0  # /ws answers 503 until this wall-clock millisecond
        self.sequence = FIRST_SEQUENCE
        self.t0 = None
        self.port = None
        self.timers = []
        self.log_path = self.evidence / 'events.jsonl'
        # tag -> handler(connection, request id, payload)
        self.rpc = {'orchestration.dispatchCommand': self.dispatch}
        # path -> handler(request, the path's last part) -> (answer, None) or (None, refusal). A
        # path that ends in / matches every path under it.
        self.controls = {
            '/__fixture/auth/permissions': lambda request, _: self.set_permissions(request),
            '/__fixture/state': lambda request, _: (self.state(), None),
        }

    # -- log --

    def scrub(self, text: str) -> str:
        for value in self.secret_values:
            if value and value in text:
                text = text.replace(value, '<redacted>')
        return text

    def log(self, event: str, **fields):
        stamp = now_ms()
        record = {'wall': iso(stamp), 'wallMs': stamp, 'event': event}
        if self.t0 is not None:
            record['sinceWorldMs'] = stamp - self.t0
        record.update({k: v for k, v in fields.items() if v is not None})
        with self.lock:
            line = self.scrub(json.dumps(record))
            fd = os.open(self.log_path, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
            try:
                os.write(fd, (line + '\n').encode())
            finally:
                os.close(fd)
            print(line, file=sys.stderr, flush=True)

    def most(self, name: str, value: int):
        if value > self.maxima[name]:
            self.maxima[name] = value

    def later(self, seconds: float, action):
        timer = threading.Timer(max(0.0, seconds), action)
        timer.daemon = True
        timer.start()
        with self.lock:
            self.timers = [t for t in self.timers if t.is_alive()]
            self.timers.append(timer)

    # -- world --

    def build_world(self, t0: int) -> dict:
        """The scenario's world: {'projects', 'threads', 'order'}."""
        return {'projects': [], 'threads': {}, 'order': []}

    def ensure_world(self):
        with self.lock:
            if self.t0 is not None:
                return
            self.t0 = now_ms()
            self.world_built(self.build_world(self.t0))
            write_json(self.root / 'world.json', {'t0Ms': self.t0, 't0': iso(self.t0)})

    def world_built(self, world: dict):
        self.log('world.built', scenario=self.run.get('scenario'))

    # -- the descriptor --

    def descriptor_label(self):
        """The descriptor's label, or MISSING to leave it out."""
        return 'Fixture Mac'

    def capabilities(self) -> dict:
        return {'threadSettlement': True, 'threadSnooze': True, 'serverResolvedCommandContext': True}

    def descriptor(self) -> dict:
        value = {
            'environmentId': self.run['environmentId'],
            'label': self.descriptor_label(),
            'platform': {'os': sys.platform, 'arch': platform.machine()},
            'serverVersion': SERVER_VERSION,
            'orchestrationProtocolVersion': 2,
            'capabilities': self.capabilities(),
        }
        if value['label'] is MISSING:
            del value['label']
        return value

    def note_descriptor_read(self):
        with self.lock:
            self.counts['descriptor.read'] += 1
            self.log('descriptor.read')

    # -- hooks a scenario overrides --

    def http(self, handler, method: str, path: str, body: bytes, session: dict, auth: str) -> bool:
        """Answers an HTTP route past auth, or returns False for a 404."""
        return False

    def on_chunk(self, connection, request_id: str, stream: str, values: list, delivered: bool, thread_id=None):
        """Called for each chunk a connection sends with `Connection.chunk`."""

    def on_ack(self, connection, request_id: str):
        """t3term acknowledged a chunk of stream `request_id`."""

    def on_interrupt(self, connection, request_id: str, tag):
        """t3term interrupted stream `request_id`, whose method was `tag`."""

    def on_close(self, connection):
        """A socket closed. Forget its streams."""

    # -- commands --

    def dispatch(self, connection, request_id: str, command: dict):
        """Refuses every command. The log keeps its type and thread, never its text."""
        kind, tid = command.get('type'), command.get('threadId')
        with self.lock:
            self.counts['command.refused'] += 1
        self.log('command.refused', conn=connection.id, requestId=request_id,
                 type=safe(kind, 60) if isinstance(kind, str) else None,
                 threadId=safe(tid, 60) if isinstance(tid, str) else None)
        connection.exit_failure(request_id, 'orchestration.dispatchCommand', 'FixtureReadOnly',
                                'The t3v fixture takes no commands. Nothing was sent anywhere.')

    def drop_sockets(self, hold: float) -> int:
        """Closes every WebSocket and answers /ws with 503 for `hold` seconds."""
        with self.lock:
            self.ws_hold_until = now_ms() + int(hold * 1000)
            connections = list(self.connections)
            for connection in connections:
                self.on_close(connection)
        for connection in connections:
            connection.close()
        return len(connections)

    # -- reports --

    def report(self) -> dict:
        """The server's own numbers, for `status` and `check`."""
        with self.lock:
            return {'rpcRequestsByTag': dict(self.rpc_tags), 'counts': dict(self.counts),
                    'maxima': dict(self.maxima), 'sessions': self.session_list(),
                    'openSockets': len(self.connections)}

    def state(self) -> dict:
        """What /__fixture/state answers: the scenario adds what t3term should show."""
        return {'server': self.report()}

    def summary(self, open_at_stop: int) -> dict:
        with self.lock:
            return dict(self.report(), run=self.root.name, scenario=self.run.get('scenario'),
                        options=copy.deepcopy(self.options), t0=iso(self.t0) if self.t0 else None,
                        stoppedAt=iso(now_ms()), openSocketsAtStop=open_at_stop)


def unmask(data: bytes, mask: bytes) -> bytes:
    if not data:
        return data
    key = (mask * (len(data) // 4 + 1))[:len(data)]
    return (int.from_bytes(data, 'big') ^ int.from_bytes(key, 'big')).to_bytes(len(data), 'big')


# Fields of a request's payload worth logging, by method. Message text and anything unknown stay
# out. A scenario may add a function for a method in DESCRIBERS.
LOGGED_FIELDS = {
    'orchestration.subscribeShell': ('afterSequence', 'requestCompletionMarker'),
    'orchestration.subscribeThread': ('threadId', 'afterSequence', 'requestCompletionMarker',
                                      'acceptBoundedSnapshot'),
    'subscribeWorktreeSetup': ('threadId',),
    'orchestration.dispatchCommand': ('type', 'threadId'),
}
DESCRIBERS = {}


def describe(tag: str, payload: dict) -> dict:
    """The payload fields worth logging."""
    if tag in DESCRIBERS:
        return DESCRIBERS[tag](payload)
    keep = LOGGED_FIELDS.get(tag)
    if keep is None:
        return {'keys': sorted(safe(k, 40) for k in payload)}
    return {k: payload[k] if not isinstance(payload[k], str) else safe(payload[k], 80) for k in keep if k in payload}


class Connection:
    """One WebSocket carrying Effect RPC, the way src/rpc.rs speaks it. A writer thread sends
    what `send_frame` queues, in order, so nothing that holds the fixture's lock ever waits on a
    socket that t3term isn't reading. The socket carries the session its ticket named, and with
    it that session's scopes."""

    def __init__(self, fixture: FixtureBase, handler, session: dict):
        self.fixture = fixture
        self.rfile, self.wfile, self.sock = handler.rfile, handler.wfile, handler.connection
        self.session = session
        self.id = next(fixture.connection_ids)
        self.open = True
        self.streams = {}  # request id -> tag, for the streams still open
        self.outbox = queue.Queue()
        self.writer = threading.Thread(target=self.write_loop, daemon=True)

    @property
    def kind(self) -> str:
        return self.session['kind']

    def send_frame(self, opcode: int, payload: bytes) -> bool:
        """Queues one frame. False once the socket closed."""
        header = bytearray([0x80 | opcode])
        size = len(payload)
        if size < 126:
            header.append(size)
        elif size < 65536:
            header.append(126)
            header += struct.pack('!H', size)
        else:
            header.append(127)
            header += struct.pack('!Q', size)
        if not self.open:
            return False
        self.outbox.put((bytes(header), payload))
        return True

    def write_loop(self):
        while True:
            frame = self.outbox.get()
            if frame is None:
                return
            try:
                self.wfile.write(frame[0])
                self.wfile.write(frame[1])
            except OSError:
                self.open = False
                return

    def send_json(self, value) -> bool:
        return self.send_frame(0x1, encode(value))

    def chunk(self, request_id: str, stream: str, values: list, thread_id=None):
        delivered = self.send_json({'_tag': 'Chunk', 'requestId': request_id, 'values': values})
        self.fixture.on_chunk(self, request_id, stream, values, delivered, thread_id)

    def exit_success(self, request_id: str, value) -> bool:
        self.streams.pop(request_id, None)
        return self.send_json({'_tag': 'Exit', 'requestId': request_id, 'exit': {'_tag': 'Success', 'value': value}})

    def fail(self, request_id: str, tag: str, error: dict) -> bool:
        """Ends a request with T3's error `error`, as Effect RPC sends a failure."""
        self.streams.pop(request_id, None)
        delivered = self.send_json({'_tag': 'Exit', 'requestId': request_id, 'exit': {'_tag': 'Failure', 'cause': [
            {'_tag': 'Fail', 'error': error}]}})
        self.fixture.log('rpc.refused', conn=self.id, kind=self.kind, requestId=request_id, tag=safe(tag, 60),
                         errorTag=error.get('_tag'), delivered=delivered)
        return delivered

    def exit_failure(self, request_id: str, tag: str, error_tag: str, message: str) -> bool:
        return self.fail(request_id, tag, {'_tag': error_tag, 'message': message})

    def read_exact(self, size: int) -> bytes:
        parts, need = [], size
        while need:
            part = self.rfile.read(need)
            if not part:
                raise EOFError('socket ended')
            parts.append(part)
            need -= len(part)
        return b''.join(parts)

    def receive(self):
        """The next data message as (kind, bytes). Answers pings itself; kind 'close' ends."""
        parts, first = [], None
        while True:
            b1, b2 = self.read_exact(2)
            fin, opcode = b1 & 0x80, b1 & 0x0F
            size = b2 & 0x7F
            if size == 126:
                size = struct.unpack('!H', self.read_exact(2))[0]
            elif size == 127:
                size = struct.unpack('!Q', self.read_exact(8))[0]
            if size > MAX_FRAME:
                raise ValueError(f'frame of {size} bytes')
            mask = self.read_exact(4) if b2 & 0x80 else b''
            data = self.read_exact(size) if size else b''
            if mask:
                data = unmask(data, mask)
            if opcode == 0x8:
                return 'close', data
            if opcode == 0x9:
                self.send_frame(0xA, data)
                continue
            if opcode >= 0x8:
                continue
            if opcode:
                first, parts = opcode, [data]
            else:
                parts.append(data)
            if fin:
                return ('text' if first == 0x1 else 'binary'), b''.join(parts)

    def serve(self):
        fixture = self.fixture
        self.writer.start()
        with fixture.lock:
            fixture.connections.add(self)
            fixture.counts['ws.open'] += 1
            fixture.counts[f'ws.open.{self.kind}'] += 1
        fixture.log('ws.open', conn=self.id, kind=self.kind, sessionId=self.session['id'])
        reason = 'closed'
        try:
            while True:
                kind, data = self.receive()
                if kind == 'close':
                    self.send_frame(0x8, data[:2])
                    reason = 'client closed'
                    break
                try:
                    decoded = json.loads(data.decode('utf-8'))
                except ValueError:
                    with fixture.lock:
                        fixture.counts['rpc.unreadable'] += 1
                    fixture.log('rpc.unreadable', conn=self.id, bytes=len(data))
                    continue
                for message in decoded if isinstance(decoded, list) else [decoded]:
                    if isinstance(message, dict):
                        self.handle(message)
        except (EOFError, OSError, ValueError) as error:
            reason = f'{type(error).__name__}: {error}'
        finally:
            self.open = False
            self.outbox.put(None)
            fixture.on_close(self)
            with fixture.lock:
                fixture.connections.discard(self)
                fixture.counts['ws.close'] += 1
                fixture.counts[f'ws.close.{self.kind}'] += 1
            fixture.log('ws.close', conn=self.id, kind=self.kind, reason=reason)

    def close(self):
        self.open = False
        self.outbox.put(None)
        try:
            self.sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass

    def handle(self, message: dict):
        fixture = self.fixture
        tag = message.get('_tag')
        if tag == 'Ping':
            with fixture.lock:
                fixture.counts['rpc.ping'] += 1
            self.send_json({'_tag': 'Pong'})
        elif tag == 'Ack':
            with fixture.lock:
                fixture.counts['rpc.ack'] += 1
                fixture.counts[f'rpc.ack.{self.kind}'] += 1
            fixture.on_ack(self, str(message.get('requestId')))
        elif tag == 'Interrupt':
            request_id = str(message.get('requestId'))
            stream = self.streams.pop(request_id, None)
            fixture.on_interrupt(self, request_id, stream)
            with fixture.lock:
                fixture.counts['rpc.interrupt'] += 1
                fixture.counts[f'rpc.interrupt.{self.kind}'] += 1
            fixture.log('rpc.interrupt', conn=self.id, kind=self.kind, requestId=request_id, tag=stream)
            self.send_json({'_tag': 'Exit', 'requestId': request_id,
                            'exit': {'_tag': 'Failure', 'cause': [{'_tag': 'Interrupt'}]}})
        elif tag == 'Request':
            payload = message.get('payload') if isinstance(message.get('payload'), dict) else {}
            self.request(str(message.get('id')), str(message.get('tag')), payload)
        else:
            fixture.log('rpc.unknown_message', conn=self.id, messageTag=safe(tag, 40))

    def request(self, request_id: str, tag: str, payload: dict):
        fixture = self.fixture
        with fixture.lock:
            fixture.rpc_tags[tag] += 1
        fixture.log('rpc.request', conn=self.id, kind=self.kind, requestId=request_id, tag=safe(tag, 60),
                    payload=describe(tag, payload))
        need = SCOPE_FOR.get(tag)
        if need and need not in self.session['scopes']:
            # RpcAuthorization.ts: the request fails before the handler runs.
            with fixture.lock:
                fixture.counts['scopeViolations'] += 1
                fixture.counts[f'scope.refused.{self.kind}.{tag}'] += 1
            self.fail(request_id, tag, {'_tag': 'EnvironmentAuthorizationError',
                                        'message': f'The authenticated token is missing required scope: {need}.',
                                        'requiredScope': need})
            return
        handler = fixture.rpc.get(tag)
        if handler is None:
            with fixture.lock:
                fixture.counts['unknownRpcs'] += 1
                fixture.counts[f'rpc.unknown.{safe(tag, 60)}'] += 1
            self.fail(request_id, tag, {'_tag': 'FixtureUnknownTag',
                                        'message': f'The t3v fixture does not serve {safe(tag, 60)}.'})
            return
        handler(self, request_id, payload)


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    server_version = 'T3vFixture'
    sys_version = ''
    fixture = None  # set by serve()

    def log_message(self, format, *args):
        # The default prints the request line, and the WebSocket's carries its ticket.
        pass

    def do_GET(self):
        self.route('GET')

    def do_POST(self):
        self.route('POST')

    def do_PUT(self):
        self.route('PUT')

    def do_DELETE(self):
        self.route('DELETE')

    def read_body(self) -> bytes:
        size = int(self.headers.get('Content-Length') or 0)
        return self.rfile.read(size) if size > 0 else b''

    def reply(self, method: str, path: str, status: int, body, auth: str):
        data = json.dumps(body).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)
        self.fixture.log('http', method=method, path=path, status=status, auth=auth)

    def bearer(self):
        header = self.headers.get('Authorization') or ''
        if not header.startswith('Bearer '):
            return None, None, 'missing'
        token = header[len('Bearer '):].strip()
        session = self.fixture.sessions.get(token)
        if session is None:
            return token, None, 'unknown'
        if session['revoked']:
            return token, None, 'revoked'
        return token, session, f'ok-{session["kind"]}'

    def keyed(self) -> bool:
        key = (self.headers.get('X-Fixture-Control') or '').encode()
        return hmac.compare_digest(key, self.fixture.control_key.encode())

    def route(self, method: str):
        fixture = self.fixture
        path, _, query = self.path.partition('?')
        body = self.read_body()
        if path == '/ws' and method == 'GET':
            return self.websocket(urllib.parse.parse_qs(query))
        if path == '/.well-known/t3/environment' and method == 'GET':
            if self.keyed():
                # `t3v doctor` reads it with the control key, so what t3term should show keeps
                # the descriptor t3term read.
                return self.reply(method, path, 200, fixture.descriptor(), 'probe')
            fixture.note_descriptor_read()
            return self.reply(method, path, 200, fixture.descriptor(), 'none')
        if path.startswith('/__fixture/'):
            return self.control(method, path, body)
        token, session, auth = self.bearer()
        if path == '/api/auth/session' and method == 'GET':
            return self.reply(method, path, 200, fixture.session_state(session), auth)
        if session is None:
            return self.reply(method, path, 401, {'message': 'The fixture does not accept this bearer session.'}, auth)
        if path == '/api/auth/websocket-ticket' and method == 'POST':
            ticket = fixture.issue_ticket(token)
            with fixture.lock:
                fixture.counts[f'auth.ticket.{session["kind"]}'] += 1
            return self.reply(method, path, 200, ticket_answer(ticket), auth)
        if path.startswith('/api/orchestration/'):
            if 'orchestration:read' not in session['scopes']:
                with fixture.lock:
                    fixture.counts['scopeViolations'] += 1
                    fixture.counts[f'scope.refused.{session["kind"]}.http'] += 1
                return self.reply(method, path, 403, {'message': 'The token is missing orchestration:read.'}, auth)
            if self.headers.get('x-t3-orchestration-protocol') != '2':
                return self.reply(method, path, 400, {'message': 'Orchestration routes need protocol 2.'}, auth)
        if fixture.http(self, method, path, body, session, auth):
            return None
        return self.reply(method, path, 404, {'message': 'The t3v fixture has nothing here.'}, auth)

    def control(self, method: str, path: str, body: bytes):
        fixture = self.fixture
        if method != 'POST' or not self.keyed():
            return self.reply(method, path, 403, {'message': 'Fixture control needs the run key.'}, 'control-denied')
        try:
            request = json.loads(body or b'{}')
        except ValueError:
            return self.reply(method, path, 400, {'message': 'Fixture control takes JSON.'}, 'control')
        if not isinstance(request, dict):
            return self.reply(method, path, 400, {'message': 'Fixture control takes a JSON object.'}, 'control')
        if path == '/__fixture/auth/issue':
            return self.reply(method, path, 200, fixture.issue_session(request), 'control')
        if path == '/__fixture/auth/revoke':
            found = fixture.revoke_session(str(request.get('sessionId') or ''))
            return self.reply(method, path, 200 if found else 404, {'revoked': found}, 'control')
        # An exact path, else a prefix such as /__fixture/thread/ that takes the last part as
        # its action.
        head, _, name = path.rpartition('/')
        handler = fixture.controls.get(path) or fixture.controls.get(head + '/')
        if handler is None:
            return self.reply(method, path, 404, {'message': 'No such fixture control.'}, 'control')
        done, refusal = handler(request, name)
        if done is None:
            return self.reply(method, path, 409, {'message': refusal}, 'control')
        return self.reply(method, path, 200, done, 'control')

    def websocket(self, query: dict):
        fixture = self.fixture
        ticket = (query.get('wsTicket') or [''])[0]
        protocol = (query.get('orchestrationProtocol') or [''])[0]
        session = fixture.take_ticket(ticket) if ticket else None
        key = self.headers.get('Sec-WebSocket-Key')
        if now_ms() < fixture.ws_hold_until:
            with fixture.lock:
                fixture.counts['ws.held'] += 1
            return self.reply('GET', '/ws', 503, {'message': 'The fixture is holding sockets closed.'}, 'held')
        if session is None:
            return self.reply('GET', '/ws', 401, {'message': 'The WebSocket ticket is missing, used or revoked.'},
                              'ticket-rejected')
        if protocol != '2':
            return self.reply('GET', '/ws', 400, {'message': 'The socket needs orchestrationProtocol=2.'}, 'ticket-ok')
        if not key or 'websocket' not in (self.headers.get('Upgrade') or '').lower():
            return self.reply('GET', '/ws', 400, {'message': 'Not a WebSocket upgrade.'}, 'ticket-ok')
        accept = base64.b64encode(hashlib.sha1((key + WS_GUID).encode()).digest()).decode()
        self.send_response(101, 'Switching Protocols')
        self.send_header('Upgrade', 'websocket')
        self.send_header('Connection', 'Upgrade')
        self.send_header('Sec-WebSocket-Accept', accept)
        self.end_headers()
        self.close_connection = True
        fixture.log('http', method='GET', path='/ws', status=101, auth=f'ticket-ok-{session["kind"]}')
        Connection(fixture, self, session).serve()


class Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True
    block_on_close = False

    def server_bind(self):
        # HTTPServer's own also sets server_name from socket.getfqdn, a reverse DNS lookup of
        # 127.0.0.1. On CI's macOS runners it hung past start's 10 s for every server after the
        # first. Nothing here reads the name.
        socketserver.TCPServer.server_bind(self)
        self.server_name = '127.0.0.1'
        self.server_port = self.server_address[1]


def serve(root: Path, run: dict, fixture_class) -> int:
    """Runs the server in the foreground until a signal or --max-seconds, then writes
    summary.json to the evidence directory."""
    from .tmux import quit_t3term, tmux

    fixture = fixture_class(root, run)
    Handler.fixture = fixture
    server = Server(('127.0.0.1', 0), Handler)
    port = server.server_address[1]
    fixture.port = port
    origin = f'http://127.0.0.1:{port}'
    runtime = runtime_file(root)
    # The shape tests/cli.rs writes, after apps/server/src/serverRuntimeState.ts.
    write_json(runtime, {'version': 1, 'pid': os.getpid(), 'port': port, 'origin': origin,
                         'startedAt': iso(now_ms())})
    (root / 'server.pid').write_text(f'{os.getpid()}\n')
    stop = threading.Event()
    reason = {'why': 'signal'}
    for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(sig, lambda *_: stop.set())
    threading.Thread(target=server.serve_forever, kwargs={'poll_interval': 0.2}, daemon=True).start()
    fixture.ensure_world()
    fixture.log('server.listening', origin=origin, pid=os.getpid(), options=fixture.options)
    max_seconds = float(run.get('maxSeconds') or 7200)
    deadline = time.monotonic() + max_seconds
    while not stop.wait(0.5):
        if time.monotonic() >= deadline:
            reason['why'] = f'--max-seconds {max_seconds:g} ran out'
            state = read_json(root / 'run.json') or run
            if state.get('tmux'):
                # Quit t3term while the server can still take its revoke.
                quit_t3term(state, 'max-seconds')
                tmux(state, 'kill-server')
            break
    fixture.log('server.stopping', reason=reason['why'])
    server.shutdown()
    server.server_close()
    # t3term closes its sockets as it quits. Give the server a moment to read their ends, so
    # openSocketsAtStop counts only sockets left open.
    drain = time.monotonic() + 2
    while time.monotonic() < drain and fixture.connections:
        time.sleep(0.05)
    with fixture.lock:
        left = list(fixture.connections)
    for connection in left:
        connection.close()
    for timer in list(fixture.timers):
        timer.cancel()
    try:
        runtime.unlink()
    except OSError:
        pass
    write_json(fixture.evidence / 'summary.json', fixture.summary(len(left)))
    fixture.log('server.stopped', openSocketsAtStop=len(left))
    return 0
