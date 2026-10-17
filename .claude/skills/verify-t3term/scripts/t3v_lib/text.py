"""Text helpers: timestamps, control characters, widths and t3term's own text cleaning."""

from __future__ import annotations

import hashlib
import json
import re
import sys
import time
import unicodedata
from datetime import datetime, timezone

SECOND, MINUTE, HOUR, DAY = 1000, 60 * 1000, 3600 * 1000, 86400 * 1000
NAME = re.compile(r'^[A-Za-z0-9][A-Za-z0-9._-]{0,80}$')
THREAD_ID = re.compile(r'^[A-Za-z0-9_-]{1,60}$')
# A field a record leaves out, as opposed to one it sends as null.
MISSING = object()
# Rust's char::is_whitespace among the control characters, which without_controls turns into spaces.
RUST_CONTROL_SPACES = '\t\n\x0b\x0c\r\x85'
# Every character Rust's str::trim takes off, which differs from Python's str.strip.
RUST_WHITESPACE = ('\t\n\x0b\x0c\r \x85\xa0\u1680\u2000\u2001\u2002\u2003\u2004\u2005'
                   '\u2006\u2007\u2008\u2009\u200a\u2028\u2029\u202f\u205f\u3000')

# Set by cli.main for a command run with --json, so an error still prints one JSON object on stdout.
JSON_ERRORS = {'on': False}


def now_ms() -> int:
    return int(time.time() * 1000)


def iso(ms: int) -> str:
    """The timestamp form T3 writes: 2026-10-08T00:06:52.769Z."""
    seconds, millis = divmod(int(ms), 1000)
    stamp = datetime.fromtimestamp(seconds, tz=timezone.utc).strftime('%Y-%m-%dT%H:%M:%S')
    return f'{stamp}.{millis:03d}Z'


def die(message: str, code: int = 1, **fields):
    """Prints the error and exits. Under --json the error also goes to stdout as one object,
    with `fields`, such as the run a failed start left behind."""
    if JSON_ERRORS['on']:
        print(json.dumps({'ok': False, 'error': message, **fields}))
    print(f't3v: {message}', file=sys.stderr)
    sys.exit(code)


def escape_controls(text: str):
    """`text` with each control character but a newline, and each lone surrogate, written as an
    escape such as \\x1b, and how many there were, so a control character never reaches this
    terminal."""
    out, count = [], 0
    for c in text:
        if c != '\n' and unicodedata.category(c) in ('Cc', 'Cs'):
            out.append(f'\\x{ord(c):02x}' if ord(c) < 0x100 else f'\\u{ord(c):04x}')
            count += 1
        else:
            out.append(c)
    return ''.join(out), count


def safe(text, limit: int = 80) -> str:
    """Any value as one printable row of at most `limit` characters."""
    shown, _ = escape_controls(str(text).replace('\n', ' '))
    return shown if len(shown) <= limit else shown[:limit - 1] + '…'


def encode(value) -> bytes:
    """`value` as compact JSON in UTF-8, as JSON.stringify writes it. A lone surrogate, which
    JSON.stringify escapes, makes the whole value ASCII with escapes."""
    try:
        return json.dumps(value, separators=(',', ':'), ensure_ascii=False).encode('utf-8')
    except UnicodeEncodeError:
        return json.dumps(value, separators=(',', ':')).encode('ascii')


def unescape(text: str) -> str:
    """`text` with \\xHH, \\uHHHH, \\e, \\a, \\r, \\n, \\t and \\\\ turned into what they name."""
    named = {'\\e': '\x1b', '\\a': '\x07', '\\r': '\r', '\\n': '\n', '\\t': '\t', '\\\\': '\\'}

    def one(match) -> str:
        found = match.group(0)
        return chr(int(found[2:], 16)) if found[1] in 'xu' else named[found]

    return re.sub(r'\\x[0-9a-fA-F]{2}|\\u[0-9a-fA-F]{4}|\\[earnt\\]', one, text)


def sha(text: str) -> str:
    """The SHA-256 of `text` in UTF-8, as hex. Logs keep it in place of what was typed."""
    return hashlib.sha256(text.encode('utf-8', 'surrogatepass')).hexdigest()


