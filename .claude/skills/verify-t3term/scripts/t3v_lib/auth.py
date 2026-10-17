"""Sessions, tickets and scopes, and the `t3` stand-in that issues and revokes sessions.

t3term asks `t3 auth session issue` for a session, finding the command through
T3TERM_T3_COMMAND. The run's `t3` shim answers it by asking the run's server over a loopback
control route guarded by the run's control key. Revoking only marks the session. Each session
keeps the scopes it was issued with, and every socket it opens carries it, so the server checks
each request against the scope T3 requires for it (SCOPE_FOR). Tokens and tickets never reach the
log: the server writes `<redacted>` in their place.
"""

from __future__ import annotations

import json
import os
import re
import secrets
import sys
import urllib.error
import urllib.request
from pathlib import Path

from .runs import opener, origin_of
from .text import iso, now_ms

# The scope each RPC needs, from T3's RpcAuthorization.ts at f570bd2. A request without it fails
# with EnvironmentAuthorizationError before its handler runs, and counts in scopeViolations. A
# method missing here needs no scope the fake checks.
SCOPE_FOR = {
    'orchestration.subscribeShell': 'orchestration:read',
    'orchestration.subscribeThread': 'orchestration:read',
    'orchestration.getTurnItem': 'orchestration:read',
    'server.getConfig': 'orchestration:read',
    'subscribeServerConfig': 'orchestration:read',
    'subscribeWorktreeSetup': 'orchestration:read',
    'orchestration.dispatchCommand': 'orchestration:operate',
    'terminal.open': 'terminal:operate',
    'terminal.attach': 'terminal:operate',
    'terminal.observe': 'terminal:read',
    'terminal.write': 'terminal:operate',
    'terminal.resize': 'terminal:operate',
    'terminal.clear': 'terminal:operate',
    'terminal.restart': 'terminal:operate',
    'terminal.close': 'terminal:operate',
    'subscribeTerminalEvents': 'terminal:read',
    'subscribeTerminalMetadata': 'terminal:read',
}
# What /api/auth/session says a terminal session may do: its own scopes (normal), one more
# (extra), without terminal:operate (missing) or no `permissions` field (absent).
PERMISSION_MODES = ('normal', 'extra', 'missing', 'absent')


def kind_of(scopes: list) -> str:
    """A session that asks for any terminal scope is a terminal session, else a login."""
    return 'terminal' if any(s.startswith('terminal:') for s in scopes) else 'login'


class Auth:
    """The sessions and tickets half of the fake server. The class that mixes it in provides
    `lock`, `log`, `counts`, `connections` and `secret_values`, and may override
    `session_started` and `session_ended`."""

    def init_auth(self):
        self.sessions = {}  # token -> {'id', 'scopes', 'kind', 'client', 'revoked'}
        self.tickets = {}  # ticket -> token, single use
        self.permissions = 'normal'

    def session_started(self, session: dict):
        """A new t3term session, which a scenario may use to start what it expects over."""

    def session_ended(self, session: dict):
        """t3term revoked `session`, as it does when it quits."""

    def issue_session(self, request: dict) -> dict:
        with self.lock:
            scopes = [s for s in request.get('scopes') or [] if isinstance(s, str)]
            kind = kind_of(scopes)
            session_id = f'sess-fixture-{len(self.sessions) + 1:02d}-{kind}'
            token = 'fixture-' + secrets.token_urlsafe(32)
            self.secret_values.add(token)
            client = 'cli' if request.get('client') == 'cli' else 'tui'
            session = {'id': session_id, 'scopes': scopes, 'kind': kind, 'client': client, 'revoked': False}
            self.sessions[token] = session
            self.counts[f'auth.issue.{kind}'] += 1
            self.log('auth.issue', sessionId=session_id, kind=kind, client=client, scopes=scopes,
                     ttl=str(request.get('ttl')), label=str(request.get('label')), subject=str(request.get('subject')))
            self.session_started(session)
            return {'sessionId': session_id, 'token': token}

    def revoke_session(self, session_id: str) -> bool:
        with self.lock:
            for session in self.sessions.values():
                if session['id'] == session_id:
                    already = session['revoked']
                    session['revoked'] = True
                    self.counts[f'auth.revoke.{session["kind"]}'] += 1
                    self.log('auth.revoke', sessionId=session_id, kind=session['kind'], alreadyRevoked=already,
                             openSockets=sum(1 for c in self.connections if c.session is session))
                    self.session_ended(session)
                    return True
            self.log('auth.revoke', sessionId=session_id, known=False)
            return False

    def session_state(self, session) -> dict:
        """What /api/auth/session says of `session`. `scopes` is the older field, which leaves out
        terminal:read (packages/contracts/src/auth.ts:131-149). `permissions` lists the session's
        scopes, or what `set_permissions` has it say for a terminal session."""
        with self.lock:
            if session is None:
                return {'authenticated': False}
            scopes = list(session['scopes'])
            answer = {'authenticated': True, 'scopes': [s for s in scopes if s != 'terminal:read']}
            mode = self.permissions if session['kind'] == 'terminal' else 'normal'
            if mode == 'extra':
                answer['permissions'] = scopes + ['orchestration:read']
            elif mode == 'missing':
                answer['permissions'] = [s for s in scopes if s != 'terminal:operate']
            elif mode != 'absent':
                answer['permissions'] = scopes
            self.counts[f'auth.check.{session["kind"]}'] += 1
            self.log('auth.check', sessionId=session['id'], kind=session['kind'], permissions=mode)
            return answer

    def set_permissions(self, request: dict):
        mode = str(request.get('mode') or '')
        with self.lock:
            if mode not in PERMISSION_MODES:
                return None, f'permissions takes {", ".join(PERMISSION_MODES)}'
            self.permissions = mode
            self.log('auth.permissions', mode=mode)
            return {'note': f'/api/auth/session now answers a terminal session with the {mode} permissions'}, None

    def issue_ticket(self, token: str) -> str:
        ticket = secrets.token_urlsafe(24)
        with self.lock:
            self.tickets[ticket] = token
            self.secret_values.add(ticket)
        return ticket

    def take_ticket(self, ticket: str):
        with self.lock:
            token = self.tickets.pop(ticket, None)
            session = self.sessions.get(token) if token else None
            return session if session and not session['revoked'] else None

    def session_list(self) -> list:
        """Each session without its token."""
        with self.lock:
            return [{'id': s['id'], 'kind': s['kind'], 'scopes': s['scopes'], 'revoked': s['revoked']}
                    for s in self.sessions.values()]


