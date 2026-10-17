"""The links scenario's data: the link builders, the presets, the pull request pages (detail,
activity, diff and file texts) and the world of eight threads. scenarios/links.py describes them.
"""

from __future__ import annotations

from ..text import DAY, HOUR, MINUTE, MISSING, field_summary, iso, now_ms, safe
from ..world import Thread

# ---- the links: src/tui/pull_requests.rs ----

MOST_LINKS = 1000  # MOST_LINKS: the links read from a record; the rest count as not read
TITLE_MAX = 512  # TITLE: the characters kept of a title
NAME_MAX = 256  # NAME: of a host, a repository, a branch or an author's login
URL_MAX = 2048  # URL: of a URL
KEY_BYTES = 1024  # KEY: a longer host or repository makes a link unreadable
STAMP_BYTES = 64  # STAMP: a longer timestamp reads as none
VIA = ('both', 'thread', 'shell')
LINK_EVENTS = ('metadata-updated', 'pull-request-synced')
LINK_PRESETS = ('mixed', 'open', 'draft', 'merged', 'closed', 'waiting', 'watch', 'partial', 'native', 'derived',
                'tombstone', 'duplicates', 'hostile', 'huge', 'legacy', 'empty', 'none', 'malformed')
SYNC_STATES = ('open', 'draft', 'merged', 'closed')
CAPABILITIES = ('on', 'off', 'missing')
CAPABILITY_WORDS = {'on': 'threadPullRequests: true', 'off': 'threadPullRequests: false',
                    'missing': 'no threadPullRequests'}
HOST = 'github.com'
REPOSITORY = 'fixture-org/linked-desk'
PROJECT_ID = 'proj-links'
# The desktop's SOURCE_LABELS. A source this version doesn't know reads `Linked`.
SOURCE_LABELS = {'manual': 'Linked by you', 'created': 'Created from this thread', 'agent': 'Linked by the agent',
                 'stack': 'Found in the stack'}
# A joined emoji without U+1F469 in it, so labelSplitEmoji counts only the hostile label's.
JOINED_SAFE = '\U0001f9d1\u200d\U0001f4bb'  # 🧑 ZWJ 💻
HOSTILE_TEXT = ('\t\x07Fix \x1b[31mred \x1b]52;c;eA==\x07osc \x9b2J \x07bel \x85漢字かな한글 cafe\u0301 '
                + JOINED_SAFE + '\r\nsecond line ')
HOSTILE_TITLE = HOSTILE_TEXT + 'x' * 600 + ' TITLE-PAST-CAP ' + 'y' * 2000
HOSTILE_URL = f'https://{HOST}/{REPOSITORY}/pull/90?q=' + 'q' * 2100 + 'URL-PAST-CAP' + 'z' * 500
HOSTILE_REPOSITORY = 'fixture-org/' + 'r' * 300 + ' REPO-PAST-CAP'
HOSTILE_BRANCH = '\t\x07feature/\x1b[31mred \x9b2J ' + 'h' * 300 + ' BRANCH-PAST-CAP ' + '-' * 1200
HOSTILE_AUTHOR = 'a' * 300 + ' AUTHOR-PAST-CAP'
LATE_TITLE = 'late-must-not-show'

# Text that only a link t3term should cut, hide or leave unread, or late data it should have
# dropped, could put in its output. rules.py adds the markers for control characters and the
# label, which the hostile preset's text carries too.
LEAK_MARKERS = {
    'titlePastTheCap': b'TITLE-PAST-CAP',
    'urlPastTheCap': b'URL-PAST-CAP',
    'repositoryPastTheCap': b'REPO-PAST-CAP',
    'branchPastTheCap': b'BRANCH-PAST-CAP',
    'authorPastTheCap': b'AUTHOR-PAST-CAP',
    'hostTooLong': b'HOST-TOO-LONG',
    'tombstoneShown': b'TOMBSTONE-HIDDEN',
    'duplicateShown': b'DUPLICATE-COPY',
    'pastTheThousand': b'PAST-THE-THOUSAND',
    'lateData': LATE_TITLE.encode(),
}
# Each of them is plain text, which a screen capture can show.
SCREEN_MARKERS = tuple(LEAK_MARKERS)


# ---- emoji that end in U+FE0F: src/tui/wide_emoji.rs ----

# thr-none's last reply, for features/transcript.md. Every emoji but the party popper ends in U+FE0F
# (VS16), which tmux draws two columns wide. Each list row with emoji sits over a row of text, so a
# scroll of one row puts letters under the column each of those emoji hides. Written with escapes,
# so the U+FE0F shows here.
VS16_REPLY = '\n'.join((
    'Emoji check: each row of the list keeps its columns.',
    '',
    '- \u26a0\ufe0f one \u2764\ufe0f two \u2714\ufe0f three',
    '- warnings, hearts and checks',
    '- \u2764\ufe0f 2  \U0001f389 3  \u26a0\ufe0f 1  reactions',
    '- every row of this list has text',
))


# ---- the links T3 sends: packages/contracts/src/threadPullRequest.ts ----


def watch_of(started: str) -> dict:
    """A ThreadPullRequestWatch, which T3 keeps on a link while it watches it for the agent."""
    return {'startedAt': started, 'headSha': None, 'failedChecks': [], 'passed': False, 'passedChecks': [],
            'remarksThrough': started, 'remarkIds': [], 'conflicting': False, 'wakes': 0}


def native_stack(sid: str, layers: list) -> dict:
    """A ThreadPullRequestStack that GitHub keeps. `layers` are (number, head, state), bottom first."""
    bottom = layers[0][0]
    return {'kind': 'native', 'id': sid, 'number': bottom, 'url': f'https://{HOST}/{REPOSITORY}/pull/{bottom}',
            'base': 'main', 'layers': [{'number': n, 'headBranch': head, 'state': state} for n, head, state in layers]}


