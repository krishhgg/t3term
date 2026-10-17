"""What the Pull requests tab should show, worked out from what the fake sent, near enough to
src/tui/pull_requests.rs that `t3v expect` can say what to look for.
"""

from __future__ import annotations

import re
from datetime import datetime, timezone

from ..text import RUST_WHITESPACE, bounded, safe
from .links_data import (CAPABILITY_WORDS, KEY_BYTES, MOST_LINKS, NAME_MAX, SOURCE_LABELS, STAMP_BYTES, TITLE_MAX,
                         URL_MAX)

# ---- what the tab shows: src/tui/pull_requests.rs, near enough ----

SHOWN_LINKS = 14  # the links `expect` prints rows for


def text_of(value, key: str):
    found = value.get(key) if isinstance(value, dict) else None
    return found if isinstance(found, str) else None


def count_of(value, key: str):
    """A field that serde_json's as_u64 reads: a whole number from 0 to 2**64 - 1."""
    found = value.get(key) if isinstance(value, dict) else None
    if isinstance(found, bool) or not isinstance(found, int) or not 0 <= found < 2 ** 64:
        return None
    return found


def number_of(value):
    """number_of: a positive `number`."""
    number = count_of(value, 'number')
    return number if number else None


def stamp_of(value):
    """stamp: a string of at most STAMP_BYTES bytes."""
    return value if isinstance(value, str) and len(value.encode()) <= STAMP_BYTES else None


def iso_ms(text):
    """theme.rs's parse_iso_ms, near enough: milliseconds since the epoch, or None."""
    if not isinstance(text, str) or len(text.encode()) < 19:
        return None
    try:
        parsed = datetime.fromisoformat(text.replace('Z', '+00:00'))
    except ValueError:
        return None
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return int(parsed.timestamp() * 1000)


def time_ago(at, now: int):
    """time_ago: `just now`, `5m ago`, `3h ago` or `12d ago`, or None."""
    then = iso_ms(at)
    if then is None:
        return None
    seconds = max(0, now - then) // 1000
    if seconds < 60:
        return 'just now'
    if seconds < 3600:
        return f'{seconds // 60}m ago'
    if seconds < 86400:
        return f'{seconds // 3600}h ago'
    return f'{seconds // 86400}d ago'


def key_of(host: str, repository: str, number: int):
    """Key::of: a link's identity, or None when its host or repository is longer than KEY_BYTES
    bytes as T3 sent it, or empty once trimmed."""
    def part(text: str):
        if len(text.encode()) > KEY_BYTES:
            return None
        text = text.strip(RUST_WHITESPACE)
        return text.lower() if text else None

    host, repository = part(host), part(repository)
    if host is None or repository is None:
        return None
    return host, repository, number


def url_host(url: str) -> str:
    """host_of: what follows `scheme://` up to a path, a query or a fragment, without user info
    or a port, lowercased, or `unknown`."""
    url = url[:2 * KEY_BYTES]
    scheme, sep, rest = url.strip(RUST_WHITESPACE).partition('://')
    if not sep or not re.fullmatch(r'[A-Za-z][A-Za-z0-9+.-]*', scheme):
        return 'unknown'
    authority = re.split(r'[/?#\\]', rest, maxsplit=1)[0].rpartition('@')[2]
    if authority.startswith('[') and ']' in authority:
        host = authority[:authority.index(']') + 1]
    else:
        host = authority.split(':')[0]
    host = host.strip(RUST_WHITESPACE)
    if not host or len(host.encode()) > KEY_BYTES:
        return 'unknown'
    return host.lower()


def url_shown(url: str):
    """url_of: the URL bounded and cleaned, and whether that cut it short."""
    shown = bounded(url, URL_MAX)
    return shown, shown.endswith('…') and len(url) > URL_MAX