def shim(argv: list) -> int:
    """`t3 auth session issue|revoke` for t3term, answered by this run's server only."""
    def fail(message: str) -> int:
        print(f't3 shim: {message}', file=sys.stderr)
        return 1

    if len(argv) < 2 or argv[0] != '--run':
        return fail('internal: --run missing')
    root = Path(os.path.realpath(argv[1]))
    rest = argv[2:]
    # Which t3term asks: the TUI in the run's pane, or a CLI line from `t3v env`.
    client = 'tui'
    if rest[:1] == ['--client'] and len(rest) > 1:
        client, rest = rest[1], rest[2:]
    if rest[:2] != ['auth', 'session'] or len(rest) < 3 or rest[2] not in ('issue', 'revoke'):
        return fail('only `auth session issue` and `auth session revoke` are supported')
    action, words = rest[2], rest[3:]
    flags, positional, scopes = {}, [], []
    i = 0
    while i < len(words):
        word = words[i]
        if word in ('--ttl', '--label', '--subject', '--scope', '--base-dir') and i + 1 < len(words):
            if word == '--scope':
                scopes.append(words[i + 1])
            else:
                flags[word] = words[i + 1]
            i += 2
        elif word.startswith('--'):
            flags[word] = True
            i += 1
        else:
            positional.append(word)
            i += 1
    expected_home = root / 'home' / '.t3'
    if Path(os.path.realpath(str(flags.get('--base-dir', '')))) != expected_home:
        return fail(f"--base-dir must be this run's T3 home, {expected_home}")
    origin = origin_of(root)
    if origin is None or not re.fullmatch(r'http://127\.0\.0\.1:\d+', origin):
        return fail("this run's server is not running")
    try:
        key = (root / 'control.key').read_text().strip()
    except OSError:
        return fail("this run's control key is missing")
    if action == 'issue':
        route, body = '/__fixture/auth/issue', {'scopes': scopes, 'ttl': flags.get('--ttl'),
                                                 'label': flags.get('--label'), 'subject': flags.get('--subject'),
                                                 'client': client}
    else:
        if not positional:
            return fail('revoke needs a session id')
        route, body = '/__fixture/auth/revoke', {'sessionId': positional[0]}
    request = urllib.request.Request(origin + route, data=json.dumps(body).encode(), method='POST',
                                     headers={'Content-Type': 'application/json', 'X-Fixture-Control': key})
    try:
        with opener().open(request, timeout=5) as response:
            answer = response.read()
    except (urllib.error.URLError, OSError) as error:
        return fail(f'the server refused {action}: {error}')
    if action == 'issue':
        # The credential goes to t3term on stdout, as `t3 auth session issue --json` does.
        sys.stdout.write(answer.decode() + '\n')
    return 0


def ticket_answer(ticket: str) -> dict:
    """What /api/auth/websocket-ticket answers."""
    return {'ticket': ticket, 'expiresAt': iso(now_ms() + 60 * 1000)}

