# Pull request code

## Sub-features

- **The tab.** Where the detail's `capabilities.diff` is true, the row above a link's details reads `← List  Summary  Timeline  Code`. → or `l` on the Timeline, or a click on `Code`, shows it, and ← or `h` goes back to the Timeline. The status line reads `→ code · ← summary` on the Timeline and `← timeline` on the Code. A detail read again without `diff` takes the tab away, and a reader on the Code lands on the Summary.
- **Reading the diff.** The first time the Code shows after the details open, t3term POSTs `/api/pull-requests/diff` with the link's project, host, repository and number and no cursor. `Loading pull request diff…` shows until the first slice comes. Once a frame shows the end of the files within two pages of the view, t3term reads the next slice with the cursor the last one gave. One slice is read at a time. Nothing reads the diff before the Code shows. Going to another tab and back within the minute reads nothing, and coming back a minute or more after the first slice came reads it again.
- **The count and the note.** The count reads `5 files+` while more slices are left, with `Loading more files…` under the files while the next one is read, then `8 files`. `The host withheld part of this diff, such as a binary file or a change too large to show.` shows in yellow under it when a slice says the host left something out.
- **Files.** Each file starts folded: `› M src/config/reader.rs +7 -5`, with `A` green, `D` red, `R` blue and `M` yellow, and the counts in green and red. A rename shows both paths, `› R src/config/source.rs → src/config/sources.rs`, with the counts on the next row where they don't fit. A file whose hunks the host left out takes the host's counts, as in `› M Cargo.lock +40 -12`.
- **Order.** Each slice's files show in the desktop's reading order, not the patch's. Source files come first, each after the changed files it imports and otherwise by path. Tests come next, each after the source file whose name it has, then lockfiles, snapshots and files under `dist`, `build`, `vendor`, `__generated__` or `__snapshots__`, by path. A later slice never moves the files above it. So the first slice reads `assets/config-flow.png`, `src/config/reader.rs`, `src/config/schema.rs`, the rename and `tests/config_reload.rs`, and the second `docs/config.md`, `src/legacy/reader_v1.rs` and `Cargo.lock`.
- **Unfolding.** Enter on the chosen file, or a click on it, turns `›` into `⌄` and shows its lines. Each line has its number in grey, the new one or the old for a removed line, then `-` and the line in red, `+` and the line in green, or a space and the line. `⋯ 14 unmodified lines` comes before a hunk that doesn't start at line 1. A line that wraps leaves the number and the mark blank on its later rows, and a tab stops every 4 columns. A binary file reads `A binary file, whose changes have no lines to show.` and a file the host withheld `The host withheld this file's changes.` Enter or a click again folds the file.
- **Review threads.** A thread on a line of an unfolded file shows under that line, behind a grey bar, where the line has that number on the thread's side. Its first row reads `⌄ Open · 4 comments`, with `Resolved` in green and `· outdated` where the host says so. A thread starts open unless it is resolved. Open, it shows each comment with its author and age, then `2 more comments on the host.` where the host counts more than T3 sent. On a row with both numbers, a thread on the old side comes before one on the new. Enter or a click on the first row opens or closes it, and it stays so across reads. A folded file shows none of its threads.
- **Off the diff.** Threads on a line no file read so far shows go to a list above the files that starts closed: `› 1 conversation not on the diff read so far` while slices are left, then `› 1 conversation not on the current diff`. Open, it lists each path once, then `Line 12` and the thread.
- **Choosing.** Once a file, a thread, the list or `↻ Retry` is in view, ↓ chooses the next one and ↑ the one before, and the status line leads with what Enter does: `Enter expand file`, `Enter collapse file`, `Enter show conversation`, `Enter hide conversation`, `Enter show conversations off the diff` or `Enter retry`. Where none is in view, they scroll.
- **Reading again.** A new refresh number, `r` and a detail whose `updatedAt` changed each read the first slice again. The files read before stay, each with its fold, until the new slice replaces them, and the count goes back to `5 files+` until the end of the files comes near again. After `r` the diff waits for the refresh number `pullRequests.invalidate` raises, or for the end of the read that sent it, so T3 has forgotten the diff it held.
- **Failures.** A first slice that fails shows `Couldn't read this diff: GitHub took too long to answer.` alone. Showing the Code again, or `r`, reads it again, and the failure stays until the files come. A later slice that fails shows `t3term couldn't read the rest of this diff.`, the reason and `↻ Retry` under the files, and Retry reads that slice again. A read again of the first slice that fails keeps the files under `May be out of date: t3term couldn't read the diff again.` and the reason.