def snapshot_read(snapshot):
    """snapshot_of: the host state as t3term reads it, or None when a required field is missing
    or not of the contract's type."""
    if not isinstance(snapshot, dict):
        return None
    draft, state = snapshot.get('isDraft'), snapshot.get('state')
    title, head, base = text_of(snapshot, 'title'), text_of(snapshot, 'headBranch'), text_of(snapshot, 'baseBranch')
    if not isinstance(draft, bool) or state not in ('open', 'merged', 'closed'):
        return None
    if title is None or head is None or base is None:
        return None
    shown = 'Draft' if state == 'open' and draft else state.capitalize()
    checks = {'passing': 'Checks passing', 'failing': 'Checks failing',
              'pending': 'Checks pending'}.get(text_of(snapshot, 'checksState'))
    review = {'approved': 'Approved', 'changes-requested': 'Changes requested',
              'review-required': 'Awaiting review'}.get(text_of(snapshot, 'reviewDecision'))
    author = snapshot.get('author')
    login = text_of(author, 'login')
    login = bounded(login, NAME_MAX) if login is not None else ''

    def key(branch: str):
        return branch if len(branch.encode()) <= KEY_BYTES else None

    return {'state': shown, 'conflicting': shown == 'Open' and text_of(snapshot, 'mergeability') == 'conflicting',
            'title': bounded(title, TITLE_MAX), 'head': bounded(head, NAME_MAX), 'base': bounded(base, NAME_MAX),
            'headKey': key(head), 'baseKey': key(base), 'updated': stamp_of(snapshot.get('updatedAt')),
            'synced': stamp_of(snapshot.get('syncedAt')), 'author': login or None,
            'bot': isinstance(author, dict) and author.get('isBot') is True,
            'additions': count_of(snapshot, 'additions'), 'deletions': count_of(snapshot, 'deletions'),
            'checks': checks, 'review': review}


def link_read(one):
    """link_of: 'dismissed' for a tombstone, None for a link t3term can't read, else the link
    as it shows."""
    source = text_of(one, 'source')
    if source is None:
        return None
    if source == 'stack-dismissed':
        return 'dismissed'
    host, repository, url = text_of(one, 'host'), text_of(one, 'repository'), text_of(one, 'url')
    number = number_of(one)
    if host is None or repository is None or number is None or url is None:
        return None
    key = key_of(host, repository, number)
    if key is None:
        return None
    if one.get('snapshot') is None:
        synced, snapshot = 'waiting', None
    else:
        snapshot = snapshot_read(one['snapshot'])
        synced = 'read' if snapshot else 'unreadable'
    linked_at = stamp_of(one.get('linkedAt'))
    at = snapshot['updated'] if snapshot and snapshot['updated'] is not None else linked_at
    stack, layers, sid = one.get('stack'), [], text_of(one.get('stack'), 'id')
    if sid is not None and len(sid.encode()) > KEY_BYTES:
        sid = None
    if sid is not None:
        raw_layers = stack.get('layers')
        for layer in (raw_layers if isinstance(raw_layers, list) else [])[:MOST_LINKS]:
            if count_of(layer, 'number') is not None:
                layers.append(layer['number'])
    shown, cut = url_shown(url)
    return {'key': key, 'number': number, 'host': bounded(host, NAME_MAX), 'repository': bounded(repository, NAME_MAX),
            'url': shown, 'urlCut': cut, 'source': SOURCE_LABELS.get(source, 'Linked'), 'linkedAt': linked_at,
            'synced': synced, 'snapshot': snapshot, 'watching': isinstance(one.get('watch'), dict),
            'stack': (sid, layers) if sid is not None else None, 'activity': iso_ms(at) or 0}


def legacy_read(linked):
    """legacy_of: the legacy link as a link by you that the host hasn't synced, or None."""
    repository, url, number = text_of(linked, 'repository'), text_of(linked, 'url'), number_of(linked)
    if repository is None or number is None or url is None:
        return None
    key = key_of(url_host(url), repository, number)
    if key is None:
        return None
    shown, cut = url_shown(url)
    return {'key': key, 'number': number, 'host': bounded(key[0], NAME_MAX), 'repository': bounded(key[1], NAME_MAX),
            'url': shown, 'urlCut': cut, 'source': 'Linked by you', 'linkedAt': None, 'synced': 'waiting',
            'snapshot': None, 'watching': False, 'stack': None, 'activity': 0}