def link(number: int, at, title=None, state='open', *, source='agent', linked=-DAY, updated=-HOUR,
         synced=-MINUTE, head=None, base='main', host=HOST, repository=REPOSITORY, url=None, waiting=False,
         watch=False, stack=None, author='fixture-dev', bot=False, checks=None, review=None, additions=None,
         deletions=None, mergeability=None) -> dict:
    """A ThreadPullRequestLink. `at` turns an offset in milliseconds into a timestamp, and an
    `updated` of None sends a null updatedAt. A link `waiting` for its host has a null snapshot.
    The snapshot leaves out each optional field given as None, as a host that didn't report it
    does. A draft is an open pull request with isDraft."""
    value = {'host': host, 'repository': repository, 'number': number,
             'url': url or f'https://{HOST}/{REPOSITORY}/pull/{number}', 'source': source,
             'linkedAt': at(linked), 'snapshot': None, 'stack': stack}
    if not waiting:
        snapshot = {'state': 'open' if state == 'draft' else state, 'title': title,
                    'headBranch': head or f'fixture/pr-{number}', 'baseBranch': base, 'isDraft': state == 'draft',
                    'updatedAt': None if updated is None else at(updated), 'syncedAt': at(synced)}
        if author is not None:
            snapshot['author'] = {'login': author, 'name': None, 'avatarUrl': None, 'isBot': bot}
        optional = (('additions', additions), ('deletions', deletions), ('checksState', checks),
                    ('reviewDecision', review), ('mergeability', mergeability))
        for key, field in optional:
            if field is not None:
                snapshot[key] = field
        value['snapshot'] = snapshot
    if watch:
        value['watch'] = watch_of(at(-2 * HOUR))
    return value


# The fixture's diff, two slices of whole files, with two lines changed only in whitespace, as T3's GitHub provider sends past 300 files. Every
# path and line is invented.
DIFF_FIRST = """diff --git a/src/config/reader.rs b/src/config/reader.rs
index 3f2a1c0..8b9d4e2 100644
--- a/src/config/reader.rs
+++ b/src/config/reader.rs
@@ -1,9 +1,10 @@
 use std::fs;
 use std::path::Path;
 
-use crate::config::Source;
+use crate::config::schema::Schema;
+use crate::config::sources::Source;
 
 /// Reads a config file.
 pub struct Reader {
     path: PathBuf,
 }
@@ -24,11 +25,12 @@ impl Reader {
     pub fn load(&self) -> Result<Config, Error> {
         let text = fs::read_to_string(&self.path)?;
-        let first = toml::from_str::<Config>(&text)?;
-        let config = toml::from_str::<Config>(&text)?;
-\tOk(config)
+        let config = toml::from_str::<Config>(&text)?;
+        Schema::current().check(&config)?;
+        Ok(config)
     }
 
     pub fn reload(&self) -> Result<Config, Error> {
-        self.load_unchecked()
+        // A reload reads and checks the file as a load does, so a file edited on disk since the last load can't skip the schema check.
+        self.load()
     }
 }
diff --git a/src/config/schema.rs b/src/config/schema.rs
index 51c0d7e..a04f3b9 100644
--- a/src/config/schema.rs
+++ b/src/config/schema.rs
@@ -40,4 +40,11 @@ impl Schema {
-        Self {version: 3}
+        Self { version: 3 }
     }
 
+    /// Checks `config` against the schema before anything reads it.
+    pub fn check(&self, config: &Config) -> Result<(), Error> {
+        if config.version > self.version {
+            return Err(Error::Newer(config.version));
+        }
+        Ok(())
+    }
 }
diff --git a/src/config/source.rs b/src/config/sources.rs
similarity index 91%
rename from src/config/source.rs
rename to src/config/sources.rs
index 7c1e2a4..0d4f6b1 100644
--- a/src/config/source.rs
+++ b/src/config/sources.rs
@@ -1,3 +1,3 @@
-/// Where a config comes from.
+/// Where a config comes from: a file, the environment or a default.
 pub enum Source {
     File(PathBuf),
diff --git a/assets/config-flow.png b/assets/config-flow.png
index 1d2e3f4..5a6b7c8 100644
Binary files a/assets/config-flow.png and b/assets/config-flow.png differ
diff --git a/tests/config_reload.rs b/tests/config_reload.rs
new file mode 100644
index 0000000..e3b1f20
--- /dev/null
+++ b/tests/config_reload.rs
@@ -0,0 +1,8 @@
+use fixture_config::Reader;
+
+#[test]
+fn a_reload_checks_the_schema() {
+\tlet reader = Reader::new("tests/newer.toml");
+\tassert!(reader.load().is_err());
+\tassert!(reader.reload().is_err());
+}
"""

DIFF_REST = """diff --git a/docs/config.md b/docs/config.md
index 9a8b7c6..1f2e3d4 100644
--- a/docs/config.md
+++ b/docs/config.md
@@ -12,3 +12,4 @@ The reader looks for `config.toml` in the project's root.
 
-A reload skips the schema check, so a newer file loads as it is.
+A load and a reload both check the file against the schema, and a file
+from a newer version fails with `Error::Newer`.
 
diff --git a/src/legacy/reader_v1.rs b/src/legacy/reader_v1.rs
deleted file mode 100644
index 4b5c6d7..0000000
--- a/src/legacy/reader_v1.rs
+++ /dev/null
@@ -1,4 +0,0 @@
-// The first reader, kept for the old reload path.
-pub fn load_unchecked(path: &Path) -> Config {
-    toml::from_str(&fs::read_to_string(path).unwrap()).unwrap()
-}
diff --git a/Cargo.lock b/Cargo.lock
"""