def without_controls(text: str) -> str:
    """src/transcript.rs's without_controls."""
    out, i = [], 0
    while i < len(text):
        c = text[i]
        if c == '\n':
            out.append('\n')
        elif c == '\r':
            if i + 1 >= len(text) or text[i + 1] != '\n':
                out.append('\n')
        elif unicodedata.category(c) == 'Cc':
            if c in RUST_CONTROL_SPACES:
                out.append(' ')
        else:
            out.append(c)
        i += 1
    return ''.join(out)


def zero_width(c: str) -> bool:
    return unicodedata.combining(c) != 0 or unicodedata.category(c) in ('Mn', 'Me', 'Cf')


def clusters(text: str) -> list:
    """src/transcript.rs's clusters, near enough for the presets here: a new piece before each
    character with a width, except a skin tone and the character after a zero-width joiner."""
    out, joined = [], False
    for c in text:
        tone = 0x1F3FB <= ord(c) <= 0x1F3FF
        if out and (zero_width(c) or joined or tone):
            out[-1] += c
        else:
            out.append(c)
        joined = c == '\u200d'
    return out


def regional(c: str) -> bool:
    return 0x1F1E6 <= ord(c) <= 0x1F1FF


def glyphs(text: str) -> list:
    """`text` cut where tmux 3.6 and unicode-width 0.2 start a new glyph: not before a combining
    or zero-width character, a skin tone, the character after a zero-width joiner, or the second
    regional indicator of a flag."""
    out, joined = [], False
    for c in text:
        flag = regional(c) and out and len(out[-1]) == 1 and regional(out[-1])
        if out and (zero_width(c) or joined or 0x1F3FB <= ord(c) <= 0x1F3FF or flag):
            out[-1] += c
        else:
            out.append(c)
        joined = c == '\u200d'
    return out


def glyph_cells(glyph: str) -> int:
    """Columns one glyph takes: two for a wide character, an emoji sequence (a joined one, a
    skin tone, a flag) or one that VS16 or a keycap makes an emoji, none for a lone combining
    character, else one."""
    first = glyph[0]
    width = 0 if zero_width(first) else 2 if unicodedata.east_asian_width(first) in ('W', 'F') else 1
    if len(glyph) > 1 and (width == 2 or '\ufe0f' in glyph or '\u20e3' in glyph or regional(first)
                           or any(unicodedata.east_asian_width(c) in ('W', 'F') for c in glyph[1:])):
        return 2
    return width


def cells(text: str) -> int:
    """Columns `text` takes on screen, glyph by glyph, as tmux 3.6 lays it out and ratatui (with
    unicode-width 0.2) draws it. `🧑‍💻`, `👩🏽`, `❤️` and a flag each take two."""
    return sum(glyph_cells(glyph) for glyph in glyphs(text))


def bounded(value: str, limit: int) -> str:
    """src/tui/setup.rs's bounded: the text without control characters, its line breaks as
    spaces, trimmed and cut at `limit` characters in whole pieces, with … after. It reads at most
    four characters for each it keeps."""
    head = value[:limit * 4]
    cut = len(value) > limit * 4
    clean = without_controls(head).replace('\n', ' ').strip(RUST_WHITESPACE)
    out, kept = [], 0
    for piece in clusters(clean):
        if kept + len(piece) > limit:
            cut = True
            break
        out.append(piece)
        kept += len(piece)
    text = ''.join(out)
    if cut:
        text = text.rstrip(RUST_WHITESPACE) + '…'
    return text


def field_summary(value) -> str:
    """One field of a record as one short printable phrase."""
    if value is MISSING:
        return 'missing'
    if value is None:
        return 'null'
    if isinstance(value, str):
        return f'"{safe(value, 48)}"' + (f' ({len(value)} characters)' if len(value) > 48 else '')
    return f'a {type(value).__name__}, {safe(json.dumps(value), 30)}'


def text_places(screen: str, text: str) -> list:
    """(column, row) of each place `text` starts on screen, rows top to bottom, counting a wide
    character as two columns."""
    places = []
    for row, line in enumerate(screen.split('\n')):
        start = line.find(text)
        while start >= 0:
            places.append((cells(line[:start]), row))
            start = line.find(text, start + 1)
    return places