def stacks_of(raws: list) -> list:
    """stacks_of: each stack's (native, places in `raws` from the bottom up)."""
    count = len(raws)
    placed = [False] * count
    stacks, groups, group_of = [], [], {}
    for index, raw in enumerate(raws):
        if raw['stack'] is None:
            continue
        group = (raw['key'][0], raw['key'][1], raw['stack'][0])
        if group not in group_of:
            group_of[group] = len(groups)
            groups.append([])
        groups[group_of[group]].append(index)
    for members in groups:
        order = {number: at for at, number in enumerate(raws[members[0]]['stack'][1])}
        members.sort(key=lambda index: order.get(raws[index]['number'], 0))
        for member in members:
            placed[member] = True
        stacks.append((True, members))
    remaining = [index for index in range(count) if not placed[index]]

    def branch(index: int, base: bool):
        snapshot = raws[index]['snapshot']
        name = snapshot and snapshot['baseKey' if base else 'headKey']
        if name is None or snapshot is None:
            return None
        return raws[index]['key'][0], raws[index]['key'][1], name

    by_head = {}
    for index in remaining:
        head = branch(index, False)
        if head is not None:
            by_head[head] = None if head in by_head else index

    def parent_of(index: int):
        base = branch(index, True)
        return None if base is None else by_head.get(base)

    has_child = [False] * count
    for index in remaining:
        parent = parent_of(index)
        if parent is not None and parent != index:
            has_child[parent] = True
    for top in remaining:
        if has_child[top]:
            continue
        layers, cursor = [], top
        while cursor is not None and not placed[cursor]:
            placed[cursor] = True
            layers.append(cursor)
            cursor = parent_of(cursor)
        # A walk that comes back to a link it passed has run into a cycle. That link and the ones
        # after it stand alone, and the links that build on them stack without them.
        if cursor is not None and cursor in layers:
            start = layers.index(cursor)
            stacks.extend((False, [index]) for index in layers[start:])
            del layers[start:]
        layers.reverse()
        if layers:
            stacks.append((False, layers))
    for index in remaining:
        if not placed[index]:
            stacks.append((False, [index]))
    return stacks


def pulls_of(record: dict) -> dict:
    """links_of: the links in the order they show, how many t3term can't read and how many it
    leaves unread, or {'unreadable': True} when `pullRequests` isn't a list."""
    raws, skipped, unread = [], 0, 0
    if 'pullRequests' in record:
        links = record['pullRequests']
        if not isinstance(links, list):
            return {'unreadable': True}
        unread = max(0, len(links) - MOST_LINKS)
        for one in links[:MOST_LINKS]:
            raw = link_read(one)
            if raw == 'dismissed':
                continue
            if raw is None:
                skipped += 1
            else:
                raws.append(raw)
    elif record.get('linkedPullRequest') is not None:
        raw = legacy_read(record['linkedPullRequest'])
        if raw is None:
            skipped += 1
        else:
            raws.append(raw)
    seen, kept = set(), []
    for raw in raws:
        if raw['key'] not in seen:
            seen.add(raw['key'])
            kept.append(raw)
    stacks = stacks_of(kept)
    stacks.sort(key=lambda stack: -max(kept[index]['activity'] for index in stack[1]))
    entries = []
    for native, layers in stacks:
        for depth, index in enumerate(layers):
            chain = (native, len(layers)) if depth == 0 and len(layers) > 1 else None
            entries.append(dict(kept[index], depth=depth, chain=chain))
    opened = sum(1 for entry in entries if entry['snapshot'] is None or entry['snapshot']['state'] in ('Open', 'Draft'))
    stamps = [entry['snapshot']['synced'] for entry in entries if entry['snapshot'] and entry['snapshot']['synced']]
    synced = max(stamps, key=lambda at: iso_ms(at) or 0, default=None)
    return {'entries': entries, 'skipped': skipped, 'unread': unread, 'open': opened, 'synced': synced}


def diff_text(snapshot) -> str:
    """diff_parts: `+1,234 -56`, or nothing when the host counted no lines."""
    if not snapshot or (not snapshot['additions'] and not snapshot['deletions']):
        return ''
    parts = []
    if snapshot['additions'] is not None:
        parts.append(f'+{snapshot["additions"]:,}')
    if snapshot['deletions'] is not None:
        parts.append(f'-{snapshot["deletions"]:,}')
    return ' '.join(parts)