# A TypeScript change whose files the patch lists out of reading order, for the file order
# pull request. Every path and line is invented.
DIFF_TS = """diff --git a/src/settings/panel.tsx b/src/settings/panel.tsx
index 1a2b3c4..5d6e7f8 100644
--- a/src/settings/panel.tsx
+++ b/src/settings/panel.tsx
@@ -1,4 +1,5 @@
 import { useStore } from "./store";
+import { formatBytes } from "../lib/format";
 
 export function SettingsPanel() {
   const store = useStore();
diff --git a/src/settings/panel.test.tsx b/src/settings/panel.test.tsx
index 2b3c4d5..6e7f809 100644
--- a/src/settings/panel.test.tsx
+++ b/src/settings/panel.test.tsx
@@ -8,3 +8,6 @@ describe("SettingsPanel", () => {
   it("renders", () => {});
+  it("shows the cache size", () => {
+    expect(render(<SettingsPanel />).text).toContain("1.5 KB");
+  });
 });
diff --git a/src/settings/store.ts b/src/settings/store.ts
index 3c4d5e6..7f80912 100644
--- a/src/settings/store.ts
+++ b/src/settings/store.ts
@@ -1,3 +1,4 @@
 import { create } from "zustand";
+import { parseBytes } from "../lib/format";
 
 export const useStore = create(() => ({ cacheSize: 0 }));
diff --git a/pnpm-lock.yaml b/pnpm-lock.yaml
index 4d5e6f7..8091a2b 100644
--- a/pnpm-lock.yaml
+++ b/pnpm-lock.yaml
@@ -1,3 +1,3 @@
 lockfileVersion: '9.0'
-  zustand: 5.0.1
+  zustand: 5.0.2
 
diff --git a/src/lib/format.ts b/src/lib/format.ts
new file mode 100644
index 0000000..9a0b1c2
--- /dev/null
+++ b/src/lib/format.ts
@@ -0,0 +1,4 @@
+export function formatBytes(bytes: number): string {
+  return `${(bytes / 1024).toFixed(1)} KB`;
+}
+export const parseBytes = (text: string) => Number.parseFloat(text) * 1024;
"""


# Each fixture commit's own diff, as T3 reads one from GitHub's files API in one slice.
DIFF_COMMITS = {
    '9dc565a0': """diff --git a/src/config/reader.rs b/src/config/reader.rs
index 6c0e1d2..8b9d4e2 100644
--- a/src/config/reader.rs
+++ b/src/config/reader.rs
@@ -31,5 +31,6 @@ impl Reader {
 
     pub fn reload(&self) -> Result<Config, Error> {
-        self.load_unchecked()
+        // A reload reads and checks the file as a load does, so a file edited on disk since the last load can't skip the schema check.
+        self.load()
     }
 }
diff --git a/tests/config_reload.rs b/tests/config_reload.rs
new file mode 100644
index 0000000..e3b1f20
--- /dev/null
+++ b/tests/config_reload.rs
@@ -0,0 +1,8 @@
+use fixture_config::Reader;
+
+#[test]
+fn a_reload_checks_the_schema() {
+\tlet reader = Reader::new("tests/newer.toml");
+\tassert!(reader.load().is_err());
+\tassert!(reader.reload().is_err());
+}
""",
    '1f3e2a90': """diff --git a/src/config/schema.rs b/src/config/schema.rs
index 51c0d7e..a04f3b9 100644
--- a/src/config/schema.rs
+++ b/src/config/schema.rs
@@ -40,4 +40,11 @@ impl Schema {
         Self { version: 3 }
     }
 
+    /// Checks `config` against the schema before anything reads it.
+    pub fn check(&self, config: &Config) -> Result<(), Error> {
+        if config.version > self.version {
+            return Err(Error::Newer(config.version));
+        }
+        Ok(())
+    }
 }
""",
}


# The reader's marks on #7's files, for `pullRequests.filesViewed`. schema.rs was pushed to since
# its mark, and a mark on a file the diff no longer has counts for nothing.
FILES_VIEWED = [
    {'path': 'src/config/reader.rs', 'state': 'viewed'},
    {'path': 'src/config/schema.rs', 'state': 'dismissed'},
    {'path': 'tests/config_reload.rs', 'state': 'viewed'},
    {'path': 'docs/config.md', 'state': 'unviewed'},
    {'path': 'Cargo.lock', 'state': 'viewed'},
    {'path': 'src/config/old_reader.rs', 'state': 'viewed'},
]

# Each file's whole text after the change, for `pullRequests.diffFileContents`, as T3 reads it
# from GitHub's contents API at the head commit. Each matches the fixture's hunks line for line.
# Every path and line is invented.
READER_RS = """use std::fs;
use std::path::Path;

use crate::config::schema::Schema;
use crate::config::sources::Source;

/// Reads a config file.
pub struct Reader {
    path: PathBuf,
}

impl Reader {
    /// A reader of the file at `path`.
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    /// The path the reader reads.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Config, Error> {
        let text = fs::read_to_string(&self.path)?;
        let config = toml::from_str::<Config>(&text)?;
        Schema::current().check(&config)?;
        Ok(config)
    }

    pub fn reload(&self) -> Result<Config, Error> {
        // A reload reads and checks the file as a load does, so a file edited on disk since the last load can't skip the schema check.
        self.load()
    }
}

impl Default for Reader {
    /// The reader of `config.toml` in the working directory.
    fn default() -> Self {
        Self::new("config.toml")
    }
}
"""

SCHEMA_TESTS = "".join(
    f"""
    #[test]
    fn version_{n}_is_{'read' if n <= 3 else 'newer'}() {{
        let config = Config::at({n});
        assert_eq!(Schema::current().check(&config).is_ok(), {'true' if n <= 3 else 'false'});
    }}
""" for n in range(1, 31))

