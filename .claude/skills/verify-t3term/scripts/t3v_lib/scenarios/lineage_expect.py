"""What the details' Lineage section should show, worked out from what the fake sent, as
src/tui/lineage.rs works it out, so `t3v expect` and `t3v lineage` can say what to look for.

lineage.rs follows T3 at f570bd2: `deriveThreadRelationshipGraph`, `immediateThreadRelationships`,
`threadRelationshipRowStatus`, `orderWebThreadLineageRows` and `resolveMergeBackTargetThreadId` in
packages/client-runtime/src/state/threadRelationships.ts; `ThreadRelationshipsPanel` in
apps/web/src/components/chat/ThreadRelationshipsControl.tsx; `threadRelationshipStatusLabel` and
`ThreadRelationshipIcon` in apps/web/src/components/chat/ThreadRelationshipIcon.tsx; and
`formatSubagentDisplayTitle` in packages/client-runtime/src/state/subagentDisplay.ts.
"""

from __future__ import annotations

import re

from ..text import MISSING, RUST_WHITESPACE, bounded, safe

# FIRST_PAGE, PAGE, TITLE, ID, TASK_PATH and TIMESTAMP in src/tui/lineage.rs.
FIRST_PAGE, PAGE = 6, 12
TITLE_MAX = 256
ID_MAX = 512
TASK_PATH = 4096
TIMESTAMP_MAX = 64
SETTLED = ('completed', 'failed', 'error', 'cancelled', 'interrupted', 'idle')
AT_WORK = ('pending', 'running', 'waiting')
GROUPS = ('related', 'active', 'previous')
# status_word in src/tui/lineage.rs. Anything else reads Unknown.
STATUS_WORDS = {
    'preparing': 'Starting', 'starting': 'Starting', 'running': 'Running', 'in_progress': 'Running',
    'pending': 'Queued', 'queued': 'Queued', 'waiting': 'Waiting', 'blocked': 'Waiting',
    'completed': 'Done', 'failed': 'Failed', 'error': 'Failed', 'cancelled': 'Stopped',
    'interrupted': 'Stopped', 'rolled_back': 'Reverted', 'resolved_native': 'Resolved (native)',
    'resolved_portable': 'Resolved (portable)', 'consumed': 'Consumed', 'superseded': 'Superseded',
    'idle': 'Idle',
}
MARKS = {'Parent thread': '↑', 'Parent agent': '↑', 'Subagent': '◆', 'Fork': '↳', 'Context transfer': '↳'}
TONE_COLORS = {'live': 'theme.info', 'failed': 'theme.error_fg', 'done': 'theme.success', 'quiet': 'theme.muted'}


def id_of(value):
    """lineage.rs's id_of: a string that isn't blank or longer than ID_MAX bytes, else None."""
    if isinstance(value, str) and len(value.encode()) <= ID_MAX and value.strip(RUST_WHITESPACE):
        return value
    return None


def relationship(thread):
    lineage = thread.get('lineage') if isinstance(thread, dict) else None
    return lineage.get('relationshipToParent') if isinstance(lineage, dict) else None


def parent_of(thread):
    """lineage.rs's parent_of: the thread of the run it was forked from, else its lineage's
    parent."""
    if not isinstance(thread, dict):
        return None
    forked = thread.get('forkedFrom')
    if isinstance(forked, dict) and forked.get('type') == 'run':
        return id_of(forked.get('threadId'))
    lineage = thread.get('lineage')
    return id_of(lineage.get('parentThreadId')) if isinstance(lineage, dict) else None


def opens(thread: dict) -> bool:
    """lineage.rs's opens: the shell's copy opens only while it isn't archived."""
    return thread.get('archivedAt') is None


def run_status(thread: dict):
    """lineage.rs's run_status: the live run status, else the thread's status."""
    for key in ('activityRunStatus', 'status'):
        if isinstance(thread.get(key), str):
            return thread[key]
    return None


def last_word(record, closed: bool):
    """lineage.rs's last_word: a status still at work is unknown once the watch closed."""
    status = record.get('status') if isinstance(record, dict) else None
    if not isinstance(status, str) or (closed and status in AT_WORK):
        return None
    return status


def tone(status) -> str:
    if status in ('running', 'in_progress', 'pending', 'waiting'):
        return 'live'
    if status in ('failed', 'error'):
        return 'failed'
    return 'done' if status == 'completed' else 'quiet'


def without_prefix(title: str) -> str:
    """lineage.rs's without_prefix: a `Subagent:` prefix in any case goes, and the white space
    after it, of which it reads at most TITLE_MAX * 4 characters."""
    if title.encode()[:9].lower() != b'subagent:':
        return title
    rest = title[9:]
    for index, c in enumerate(rest):
        if c not in RUST_WHITESPACE or index == TITLE_MAX * 4:
            return rest[index:]
    return ''