## How to get to it (user POV)

Open a thread, open its details (Alt+B), press → for `Pull requests`, choose a link and press Enter, then press → twice or click `Code`. Once a file is in view, ↑ and ↓ choose it and Enter unfolds or folds it, or click the file. PgUp, PgDn, Home, End and the wheel scroll. `r` reads the pull request and its diff from the host again.

## Driving it with t3v

Scenario `links` at 100x30. The diff route answers at once with two slices. The first holds `src/config/reader.rs`, `src/config/schema.rs`, the rename `src/config/source.rs → src/config/sources.rs`, the binary `assets/config-flow.png` and the new `tests/config_reload.rs`, with `truncated` and the cursor `s2`. The second holds `docs/config.md`, the deleted `src/legacy/reader_v1.rs` and a `Cargo.lock` with a header and no hunks, which the host counted at +40 -12. Files in the run directory change the reads: `diff-slow` holds each slice 4 s, `diff-fail` fails the first slice and `diff-fail-rest` the second with `GitHub took too long to answer.`, `diff-off` makes the detail say the host gives no diff, `diff-ts` serves a TypeScript diff in one slice, and `diff-threads` gives #7's conversation four review threads: a resolved one on removed line 26 of `src/config/reader.rs`, an open one on added line 28 whose host counts 4 comments where T3 sent 2, an outdated one on added line 45 of `src/config/schema.rs`, and one on line 12 of `src/config/watch.rs`, which the diff doesn't touch. The TypeScript diff's patch has `src/settings/panel.tsx`, which imports `./store` and `../lib/format`, then `src/settings/panel.test.tsx`, `src/settings/store.ts`, which imports `../lib/format`, `pnpm-lock.yaml` and the new `src/lib/format.ts`. The server logs each read of the diff in events.jsonl as `pr.diff` with its cursor, and `reads` lists those cursors, `null` for the first slice.