SCHEMA_RS = """//! The schema a config file is checked against.

use crate::config::{Config, Error};

/// The newest version of the config file this build reads.
pub const VERSION: u32 = 3;

/// What a config file may hold, by version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schema {
    /// The newest version this schema reads.
    pub version: u32,
}

/// The fields each version added, oldest first.
pub const FIELDS: [&[&str]; 3] = [
    &["path", "watch"],
    &["sources", "profile"],
    &["schema", "reload"],
];

impl Schema {
    /// The schema of `version`, or None past the newest.
    pub fn of(version: u32) -> Option<Self> {
        (1..=VERSION).contains(&version).then_some(Self { version })
    }

    /// The fields a file of this version may hold.
    pub fn fields(&self) -> impl Iterator<Item = &'static str> {
        FIELDS[..self.version as usize].iter().flat_map(|fields| fields.iter().copied())
    }

    /// Whether `field` came after this version.
    pub fn is_newer(&self, field: &str) -> bool {
        !self.fields().any(|known| known == field)
    }

    /// The schema this build reads.
    pub fn current() -> Self {
        Self { version: 3 }
    }

    /// Checks `config` against the schema before anything reads it.
    pub fn check(&self, config: &Config) -> Result<(), Error> {
        if config.version > self.version {
            return Err(Error::Newer(config.version));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
""" + SCHEMA_TESTS + """}
"""

SOURCES_RS = """/// Where a config comes from: a file, the environment or a default.
pub enum Source {
    File(PathBuf),
    Environment(String),
    Default,
}

impl Source {
    /// What a person reads for this source in `t3 config --sources`.
    pub fn label(&self) -> String {
        match self {
            Source::File(path) => path.display().to_string(),
            Source::Environment(name) => format!("${name}"),
            Source::Default => "built in".into(),
        }
    }
}
"""

CONFIG_MD = """# Configuration

The reader takes its settings from three places, in this order:

1. A `config.toml` file.
2. Environment variables that start with `FIXTURE_`.
3. The built-in defaults.

## Where the file lives

The reader looks for `config.toml` in the project's root.

A load and a reload both check the file against the schema, and a file
from a newer version fails with `Error::Newer`.

## Versions

Version 3 added `schema` and `reload`. Version 2 added `sources` and
`profile`, and version 1 had `path` and `watch`.
"""

FILE_TEXTS = {
    'src/config/reader.rs': READER_RS,
    'src/config/schema.rs': SCHEMA_RS,
    'src/config/sources.rs': SOURCES_RS,
    'docs/config.md': CONFIG_MD,
}


def diff_of(cursor, typescript=False, commit=None):
    """The fixture's diff answer for `cursor`: the first slice, or the rest after `s2`. With
    `typescript`, the one slice of DIFF_TS. With `commit`, that commit's one slice, empty for a
    commit the fixture doesn't know."""
    if commit is not None:
        return {'patch': DIFF_COMMITS.get(commit, ''), 'truncated': False, 'nextCursor': None}
    if typescript:
        return {'patch': DIFF_TS, 'truncated': False, 'nextCursor': None}
    if cursor is None:
        return {'patch': DIFF_FIRST, 'truncated': True, 'nextCursor': 's2'}
    return {'patch': DIFF_REST, 'truncated': True, 'nextCursor': None,
            'omittedFileStats': [{'path': 'Cargo.lock', 'additions': 40, 'deletions': 12}]}


def detail_of(found: dict, payload: dict) -> dict:
    """A PullRequestDetail for a fixture link. Every value is invented."""
    snapshot = found.get('snapshot') or {}
    number = found['number']
    state = snapshot.get('state', 'open')
    run = f'https://{HOST}/{REPOSITORY}/actions/runs/{9000 + number}'
    return {
        'provider': 'github',
        'capabilities': {'diff': True, 'comment': True, 'actions': ['merge', 'close'], 'mergeMethods': ['squash'],
                         'updateMethods': ['merge'], 'search': True, 'reactions': True, 'viewedFiles': 'host',
                         'review': {'inlineComment': True, 'reply': True, 'resolve': True,
                                    'verdicts': ['comment', 'approve', 'request-changes']},
                         'reviewers': {'request': True, 'listCandidates': True},
                         'edit': {'changeRequest': True, 'comment': True}, 'stacks': True, 'stackActions': False,
                         'labels': True},
        'viewerPermissions': {'stackRebase': False, 'actions': ['merge'], 'comment': True, 'resolve': True,
                              'verdicts': ['comment'], 'requestReviewers': True, 'updateMethods': ['merge'],
                              'labels': True},
        'projectId': payload.get('projectId'), 'projectTitle': 'Linked Desk', 'workspaceRoot': '/fixture/linked-desk',
        'repository': REPOSITORY, 'number': number, 'title': snapshot.get('title') or f'Fixture #{number}',
        'body': ('Splits the config reader so each source is **parsed once** and checked against '
                 'the schema before anything reads it.\n\n## Why\n\nThe old reader parsed the file '
                 'twice and skipped the check on reload.\n\n- `load` reads and checks\n- `reload` reuses it\n'
                 '- the tests cover both'),
        'url': found.get('url'), 'author': {'login': 'fixture-dev', 'name': None, 'avatarUrl': None},
        'state': state, 'isDraft': bool(snapshot.get('isDraft')), 'mergeability': 'mergeable',
        'additions': 120, 'deletions': 8, 'changedFiles': 4,
        'headBranch': snapshot.get('headBranch', f'fixture/pr-{number}'), 'headRepositoryNameWithOwner': REPOSITORY,
        'headSha': '9dc565a0', 'baseBranch': snapshot.get('baseBranch', 'main'),
        'createdAt': '2026-10-12T08:00:00Z', 'updatedAt': '2026-10-14T08:00:00Z', 'observedAt': now_ms(),
        'mergedAt': iso(now_ms() - DAY) if state == 'merged' else None,
        'closedAt': iso(now_ms() - (DAY if state == 'merged' else 2 * DAY)) if state in ('merged', 'closed') else None,
        'reviewers': [{'login': 'review-bot', 'name': None, 'avatarUrl': None, 'isBot': True},
                      {'login': 'fixture-reviewer', 'name': None, 'avatarUrl': None}],
        'labels': [{'name': 'config', 'color': '0e8a16'}, {'name': 'needs-review', 'color': None}],
        'checks': [
            {'name': 'macOS', 'status': 'success', 'description': None, 'url': run},
            {'name': 'Lint and format', 'status': 'success', 'description': None, 'url': run},
            {'name': 'Integration tests on the fixture runner', 'status': 'pending', 'description': None,
             'url': None},
            {'name': 'Deploy preview', 'status': 'action-required', 'description': None, 'url': run},
            {'name': 'Windows', 'status': 'skipped', 'description': None, 'url': None},
        ],
        'mergeCapabilities': {'merge': False, 'squash': True, 'rebase': False}, 'viewer': 'fixture-dev',
        'baseComparison': 'behind', 'behindBy': 3, 'autoMergeEnabled': False, 'autoMergeMethod': 'squash',
        'workflowApprovalsRequired': 1,
    }