def task_name(text: str):
    """lineage.rs's task_name: /root/review/fix_the_parser reads Fix The Parser."""
    if len(text.encode()) > TASK_PATH or not text.startswith('/root/'):
        return None
    rest = text[len('/root/'):]
    if rest.endswith('/'):
        rest = rest[:-1]
    segments = rest.split('/')
    if any(segment == '' for segment in segments):
        return None
    words, word = [], ''
    for c in segments[-1]:
        if c == '_' or c in RUST_WHITESPACE:
            if word:
                words.append(word)
            word = ''
        else:
            word += c
    if word:
        words.append(word)
    return ' '.join(w[0].upper() + w[1:] for w in words) if words else None


def display(raw: str, subagent: bool) -> str:
    """lineage.rs's display: a title cleaned and cut at TITLE_MAX characters. An agent's loses
    its prefix, and a task path reads as words."""
    if not subagent:
        return bounded(raw, TITLE_MAX)
    text = without_prefix(raw)
    name = task_name(text)
    return bounded(name if name is not None else text, TITLE_MAX)


def agent_title(agent: dict):
    """lineage.rs's agent_title: the agent's title, even an empty one, else its prompt cut to
    77 characters and ... when it has more than 80."""
    if isinstance(agent.get('title'), str):
        return agent['title']
    prompt = agent.get('prompt')
    if not isinstance(prompt, str):
        return None
    return prompt if len(prompt) <= 80 else prompt[:77] + '...'


def parse_ms(text: str):
    """theme.rs's parse_iso_ms, which reads fixed byte offsets and ignores the zone."""
    data = text.encode()
    if len(data) < 19:
        return None

    def num(start: int, end: int):
        part = data[start:end]
        return int(part) if re.fullmatch(rb'[+-]?[0-9]+', part) else None

    def div(a: int, b: int) -> int:
        # Rust's / on i64, which rounds toward zero.
        q = abs(a) // abs(b)
        return q if (a >= 0) == (b > 0) else -q

    fields = [num(0, 4), num(5, 7), num(8, 10), num(11, 13), num(14, 16), num(17, 19)]
    if any(field is None for field in fields):
        return None
    year, month, day, hour, minute, second = fields
    millis = 0
    if data[19:20] == b'.':
        digits = re.match(rb'[0-9]*', data[20:]).group(0)
        millis = int((digits[:3] + b'000')[:3])
    # Days from civil, after Howard Hinnant.
    y = year - 1 if month <= 2 else year
    era = div(y if y >= 0 else y - 399, 400)
    yoe = y - era * 400
    mp = (month + 9) - div(month + 9, 12) * 12
    doy = div(153 * mp + 2, 5) + day - 1
    doe = yoe * 365 + div(yoe, 4) - div(yoe, 100) + doy
    days = era * 146097 + doe - 719468
    return (((days * 24 + hour) * 60 + minute) * 60 + second) * 1000 + millis


def default_paging() -> dict:
    """Paging::default: each group shows FIRST_PAGE rows and Previous agents starts closed."""
    return {'previous_open': False, 'shown': {group: FIRST_PAGE for group in GROUPS}}