```bash
$T3V start --cols 100 --rows 30
E=$(jq -r .evidence "$RUN/run.json")
reads() { jq -s -c '[.[] | select(.event == "pr.diff") | .cursor]' "$E/events.jsonl"; }
mark() { M=$(reads | jq length); }
since() { reads | jq -c ".[$M:]"; }
settle() { local i; for i in $(seq 50); do [ "$(since)" = "$1" ] && return 0; sleep 0.2; done; since; return 1; }
$T3V click --text "Single: one open link" && $T3V wait --text "/  Single: one open link"
$T3V keys M-b && $T3V wait --text "Thread details" && $T3V keys Right && $T3V wait --text "#7 Open: ready to merge" --idle 300
$T3V keys Enter && $T3V wait --text "← List  Summary  Timeline  Code" --text "Read from github.com just now" --text "r refresh" --idle 300
[ "$(reads)" = '[]' ]   # nothing reads the diff before the Code shows

# The first slice, then the second once the end of the files is in view, each held 4 s.
touch "$RUN/diff-slow"
$T3V keys Right && $T3V wait --text "→ code · ← summary"
$T3V keys Right && $T3V wait --text "Loading pull request diff…" --text "← timeline"
$T3V capture loading-100x30
$T3V wait --text "5 files+" --text "The host withheld part of this diff, such as a" --text "› A tests/config_reload.rs +8 -0" --text "Loading more files…"
$T3V capture more-100x30
$T3V wait --text "8 files" --text "› M docs/config.md +2 -1" --text "› D src/legacy/reader_v1.rs +0 -4" --gone "Loading more files…" --idle 300
rm "$RUN/diff-slow"
[ "$(reads)" = '[null,"s2"]' ]
C=$($T3V capture files-100x30); S=$(echo "$C" | grep 'ans$'); T=$(echo "$C" | grep 'txt$')
# Each slice in reading order: source by path, as none imports another, then the test. Cargo.lock is below the view.
[ "$(grep -o '› [AMDR] [^ ]*' "$T" | paste -sd' ' -)" = '› M assets/config-flow.png › M src/config/reader.rs › M src/config/schema.rs › R src/config/source.rs › A tests/config_reload.rs › M docs/config.md › D src/legacy/reader_v1.rs' ]
grep -q '38;2;255;185;0mM.\[38;2;245;245;245m src/config/reader.rs .\[38;2;0;212;146m+7.\[38;2;245;245;245m .\[38;2;255;100;103m-5' "$S" && grep -q '38;2;43;127;255mR.\[38;2;245;245;245m src/config/source.rs → src/config/sources.rs' "$S" && grep -q '38;2;0;212;146mA.\[38;2;245;245;245m tests/config_reload.rs' "$S" && grep -q '38;2;255;100;103mD.\[38;2;245;245;245m src/legacy/reader_v1.rs' "$S" && grep -q '38;2;255;185;0mThe host withheld part' "$S"

# ↓ chooses the first file, the binary, and ↓ again the reader. Enter unfolds it: numbers in grey, removed lines red, added green.
$T3V keys Down Down && $T3V wait --text "Enter expand file"
$T3V keys Enter && $T3V wait --text "⌄ M src/config/reader.rs +7 -5" --text " 4 - use crate::config::Source;" --text " 4 + use crate::config::schema::Schema;" --text "Enter collapse file"
S=$($T3V capture reader-100x30 | grep 'ans$')
grep -q '38;2;129;129;129m 4 .\[38;2;255;100;103m- use crate::config::Source;' "$S" && grep -q '38;2;129;129;129m 4 .\[38;2;0;212;146m+ use crate::config::schema::Schema;' "$S" && grep -q '38;2;129;129;129m 1 .\[38;2;245;245;245m  use std::fs;' "$S"
# The unmodified lines before its second hunk, a tab stopping at 4 columns, and a wrapped line leaving the number and the mark blank.
$T3V keys PgDn && $T3V wait --text "⋯ 14 unmodified lines" --text "28 -     Ok(config)" --text "33 +         // A reload reads and checks the file a" --text "     s a load does, so a file edited on disk since t"
$T3V keys Home && $T3V keys Down Down && $T3V wait --text "Enter collapse file"
$T3V keys Enter && $T3V wait --text "› M src/config/reader.rs +7 -5" --gone "⋯ 14 unmodified lines"

# A click unfolds the others: the binary, the rename and the new file with its tabs.
$T3V click --text "tests/config_reload.rs" && $T3V wait --text "⌄ A tests/config_reload.rs +8 -0" --text "4 + fn a_reload_checks_the_schema() {"
$T3V click --text "src/config/source.rs" && $T3V wait --text "⌄ R src/config/source.rs → src/config/sources.rs" --text "1 - /// Where a config comes from." --text "1 + /// Where a config comes from: a file, the envir"
$T3V click --text "assets/config-flow.png" && $T3V wait --text "⌄ M assets/config-flow.png" --text "A binary file, whose changes have no lines to show."
$T3V capture kinds-100x30
$T3V keys End && $T3V wait --text "› M Cargo.lock +40 -12" --text "5 +     let reader = Reader::new(\"tests/newer.toml\")"
$T3V click --text "Cargo.lock" && $T3V wait --text "⌄ M Cargo.lock +40 -12"
$T3V keys End && $T3V wait --text "The host withheld this file's changes."

# The Timeline and back within the minute reads nothing.
mark
$T3V keys Left && $T3V wait --text "→ code · ← summary"
$T3V keys Right && $T3V wait --text "⌄ M Cargo.lock +40 -12" --text "← timeline"
[ "$(since)" = '[]' ]

# A refresh number reads the first slice again, and once more when the detail's updatedAt has moved. The folds stay. At the top of the long files the end is more than two pages away, so the second slice waits for End.
$T3V keys Home && $T3V wait --text "8 files" --idle 300
$T3V click --text "src/config/schema.rs" && $T3V wait --text "⌄ M src/config/schema.rs +8 -1"
$T3V click --text "src/config/reader.rs" && $T3V wait --text "⌄ M src/config/reader.rs +7 -5"
mark
$T3V refresh && settle '[null,null]'
$T3V wait --text "5 files+" --text "⌄ M src/config/reader.rs +7 -5" --idle 300
[ "$(since)" = '[null,null]' ]
$T3V keys End && settle '[null,null,"s2"]'
$T3V keys End && $T3V wait --text "⌄ M Cargo.lock +40 -12" --text "The host withheld this file's changes."

# The second slice fails: its reason and Retry under the files. Retry reads it again.
touch "$RUN/diff-fail-rest"
mark
$T3V refresh && settle '[null,"s2",null,"s2"]'
$T3V keys End && $T3V wait --text "t3term couldn't read the rest of this diff." --text "Couldn't read this diff: GitHub took too long to" --text "↻ Retry" --idle 300
$T3V capture retry-100x30
rm "$RUN/diff-fail-rest"
$T3V keys Up && $T3V wait --text "Enter retry"
mark
$T3V keys Enter && $T3V wait --gone "↻ Retry" --text "› M docs/config.md +2 -1"
[ "$(since)" = '["s2"]' ]
$T3V keys End && $T3V wait --text "⌄ M Cargo.lock +40 -12"

# A read again of the first slice fails: the files stay under the note. r with the read working drops it.
touch "$RUN/diff-fail"
$T3V wait --text "r refresh"
mark
$T3V keys r && settle '[null,null]'
$T3V keys Home && $T3V wait --text "8 files" --text "May be out of date: t3term couldn't read the diff" --text "Couldn't read this diff: GitHub took too long to" --text "⌄ M src/config/reader.rs +7 -5" --text "r refresh" --idle 300
$T3V capture stale-100x30
rm "$RUN/diff-fail"
mark
$T3V keys r && settle '[null,null]'
$T3V wait --text "5 files+" --gone "May be out of date" --text "r refresh" --idle 300

# A detail without a diff takes the tab away and lands on the Summary. With the diff back, the Code reads again.
touch "$RUN/diff-off"
$T3V refresh && $T3V wait --text "← List  Summary  Timeline" --text "Read from github.com just now" --text "→ timeline · ← list" --gone "Timeline  Code" --idle 300
rm "$RUN/diff-off"
mark
$T3V refresh && $T3V wait --text "← List  Summary  Timeline  Code" --text "r refresh" --idle 300
[ "$(since)" = '[]' ]
$T3V keys Right Right && $T3V wait --text "5 files+" --text "⌄ M src/config/reader.rs +7 -5"
settle '[null]'

# A TypeScript diff: format.ts, which the others import, then store.ts, which imports it, then panel.tsx, which imports both, then its test and the lockfile.
touch "$RUN/diff-ts"
mark
$T3V refresh && settle '[null,null]'
$T3V wait --text "› A src/lib/format.ts" --text "› M pnpm-lock.yaml" --gone "src/config/reader.rs" --idle 300
rm "$RUN/diff-ts"
T=$($T3V capture typescript-100x30 | grep 'txt$')
[ "$(grep -o '› [AMDR] [^ ]*' "$T" | paste -sd' ' -)" = '› A src/lib/format.ts › M src/settings/store.ts › M src/settings/panel.tsx › M src/settings/panel.test.tsx › M pnpm-lock.yaml' ]

# Review threads come with the conversation, 3 s late. One on a file the diff doesn't touch goes to a closed list above the files, whose label says whether slices are left.
touch "$RUN/diff-threads"
mark
$T3V refresh && settle '[null,null]'
$T3V wait --text "› 1 conversation not on the diff read so far" --text "⌄ M src/config/reader.rs +7 -5" --idle 300
$T3V keys End && settle '[null,null,"s2"]'
$T3V keys Home && $T3V wait --text "› 1 conversation not on the current diff" --idle 300
# Under reader.rs, the resolved thread on removed line 26 starts closed, in green, and the open one on added line 28 shows its comments behind a bar, then the ones the host has beyond T3's.
$T3V keys PgDn && $T3V wait --text "› Resolved · 1 comment" --text "⌄ Open · 4 comments" --text "Should reload check the schema too?" --idle 300
C=$($T3V capture threads-100x30); S=$(echo "$C" | grep 'ans$'); T=$(echo "$C" | grep 'txt$')
[ "$(grep -o -e '26 - ' -e 'Resolved · 1 comment' -e '28 + ' -e 'Open · 4 comments' "$T" | paste -sd'|' -)" = '26 - |Resolved · 1 comment|28 + |Open · 4 comments' ]
grep -q '38;2;0;212;146m› Resolved.\[38;2;129;129;129m · 1 comment' "$S" && grep -q '38;2;58;58;58m│ .\[38;2;245;245;245m⌄ Open.\[38;2;129;129;129m · 4 comments' "$S"
$T3V keys PgDn && $T3V wait --text "It does now: reload goes through load." --text "2 more comments on the host." --idle 300
$T3V keys PgUp && $T3V wait --text "› Resolved · 1 comment" --idle 300
# A click opens the resolved thread and closes the open one, and each stays so across a read again.
$T3V click --text "Resolved · 1 comment" && $T3V wait --text "⌄ Resolved · 1 comment" --text "This parsed the file twice." --text "Enter hide conversation"
$T3V click --text "Open · 4 comments" && $T3V wait --text "› Open · 4 comments" --gone "Should reload check the schema too?" --text "Enter show conversation"
mark
$T3V refresh && settle '[null,null]'
$T3V wait --text "⌄ Resolved · 1 comment" --text "› Open · 4 comments" --idle 300
# ↓ from the top chooses the list, and Enter opens it: each path once, then the line and the thread.
$T3V keys Home Down && $T3V wait --text "Enter show conversations off the diff"
$T3V keys Enter && $T3V wait --text "⌄ 1 conversation not on the diff read so far" --text "src/config/watch.rs" --text "Line 12" --text "The watcher still calls load_unchecked." --text "Enter hide conversations off the diff"
$T3V capture off-diff-100x30
$T3V keys Enter && $T3V wait --text "› 1 conversation not on the diff read so far" --gone "Line 12"
# A folded file's threads don't show. Below, schema.rs has an outdated thread on added line 45.
$T3V click --text "src/config/reader.rs" && $T3V wait --text "› M src/config/reader.rs +7 -5" --gone "Resolved · 1 comment" --gone "Open · 4 comments"
$T3V keys PgDn && $T3V wait --text "⌄ Open · 1 comment · outdated" --text "A config with no version reads as 0 here."
rm "$RUN/diff-threads"

# Another thread's link, whose first slice fails: the failure alone. Showing the Code again reads it again, and so does opening the details again.
touch "$RUN/diff-fail"
$T3V keys Escape && $T3V wait --gone "Thread details"
$T3V click --text "Mixed: every state" && $T3V wait --text "/  Mixed: every state"
$T3V keys M-b && $T3V wait --text "Pull requests 11" --idle 300
$T3V keys Down Enter && $T3V wait --text "#70 Watched: checks running" --text "Read from github.com just now" --text "r refresh" --idle 300
mark
$T3V keys Right Right && $T3V wait --text "Couldn't read this diff: GitHub took too long to" --gone "8 files" --gone "5 files+" --idle 300
$T3V capture failed-100x30
[ "$(since)" = '[null]' ]
$T3V keys Left && $T3V wait --text "→ code"
$T3V keys Right && settle '[null,null]'
$T3V wait --text "Couldn't read this diff: GitHub took too long to" --text "← timeline" --idle 300
rm "$RUN/diff-fail"
$T3V keys Left Left Left && $T3V wait --gone "← List"
$T3V keys Enter && $T3V wait --text "#70 Watched: checks running" --text "← List  Summary  Timeline  Code" --idle 300
$T3V keys Right Right && $T3V wait --text "8 files" --text "› M src/config/reader.rs +7 -5"
[ "$(since)" = '[null,null,null,"s2"]' ]

# At 80x24 every row still fits the pane.
$T3V resize 80 24 && $T3V wait --text "8 files" --idle 300
$T3V keys Down Down Enter && $T3V wait --text "⌄ M src/config/reader.rs +7 -5" --text " 4 + use crate::config::schema::Schema;" --text "Enter collapse file"
$T3V capture reader-80x24
$T3V status --json | jq -e '.scopeViolations == 0 and .unknownRpcs == 0 and .fixtureErrors == 0'
```