def link_rows(entry: dict, now: int) -> list:
    """entry_lines: a link's three rows, each whole, before t3term cuts them to the card."""
    snapshot = entry['snapshot']
    if entry['synced'] == 'waiting':
        mark, state = '○', 'Waiting for host state'
    elif entry['synced'] == 'unreadable':
        mark, state = '○', 'Host state unreadable'
    else:
        state = snapshot['state']
        mark = '◌' if state == 'Draft' else '●'
    opened = snapshot is None or snapshot['state'] in ('Open', 'Draft')
    parts = []
    if entry['chain']:
        native, size = entry['chain']
        parts.append(f'Stack of {size}' if native else f'{size} chained')
    parts.append(state)
    if opened and entry['watching']:
        parts.append('Watching')
    if snapshot and opened:
        if snapshot['conflicting']:
            parts.append('Conflicts')
        parts.extend(part for part in (snapshot['checks'], snapshot['review']) if part)
    ago = time_ago(snapshot['updated'], now) if snapshot else None
    title = snapshot['title'] if snapshot and snapshot['title'] else entry['repository']
    diff = diff_text(snapshot)
    pad = '  ' * min(entry['depth'], 3)
    if snapshot:
        place = f'{entry["repository"]} · {snapshot["head"]} → {snapshot["base"]}'
    else:
        place = f'{entry["host"]}/{entry["repository"]}'
    return [f'{pad}{mark} #{entry["number"]} {title}' + (f'   {diff}' if diff else ''),
            f'{pad}  ' + ' · '.join(parts) + (f'   {ago}' if ago else ''),
            f'{pad}  {place}']


def expected_pulls(record: dict, supported: bool, now: int) -> dict:
    """List::of and the page it lays out, for a thread `record` t3term holds."""
    if not supported:
        return {'tab': 'Pull requests', 'message': [
            'Linked pull requests unavailable', 'This environment does not support multiple linked pull requests.']}
    body = pulls_of(record)
    if body.get('unreadable'):
        return {'tab': 'Pull requests', 'message': [
            'Linked pull requests unreadable', "T3 sent this thread's pull requests in a form t3term can't read."]}
    entries = body['entries']
    if not entries:
        message = ['No linked pull requests', 'Pull requests the agent opens from this thread land here.']
        skipped = body['skipped'] + body['unread']
        if skipped:
            message.append("1 link couldn't be read." if skipped == 1 else f"{skipped} links couldn't be read.")
        return {'tab': 'Pull requests', 'message': message}
    parts = [f'{body["open"]} open', f'{len(entries)} linked']
    ago = time_ago(body['synced'], now)
    if ago:
        parts.append(f'synced {ago}')
    if body['skipped']:
        parts.append(f'{body["skipped"]} unreadable')
    if body['unread']:
        parts.append(f'{body["unread"]} not read')
    return {'tab': f'Pull requests {len(entries)}', 'footer': ' · '.join(parts), 'count': len(entries),
            'rows': [link_rows(entry, now) for entry in entries[:SHOWN_LINKS]],
            'urlCut': [entry['number'] for entry in entries if entry['urlCut']],
            'more': max(0, len(entries) - SHOWN_LINKS)}


def shown_value(value: str) -> str:
    """An expected value as one printable row, with its length when the row cuts it."""
    return safe(value, 160) + (f'  ({len(value)} characters in all)' if len(value) > 160 else '')


def print_pulls(expect: dict):
    """What the Pull requests tab should show, from what the fixture sent."""
    held, served = expect.get('capabilityHeld'), expect.get('capabilityServed')
    if held:
        line = f'{CAPABILITY_WORDS[held]}, from {expect.get("capabilityFrom")}'
        if served != held:
            line += f'; the fixture serves {CAPABILITY_WORDS.get(served, served)} now'
    else:
        line = 'none yet: t3term has read neither the descriptor nor a config'
    print(f'  capability    {line}')
    pulls = expect.get('pulls') or {}
    if not pulls:
        print('  pull requests no thread open, so the tab reads "No thread is open."')
    for tid, shown in pulls.items():
        print(f'  pull requests of {tid}, from {shown.get("source")}:')
        print(f'    tab name    "{shown.get("tab")}"')
        for line in shown.get('message') or []:
            print(f'    {shown_value(line)}')
        for rows in shown.get('rows') or []:
            for row in rows:
                print(f'    {shown_value(row)}')
        if shown.get('more'):
            print(f'    ({shown["more"]} more links below these, three rows each)')
        if shown.get('footer'):
            print(f'    footer      "{shown_value(shown["footer"])}"')
        cut = shown.get('urlCut') or []
        if cut:
            print('    URL cut     ' + ', '.join(f'#{n}' for n in cut[:10])
                  + ': their details end the URL with … and add the "Cut short" note')
    if any(shown.get('rows') for shown in pulls.values()):
        print("    (each row whole: t3term cuts it to the card, sets the line counts and the time at the card's "
              "right edge, and moves a stack's upper layers two columns in each)")