def lineage_of(current: str, threads: list, projection, closed: bool, paging: dict) -> dict:
    """Lineage::of in src/tui/lineage.rs: what the section shows for thread `current`, from the
    shell's threads and the projection t3term holds for it, which counts only while it is
    `current`'s. `closed` says the thread's watch closed."""
    by_id, firsts = {}, []
    for thread in threads:
        tid = id_of(thread.get('id')) if isinstance(thread, dict) else None
        if tid is not None and tid not in by_id:
            by_id[tid] = thread
            firsts.append((tid, thread))

    # (source, target, kind) -> status, in the order each first came. A later edge between the
    # same two threads of the same kind takes the earlier one's place, as a dict key keeps its.
    edges = {}

    def add(source: str, target: str, kind: str, status):
        if source != target:
            edges[(source, target, kind)] = status

    # A later copy of a thread counts for nothing.
    for tid, thread in firsts:
        parent = parent_of(thread)
        if parent is None or (parent != current and tid != current):
            continue
        add(parent, tid, 'subagent' if relationship(thread) == 'subagent' else 'fork', run_status(thread))

    own = projection.get('thread') if isinstance(projection, dict) else None
    if not isinstance(own, dict) or id_of(own.get('id')) != current:
        projection = None

    def listing(key: str) -> list:
        value = projection.get(key) if projection is not None else None
        return value if isinstance(value, list) else []

    subagents = listing('subagents')
    agents = {}  # child thread id -> the last agent record that names it
    for agent in subagents:
        child = id_of(agent.get('childThreadId')) if isinstance(agent, dict) else None
        if child is None:
            continue
        agents[child] = agent
        listed = by_id.get(child)
        if listed is not None and closed:
            # The shell's copy speaks for it, through the edge its lineage made, if it made one.
            continue
        status = listed.get('activityRunStatus') if listed is not None else None
        if not isinstance(status, str):
            status = last_word(agent, closed)
        add(current, child, 'subagent', status)
    for transfer in listing('contextTransfers'):
        if not isinstance(transfer, dict):
            continue
        source, target = id_of(transfer.get('sourceThreadId')), id_of(transfer.get('targetThreadId'))
        if source is None or target is None or current not in (source, target):
            continue
        add(source, target, 'transfer', last_word(transfer, closed))

    # The thread a fork merges back to, by the projection's copy, or the shell's once closed.
    record = projection.get('thread') if projection is not None else None
    if closed and current in by_id:
        record = by_id[current]
    merge_target = parent_of(record) if relationship(record) == 'fork' else None

    def is_parent(kind: str, target: str) -> bool:
        return kind != 'transfer' and target == current

    seen, candidates = set(), []
    for (source, target, kind), status in edges.items():
        rid = target if source == current else source
        if rid in seen:
            continue
        seen.add(rid)
        created = by_id[rid].get('createdAt') if rid in by_id else None
        if not isinstance(created, str) or len(created.encode()) > TIMESTAMP_MAX:
            created = None
        else:
            created = parse_ms(created)
        candidates.append({'id': rid, 'target': target, 'kind': kind, 'status': status, 'created': created,
                           'rank': 0 if is_parent(kind, target) else 1 if rid == merge_target else 2})
    # The parent, then the thread a fork merges back to, then the newest first, then by id.
    candidates.sort(key=lambda c: (c['rank'], c['created'] is None, -(c['created'] or 0), c['id']))

    def group_of(c: dict) -> str:
        if c['kind'] != 'subagent' or is_parent(c['kind'], c['target']):
            return 'related'
        return 'previous' if c['status'] in SETTLED else 'active'

    # Agents without a thread yet have no row, so they count on their own while the watch can
    # still hear them end.
    unseen = 0 if closed else sum(1 for agent in subagents
                                  if isinstance(agent, dict) and 'childThreadId' in agent
                                  and agent['childThreadId'] is None and agent.get('status') == 'running')
    working = sum(1 for c in candidates if group_of(c) == 'active' and c['status'] == 'running')
    running = unseen + working

    parts = []
    for group in GROUPS:
        members = [c for c in candidates if group_of(c) == group]
        if not members:
            continue
        is_open = group != 'previous' or paging['previous_open']
        shown = min(paging['shown'][group], len(members)) if is_open else 0
        rows = [row_for(c, current, by_id, agents) for c in members[:shown]]
        part = {'group': group, 'total': len(members), 'open': is_open, 'rows': rows,
                'failed': sum(1 for c in members if c['status'] in ('failed', 'error')),
                'more': min(len(members) - len(rows), PAGE) if is_open else 0}
        if group == 'previous':
            part['heading'] = '⌄ Previous agents' if is_open else f'› Previous agents ({len(members)})'
            part['hint'] = f'Enter {"hide" if is_open else "show"} previous agents'
        parts.append(part)
    return {'thread': current, 'running': running, 'parts': parts, 'related': sorted(seen),
            'heading': f'Lineage · {running} running' if running else 'Lineage',
            'shows': bool(parts) or running > 0}


def row_for(c: dict, current: str, by_id: dict, agents: dict) -> dict:
    """lineage.rs's row_for: a related thread's row, with its words, its tone and what Enter on
    it says in the status line."""
    rid, kind = c['id'], c['kind']
    parent = kind != 'transfer' and c['target'] == current
    thread = by_id.get(rid)
    # A parent shows its own activity, every other row its edge's status.
    if kind == 'transfer' or rid == c['target']:
        status = c['status']
    else:
        status = run_status(thread) if thread is not None else None
    if kind == 'transfer':
        relation = 'Context transfer'
    elif kind == 'subagent':
        relation = 'Parent agent' if parent else 'Subagent'
    else:
        relation = 'Parent thread' if parent else 'Fork'
    subagent = kind == 'subagent'
    agent = agents.get(rid) if subagent and not parent else None
    # The shell's title, the agent's, then the id. One that cleans to nothing gives way.
    raw = thread.get('title') if thread is not None else None
    if not isinstance(raw, str):
        raw = agent_title(agent) if agent is not None else None
    title = display(raw, subagent) if raw is not None else ''
    title = title or display(rid, subagent) or 'Untitled'
    available = thread is not None and opens(thread)
    word = STATUS_WORDS.get(status, 'Unknown') if isinstance(status, str) else 'Unknown'
    if not available and word == 'Unknown':
        word = 'Unavailable'
    return {'id': rid, 'title': title, 'relation': relation, 'mark': MARKS[relation], 'word': word,
            'tone': tone(status), 'available': available,
            'hint': f'Enter open {relation.lower()}' if available else f'{relation} unavailable'}