## Gotchas

- The diff comes over HTTP, so `rpcRequestsByTag` doesn't count it. Count the `pr.diff` lines in events.jsonl, which also give each read's cursor.
- Each refresh number moves the fixture detail's `updatedAt`. So `t3v refresh` and `r` read the first slice twice: at once, then again when the read of the detail and the conversation is done, 3 s later. `settle` waits for both.
- `8 files` is the first row of the tab, so it scrolls away with the files. Wait for a file of the second slice instead, such as `› M docs/config.md`.
- End before the second slice comes stops at the end of the first, and the second slice's files come in under the view. Press End again to see them.
- `Refreshing from github.com…` shows in the header, which scrolls away on the Code. Wait for `r refresh` to come back instead.
- `r` does nothing until the read of the detail and the conversation is done, 3 s after it starts. Wait for `r refresh` in the status line before each press.
- A first slice that failed shows its failure, not `Loading pull request diff…`, while it is read again.
- Switching threads drops the read and its Code, so coming back reads the diff from the start with every file folded. Closing the dialog with Esc and opening it again keeps them.
- The recipe doesn't wait out the minute after which showing the Code reads it again, though it checks that showing it again reads a first slice that failed, and the fixture can't serve an empty diff (`This pull request has no file changes.`) or one the parser can't split.
- Clicks land on the rows the last frame drew. Click the lowest file first when unfolding several, so the rows above don't move.