def person(login: str, bot: bool = False) -> dict:
    return {'login': login, 'name': None, 'avatarUrl': None, 'isBot': bot}


def activity_of(number: int, refreshes: int = 0, threads_on_diff: bool = False) -> dict:
    """A PullRequestActivity for a fixture link. Every value is invented. #41 has a full conversation
    (12 active remarks, 14 bots' remarks, a resolved thread and a dismissed review), #51 has none and the rest one each.
    Five of #41's remarks carry reactions, one of them a bot's, and so do two of #60's.
    With `threads_on_diff`, #7 also has review threads on lines of DIFF_FIRST and one on a file the diff doesn't touch."""
    def ago(minutes: float) -> str:
        return iso(now_ms() - int(minutes * MINUTE))

    def remark(id_, kind, who, minutes, body, *, path=None, state=None, bot=False, reactions=None):
        value = {'id': id_, 'kind': kind, 'author': person(who, bot), 'body': body, 'createdAt': ago(minutes),
                 'url': f'https://{HOST}/{REPOSITORY}/pull/{number}#{id_}', 'path': path, 'reviewState': state}
        if reactions is not None:
            # (content, count, the viewer reacted). Every name is invented.
            value['reactions'] = [{'content': c, 'count': n, 'actors': ['mona'], 'viewerHasReacted': mine}
                                  for c, n, mine in reactions]
        return value

    def thread(id_, path, line, resolved, comments, side='right', outdated=False, count=None, cursor=None):
        extra = {} if cursor is None else {'nextCommentsCursor': cursor}
        return {**extra, 'id': id_, 'path': path, 'line': line, 'side': side, 'isResolved': resolved,
                'isOutdated': outdated, 'commentCount': len(comments) if count is None else count,
                'comments': [{'id': c['id'], 'author': c['author'], 'body': c['body'], 'createdAt': c['createdAt'],
                              'url': c['url']} for c in comments]}

    commits = [{'oid': '1f3e2a90', 'messageHeadline': 'Split the config reader', 'committedDate': ago(26 * 60),
                'additions': 90, 'deletions': 6, 'authors': [person('fixture-dev')]},
               {'oid': '9dc565a0', 'messageHeadline': 'Reuse the parsed config on reload', 'committedDate': ago(60),
                'additions': 30, 'deletions': 2, 'authors': [person('fixture-dev')]}]
    base = {'author': person('fixture-dev'), 'commentsTruncated': False, 'reviewThreadsTruncated': False,
            'commits': commits, 'reactions': []}
    if number == 51:
        return dict(base, reviewers=[], comments=[], reviewThreads=[], commentCount=0)
    if number == 60:
        merged_commits = [
            {'oid': '5b7c0d21', 'messageHeadline': 'Count the open links in the footer',
             'committedDate': ago(40 * 60), 'additions': 1200, 'deletions': 50,
             'authors': [person('renovate[bot]', True)]},
            {'oid': '77aa0e3f', 'messageHeadline': '', 'committedDate': ago(30 * 60),
             'authors': [person('fixture-dev')]},
            {'oid': 'c0ffee12', 'messageHeadline': 'Group the footer digits', 'committedDate': ago(28 * 60),
             'additions': 34, 'deletions': 6, 'authors': [person('fixture-dev')]},
        ]
        comments = [
            remark('m1', 'issue-comment', 'fixture-dev', 39 * 60, 'Ready for a look.',
                   reactions=[('rocket', 2, False), ('eyes', 1, True)]),
            remark('m2', 'review', 'fixture-reviewer', 35 * 60, 'Please group the digits.',
                   state='CHANGES_REQUESTED'),
            remark('m3', 'issue-comment', 'fixture-dev', 29 * 60, 'Grouped in c0ffee1.',
                   reactions=[('thumbs-up', 3, True), ('hooray', 1, False)]),
            remark('m4', 'review-comment', 'Fixture-Reviewer', 27 * 60, 'Nit: `1,234` reads better.',
                   path='src/footer.rs'),
            remark('m5', 'review', 'fixture-reviewer', 26 * 60, '', state='APPROVED'),
        ]
        # Each `refresh` adds a remark after the merge.
        for i in range(min(refreshes, 3)):
            said = ('Released in 2.4.0.', 'The footer reads well on the narrow layout too.',
                    'Thanks for the quick turnaround.')[i]
            comments.append(remark(f'n{i}', 'issue-comment', ('fixture-dev', 'mona', 'lee')[i], 3 - i, said))
        return dict(base, commits=merged_commits, reviewers=[person('fixture-reviewer')], comments=comments,
                    reviewThreads=[], commentCount=len(comments))
    if number == 7 and threads_on_diff:
        reader = 'src/config/reader.rs'
        twice = remark('d1', 'review-comment', 'mona', 200, 'This parsed the file twice.', path=reader)
        check = remark('d2', 'review-comment', 'fixture-reviewer', 90,
                       'Should `reload` check the schema too?\n\n```rust\nSchema::current().check(&config)?;\n```',
                       path=reader)
        answer = remark('d3', 'review-comment', 'fixture-dev', 60, 'It does now: `reload` goes through `load`.',
                        path=reader)
        version = remark('d4', 'review-comment', 'lee', 45,
                         'A config with no `version` reads as 0 here. Is that what we want?', path='src/config/schema.rs')
        watcher = remark('d5', 'review-comment', 'mona', 30, 'The watcher still calls `load_unchecked`.',
                         path='src/config/watch.rs')
        comments = [remark(f'c{number}', 'issue-comment', 'fixture-dev', 300, 'Opened from the fixture.'),
                    twice, check, answer, version, watcher]
        threads = [thread('d-twice', reader, 26, True, [twice], side='left'),
                   thread('d-check', reader, 28, False, [check, answer], count=4, cursor='d-check-p2'),
                   thread('d-version', 'src/config/schema.rs', 45, False, [version], outdated=True),
                   thread('d-watch', 'src/config/watch.rs', 12, False, [watcher])]
        return dict(base, reviewers=[person('mona'), person('fixture-reviewer'), person('lee')], comments=comments,
                    reviewThreads=threads, commentCount=len(comments))
    if number != 41:
        comments = [remark(f'c{number}', 'issue-comment', 'fixture-dev', 30, 'Opened from the fixture.')]
        return dict(base, reviewers=[], comments=comments, reviewThreads=[], commentCount=1)
    older = [remark(f'old{i}', 'issue-comment', 'fixture-dev', 30 * 60 - i, f'Older note {i} on the plan.')
             for i in range(1, 6)]
    stale = remark('r-mona', 'review', 'mona', 180, 'The reload path still parses the file twice.',
                   state='CHANGES_REQUESTED')
    resolved = remark('t1c1', 'review-comment', 'mona', 179,
                      '> [!NOTE]\n> This parses the file a **second** time. See [the reader](https://example.invalid/reader) '
                      'and `parse_once` for the first.',
                      path='src/config/reload.rs', reactions=[('eyes', 2, False)])
    dismissed = remark('r-lee', 'review', 'lee', 150, '', state='DISMISSED')
    linted = [remark(f'lint{i}', 'review-comment', 'lint-checks[bot]', 160 - i * 9,
                     f'Line {10 + i}: `unused_import` is set to deny. Remove the import or allow it here.',
                     path=('src/config/load.rs', 'src/config/reload.rs', 'src/config/schema.rs')[i % 3], bot=True)
              for i in range(12)]
    summary = remark('bot1', 'issue-comment', 'review-bot', 170,
                     '<!-- review-bot: summary -->**Summary.** Four files change, the tests cover both paths.',
                     bot=True)
    rebased = remark('c1', 'issue-comment', 'fixture-dev', 70,
                     'Rebased on main. `reload` now reuses the parsed config.',
                     reactions=[('thumbs-up', 2, False), ('rocket', 1, True)])
    question = remark('t2c1', 'review-comment', 'fixture-reviewer', 40,
                      'Can this reuse `parse_once`?\n\n```rust\nlet config = parse_once(&path)?;\n```',
                      path='src/config/load.rs')
    reply = remark('t2c2', 'review-comment', 'fixture-dev', 35, 'Done in 9dc565a.', path='src/config/load.rs')
    schema = remark('c2', 'issue-comment', 'fixture-reviewer', 25,
                    'One question on the schema check, otherwise this looks good.')
    approved = remark('r-reviewer', 'review', 'fixture-reviewer', 20, 'Looks good. Thanks for splitting it.',
                      state='APPROVED', reactions=[('hooray', 3, True), ('heart', 2, False)])
    bot_review = remark('bot2', 'review', 'review-bot', 15, 'No new findings.', state='COMMENTED', bot=True,
                        reactions=[('thumbs-down', 2, False)])
    preview = remark('c3', 'issue-comment', 'octo-ops', 5, 'The deploy preview waits for an approval.',
                     reactions=[('eyes', 4, False), ('thumbs-up', 1, True), ('confused', 1, False),
                                ('laugh', 12, False)])
    comments = older + [stale, resolved, summary, dismissed] + linted + [rebased, question, reply, schema, approved,
                                                                       bot_review, preview]
    threads = [thread('t1', 'src/config/reload.rs', 18, True, [resolved]),
               thread('t2', 'src/config/load.rs', 42, False, [question, reply])]
    return dict(base, reviewers=[person('review-bot', True), person('fixture-reviewer')], comments=comments,
                reviewThreads=threads, commentCount=len(comments))