# ---- what t3term holds: src/projection.rs ----


def upsert(entries: list, entity):
    """projection.rs's upsert: the entity in place of the first entry with its id, else last."""
    has_id = isinstance(entity, dict) and 'id' in entity
    index = next((i for i, entry in enumerate(entries)
                  if has_id and isinstance(entry, dict) and entry.get('id', MISSING) == entity['id']), None)
    if index is None:
        entries.append(entity)
    else:
        entries[index] = entity


# ---- printing ----


def print_lineage(lineage: dict, tid: str):
    """The rows the details' Lineage should show for thread `tid`, below its facts and one blank
    row, and what Enter on each says in the status line."""
    if not lineage.get('shows'):
        print(f'  Lineage of {tid}: none. The details end with the facts, and the status line offers no '
              f'"choose"')
    else:
        print(f'  Lineage of {tid}:')
        print(f'    {lineage["heading"]}')
        for part in lineage.get('parts') or []:
            if part['group'] == 'previous':
                failed = f' {part["failed"]} failed' if part['failed'] else ''
                print(f'    {part["heading"]} ──{failed}  [{part["hint"]}; a muted heading, a rule to the '
                      "row's end" + (', and the failed count in theme.error_fg]' if failed else ']'))
            for row in part['rows']:
                word = 'theme.error_fg' if row['tone'] == 'failed' else 'theme.muted'
                look = (f'mark in {TONE_COLORS[row["tone"]]}, word in {word} at the right'
                        + ('' if row['available'] else ', title muted'))
                print(f'    {row["mark"]} {safe(row["title"], 64)}  ·  {row["word"]}    [{row["hint"]}; {look}]')
            if part['more']:
                print(f'    + Show {part["more"]} more  [Enter show {part["more"]} more]')
    for note in lineage.get('notes') or []:
        print(f'    ({note})')


def print_lineages(expect: dict):
    """Lineage for each thread t3term watches."""
    lineage = expect.get('lineage') or {}
    if not lineage:
        print('  Lineage       no thread is open, so the details show no Lineage')
    for tid, entry in lineage.items():
        print_lineage(entry, tid)
    if lineage:
        print('    (a title is cut with … to fit; a row narrower than its mark, 4 columns of title, a space '
              'and its word drops the word)')


def print_thread_list(entries: list, transfers: list, everything: bool = False):
    """`t3v threads` for a lineage run: thr-many's agent threads fold away unless `everything`."""
    folded = 0
    for entry in entries:
        if not everything and entry['threadId'].startswith('thr-many-'):
            folded += 1
            continue
        line = (f'  {entry["threadId"]:<20} {safe(entry["title"], 34):<34} {entry["run"]}; '
                f'{entry["streams"]} open and {entry.get("held", 0)} held stream(s)')
        if entry.get('relationship'):
            line += f', {entry["relationship"]} of {entry["parent"]}'
        if entry.get('agents'):
            line += f', {entry["agents"]} agent(s)'
        if entry.get('archived'):
            line += ', archived'
        if entry.get('watch'):
            line += f', watch {entry["watch"]}'
        if not entry.get('listed', True):
            line += ', left off the shell'
        print(line)
        if 'ownTitle' in entry:
            print(f'      its own copy has the title {entry["ownTitle"]}')
    if folded:
        print(f'  … and {folded} agent threads of thr-many, which `t3v threads --all` lists')
    print()
    print('  transfers, in the order T3 made them:')
    for x in transfers:
        resolution = f', {x["resolution"]}' if x.get('resolution') else ''
        print(f'  {x["id"]:<22} {x["type"]} {x["source"]} → {x["target"]}: {x["status"]}{resolution}')
    if not transfers:
        print('  none')


def print_agents(answer: dict):
    """`t3v agents THREAD`: each agent's status, title, thread and the thread's newest run."""
    agents = answer.get('agents') or []
    for agent in agents:
        line = f'  {agent["agentId"]:<20} {agent["status"]:<11} title {agent["title"]}'
        if agent.get('child'):
            line += f'; thread {agent["child"]}'
            line += f', {agent["childRun"]}' if agent.get('childRun') else ', no runs'
            if not agent.get('listed'):
                line += ', left off the shell'
        else:
            line += '; no thread yet'
        print(line)
    if not agents:
        print(f'  {answer.get("threadId")} has started no agents')