def legacy_link(number: int, repository: str, url: str) -> dict:
    """A ThreadLinkedPullRequest, the one link older payloads carry."""
    return {'projectId': PROJECT_ID, 'repository': repository, 'number': number, 'url': url}


def link_preset(preset: str, at) -> dict:
    """The `pullRequests` and `linkedPullRequest` a preset sets. MISSING leaves a field out.
    `at` turns an offset in milliseconds into a timestamp."""
    only = {'linkedPullRequest': MISSING}
    if preset == 'mixed':
        stack = native_stack('stack-41', [(41, 'stack/one', 'open'), (42, 'stack/two', 'open'),
                                          (43, 'stack/three', 'open')])
        links = [
            link(41, at, 'Stack base: parse the config', source='stack', head='stack/one', updated=-50 * MINUTE,
                 stack=stack, checks='passing', review='approved', additions=120, deletions=8),
            link(43, at, 'Stack top: wire the panel', 'draft', source='stack', head='stack/three',
                 base='stack/two', updated=-40 * MINUTE, stack=stack, checks='pending', review='review-required'),
            link(42, at, 'Stack middle: read the links', head='stack/two', base='stack/one', updated=-45 * MINUTE,
                 stack=stack, watch=True, mergeability='conflicting', checks='failing',
                 review='changes-requested', additions=310, deletions=44),
            link(51, at, 'Chain base: split the reader', head='chain/one', updated=-3 * HOUR, checks='passing'),
            link(52, at, 'Chain middle: bound the text', head='chain/two', base='chain/one', updated=-2 * HOUR),
            link(53, at, 'Chain top: draw the rows', 'draft', head='chain/three', base='chain/two',
                 updated=-90 * MINUTE),
            link(60, at, 'Merged: footer counts', 'merged', updated=-DAY, author='renovate[bot]', bot=True,
                 additions=1234, deletions=56),
            link(61, at, 'Closed: an older approach', 'closed', updated=-2 * DAY),
            link(62, at, source='manual', linked=-5 * MINUTE, waiting=True),
            link(70, at, 'Watched: checks running', source='created', updated=-10 * MINUTE, watch=True,
                 checks='pending'),
            link(71, at, 'TOMBSTONE-HIDDEN: unlinked from its stack', source='stack-dismissed', updated=-MINUTE),
            link(41, at, 'DUPLICATE-COPY of #41', host='GitHub.COM ', repository=' Fixture-Org/Linked-Desk',
                 updated=-MINUTE),
            link(80, at, 'Partial: the host left fields out', source='manual', linked=-6 * HOUR, updated=None,
                 author=None),
        ]
        return dict(only, pullRequests=links)
    singles = {
        'open': {'title': 'Open: ready to merge', 'checks': 'passing', 'review': 'approved', 'additions': 42,
                 'deletions': 7, 'mergeability': 'mergeable'},
        'draft': {'title': 'Draft: still writing', 'state': 'draft', 'checks': 'pending',
                  'review': 'review-required'},
        'merged': {'title': 'Merged: landed', 'state': 'merged', 'checks': 'passing', 'review': 'approved'},
        'closed': {'title': 'Closed: not needed', 'state': 'closed', 'checks': 'failing'},
        'waiting': {'waiting': True, 'source': 'manual', 'linked': -2 * MINUTE},
        'watch': {'title': 'Watched: checks running', 'watch': True, 'checks': 'pending'},
    }
    if preset in singles:
        return dict(only, pullRequests=[link(7, at, updated=-15 * MINUTE, **singles[preset])])
    if preset == 'partial':
        bare = link(7, at, 'Partial: the host left fields out', updated=None, author=None)
        untitled = link(8, at, 'Untitled snapshot')
        del untitled['snapshot']['title']
        numberless = link(9, at, 'No number')
        del numberless['number']
        uncounted = link(11, at, 'Counted nothing', additions=0)
        return dict(only, pullRequests=[bare, untitled, numberless, uncounted])
    if preset == 'native':
        stack = native_stack('stack-21', [(21, 'native/one', 'merged'), (22, 'native/two', 'open'),
                                          (23, 'native/three', 'open')])
        links = [
            link(23, at, 'Native top: the panel', 'draft', source='stack', head='native/three', base='native/two',
                 updated=-20 * MINUTE, stack=stack),
            link(22, at, 'Native middle: the reader', source='stack', head='native/two', base='native/one',
                 updated=-30 * MINUTE, stack=stack, checks='passing'),
            link(21, at, 'Native base: the contract', 'merged', head='native/one', updated=-HOUR, stack=stack),
        ]
        return dict(only, pullRequests=links)
    if preset == 'derived':
        links = [
            link(31, at, 'Derived base', head='d/one', updated=-3 * HOUR),
            link(32, at, 'Derived middle', head='d/two', base='d/one', updated=-2 * HOUR),
            link(33, at, 'Derived top', head='d/three', base='d/two', updated=-HOUR),
            link(34, at, 'Shared head one', head='shared/head', updated=-4 * HOUR),
            link(35, at, 'Shared head two', head='shared/head', updated=-5 * HOUR),
            link(36, at, 'On the shared head', head='d/after-shared', base='shared/head', updated=-6 * HOUR),
            link(37, at, 'Cycle one', head='cycle/a', base='cycle/b', updated=-7 * HOUR),
            link(38, at, 'Cycle two', head='cycle/b', base='cycle/a', updated=-8 * HOUR),
            link(39, at, 'On the cycle', head='cycle/c', base='cycle/a', updated=-9 * HOUR),
        ]
        return dict(only, pullRequests=links)
    if preset == 'tombstone':
        links = [link(44, at, 'Shown beside a tombstone', checks='passing'),
                 link(45, at, 'TOMBSTONE-HIDDEN: unlinked from its stack', source='stack-dismissed')]
        return dict(only, pullRequests=links)
    if preset == 'duplicates':
        other = 'fixture-org/other-repo'
        links = [
            link(46, at, 'First copy shows', updated=-30 * MINUTE),
            link(46, at, 'DUPLICATE-COPY in capitals', host='GitHub.COM', repository='Fixture-Org/Linked-Desk',
                 updated=-MINUTE),
            link(46, at, 'DUPLICATE-COPY with spaces', host=' github.com ', repository=' fixture-org/linked-desk ',
                 updated=-MINUTE),
            link(46, at, 'Other repository', repository=other, url=f'https://{HOST}/{other}/pull/46',
                 updated=-2 * HOUR),
        ]
        return dict(only, pullRequests=links)
    if preset == 'hostile':
        long_stack = native_stack('s' * (KEY_BYTES + 1), [(93, 'fixture/pr-93', 'open')])
        long_host = link(91, at, 'HOST-TOO-LONG: this link must not show', host='h' * (KEY_BYTES + 76) + '.example')
        long_id = link(93, at, HOSTILE_TEXT + 'Long stack id', stack=long_stack)
        # Too long to be a time, so its details name who linked it without when.
        long_id['linkedAt'] = 'not-a-time-' + 'x' * 100
        links = [
            link(90, at, HOSTILE_TITLE, url=HOSTILE_URL, repository=HOSTILE_REPOSITORY, head=HOSTILE_BRANCH,
                 base='\x1b]52;c;eA==\x07main', author=HOSTILE_AUTHOR, checks='failing', review='approved',
                 additions=2 ** 53, deletions=0, watch=True, updated=-MINUTE),
            long_host,
            long_id,
        ]
        return dict(only, pullRequests=links)
    if preset == 'huge':
        states = ('open', 'draft', 'merged', 'closed')
        links = []
        for k in range(MOST_LINKS + 5):
            number = 1001 + k
            title = f'Bulk link {number}' + (' PAST-THE-THOUSAND' if k >= MOST_LINKS else '')
            links.append(link(number, at, title, states[k % 4], updated=-(k + 1) * MINUTE,
                              checks=('passing', 'failing', 'pending')[k % 3], additions=k, deletions=k // 2))
        return dict(only, pullRequests=links)
    if preset == 'legacy':
        return {'pullRequests': MISSING, 'linkedPullRequest': legacy_link(
            12, 'Fixture-Org/Legacy-Desk', 'https://GitHub.com:443/fixture-org/legacy-desk/pull/12')}
    if preset == 'empty':
        return {'pullRequests': [], 'linkedPullRequest': legacy_link(
            13, 'fixture-org/legacy-desk', f'https://{HOST}/fixture-org/legacy-desk/pull/13')}
    if preset == 'none':
        return {'pullRequests': MISSING, 'linkedPullRequest': MISSING}
    if preset == 'malformed':
        return dict(only, pullRequests={'links': [link(7, at, 'Inside an object, so unreadable')]})
    raise ValueError(f'no link preset {preset}')


def links_summary(record: dict) -> str:
    """A copy of a thread's links as one short phrase."""
    links = record.get('pullRequests', MISSING)
    if links is MISSING:
        text = 'no pullRequests'
    elif isinstance(links, list):
        dismissed = sum(1 for one in links if isinstance(one, dict) and one.get('source') == 'stack-dismissed')
        numbers = [one.get('number') for one in links[:8] if isinstance(one, dict)]
        text = f'{len(links)} in pullRequests, numbers {numbers}' + ('…' if len(links) > 8 else '')
        if dismissed:
            text += f', {dismissed} dismissed'
    else:
        text = f'pullRequests is {field_summary(links)}'
    linked = record.get('linkedPullRequest', MISSING)
    if isinstance(linked, dict):
        text += f'; linkedPullRequest #{safe(linked.get("number"), 12)}'
    elif linked is not MISSING:
        text += f'; linkedPullRequest {field_summary(linked)}'
    return text


# ---- the transcript's code fences: src/tui/markdown.rs ----

# thr-legacy's last reply, for features/transcript.md. Each block shows one rule of how a fence
# opens and closes: a four-backtick fence that holds a three-backtick one, a tilde fence, a
# backtick run with text after it inside a fence, backticks that are inline code, and a run
# followed by U+00A0 inside a fence. The U+00A0 is written as an escape, so it shows here. Every
# line is invented.
FENCE_REPLY = '''Fence check: a longer fence holds a shorter one.

````markdown
Run the reader once:
```sh
t3term threads --json
```
````

The config takes a tilde fence:

~~~toml
[reader]
once = true
~~~

A run with text after it stays code:

```rust ignore
let once = true;
```` not a close
```

```x`y``` is inline code, not a fence.

A run with U+00A0 after it stays code:

````rust
before_nbsp
````\u00a0
AFTER_NBSP_MUST_STAY_CODE
- still in the panel
````

End of the fence check.'''


# ---- the invented world ----


def build_world(t0: int) -> dict:
    def at(offset: int) -> str:
        return iso(t0 + offset)

    desk = {'id': PROJECT_ID, 'title': 'Linked Desk', 'workspaceRoot': '/fixture/linked-desk',
            'repositoryIdentity': None, 'createdAt': at(-20 * DAY), 'updatedAt': at(-DAY), 'deletedAt': None}
    threads, order = {}, []

    def make(tid: str, title: str, preset: str, created: int, last: int):
        thread = Thread(tid, desk, title, at(created), at(0))
        thread.set_fields(link_preset(preset, at), 'both')
        thread.history(at, 2, last)
        threads[tid] = thread
        order.append(tid)

    make('thr-mixed', 'Mixed: every state', 'mixed', -DAY, -20 * MINUTE)
    make('thr-single', 'Single: one open link', 'open', -2 * DAY, -HOUR)
    make('thr-empty', 'Empty: list cleared', 'empty', -3 * DAY, -2 * HOUR)
    make('thr-legacy', 'Legacy: one old link', 'legacy', -4 * DAY, -3 * HOUR)
    make('thr-none', 'None: no field', 'none', -5 * DAY, -4 * HOUR)
    make('thr-many', 'Many: 1,005 links', 'huge', -6 * DAY, -5 * HOUR)
    make('thr-hostile', 'Hostile: text and sizes', 'hostile', -7 * DAY, -6 * HOUR)
    make('thr-malformed', 'Malformed: not a list', 'malformed', -8 * DAY, -7 * HOUR)
    # The transcript's code fences: a reply after the last turn's on thr-legacy.
    legacy = threads['thr-legacy']
    legacy.reply(2, FENCE_REPLY, legacy.latest_run()['completedAt'])
    for thread in threads.values():
        thread.settle_record()
    # Emoji that end in U+FE0F: a reply after the last turn's on thr-none.
    none = threads['thr-none']
    none.reply(2, VS16_REPLY, none.latest_run()['completedAt'])
    return {'projects': [desk], 'threads': threads, 'order': order}

