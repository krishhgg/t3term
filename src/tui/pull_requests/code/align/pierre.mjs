// Writes pierre.json: random patches and, for each hunk, its parts as the desktop shows them,
// which tests.rs checks t3term's against. @pierre/diffs 1.5.2, the version the desktop pins,
// reads each patch with `parsePatchFiles`, which splits each change by how alike its lines are
// (utils/realignChangeContent.js). Each hunk's parts are written as the patch has them, and
// again once the desktop hides the changes to whitespace, with `hideWhitespaceChanges` as
// apps/web/src/lib/diffRendering.ts:87-138 has it at f570bd2, which runs jsdiff 9.0.0. Link
// their packages into node_modules beside this file, as node_modules/@pierre/diffs and
// node_modules/diff, then:
//
//   node --preserve-symlinks pierre.mjs SEED CASES MOST_LINES > pierre.json
//
// pierre.json was written with SEED 7, CASES 70 and MOST_LINES 14.
import { parsePatchFiles } from "./node_modules/@pierre/diffs/dist/utils/parsePatchFiles.js";
import { parseDiffFromFile } from "./node_modules/@pierre/diffs/dist/utils/parseDiffFromFile.js";

function hideWhitespaceChanges(file) {
  const hunks = file.hunks.map((hunk) => {
    const oldContents = file.deletionLines
      .slice(hunk.deletionLineIndex, hunk.deletionLineIndex + hunk.deletionCount)
      .map((line) => `${line.replace(/\s/g, "")}\n`)
      .join("");
    const newContents = file.additionLines
      .slice(hunk.additionLineIndex, hunk.additionLineIndex + hunk.additionCount)
      .map((line) => `${line.replace(/\s/g, "")}\n`)
      .join("");
    const filtered = parseDiffFromFile(
      { name: file.name, contents: oldContents },
      { name: file.name, contents: newContents },
      { context: Infinity },
    ).hunks[0];
    return {
      ...hunk,
      hunkContent: filtered
        ? filtered.hunkContent.map((content) => ({
            ...content,
            additionLineIndex: content.additionLineIndex + hunk.additionLineIndex,
            deletionLineIndex: content.deletionLineIndex + hunk.deletionLineIndex,
          }))
        : [
            {
              type: "context",
              lines: hunk.additionCount,
              additionLineIndex: hunk.additionLineIndex,
              deletionLineIndex: hunk.deletionLineIndex,
            },
          ],
    };
  });
  return { ...file, hunks };
}

function mulberry32(seed) {
  return function () {
    seed |= 0;
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const seed = Number(process.argv[2] ?? 1);
const count = Number(process.argv[3] ?? 100);
const most = Number(process.argv[4] ?? 12);
const random = mulberry32(seed);
const pick = (items) => items[Math.floor(random() * items.length)];
const below = (n) => Math.floor(random() * n);

// Lines of code, some alike at the start or the end, and some whose likeness depends on
// counting a character outside the Basic Multilingual Plane as two code units.
const BASES = [
  "let value = compute(first, second);",
  "let value = compute(first);",
  "return value;",
  "return other;",
  "}",
  "fn load(&self) -> Result<Config, Error> {",
  "fn save(&self) -> Result<(), Error> {",
  "// 😀 note",
  "// 😃 note",
  "// 🙂",
  "x",
  "日本語のテキスト",
  "日本語",
];
const SPACES = [" ", "  ", "\t", "\u00a0", "\u3000", "\ufeff"];
// Not whitespace to JavaScript.
const NOT_SPACES = ["\u0085", "\u200b"];

// A line like `base`: the same, or with a few characters added or taken away at either end,
// and now and then with whitespace or a character JavaScript doesn't count as whitespace added.
function like(base) {
  let line = base;
  const roll = random();
  if (roll < 0.2) line = line + pick(["x", ";", "😀", " // done"]);
  else if (roll < 0.35) line = pick(["  ", "\t", "pub ", "🙂"]) + line;
  // Cut by characters, so no character outside the Basic Multilingual Plane loses half.
  const chars = Array.from(line);
  if (roll >= 0.35 && roll < 0.5 && chars.length > 2) {
    line = chars.slice(0, chars.length - 1 - below(2)).join("");
  } else if (roll >= 0.5 && roll < 0.6 && chars.length > 2) {
    line = chars.slice(1 + below(2)).join("");
  }
  if (random() < 0.15) line = pick(SPACES) + line;
  if (random() < 0.1) line = line + pick(SPACES);
  if (random() < 0.04) line = line + pick(NOT_SPACES);
  if (random() < 0.04) line = line + "\r";
  return line;
}

function text() {
  if (random() < 0.08) return "";
  if (random() < 0.04) return pick(SPACES).repeat(1 + below(2));
  return like(pick(BASES));
}

// A hunk's lines as git writes them: runs of context and of changes, each change's removed
// lines before its added ones, or now and then mixed.
function hunkLines(size) {
  const lines = [];
  while (lines.length < size) {
    if (random() < 0.3) {
      const n = 1 + below(3);
      for (let i = 0; i < n; i++) lines.push([" ", text()]);
      continue;
    }
    // Most changes have a side longer by one to three lines.
    let removed = below(5);
    let added = below(5);
    if (random() < 0.7) {
      if (random() < 0.5) added = removed + 1 + below(3);
      else removed = added + 1 + below(3);
    }
    // The shorter side's lines, and the longer side's: lines like those, in order, with
    // others put in among them.
    const shorter = [];
    for (let i = 0; i < Math.min(removed, added); i++) shorter.push(text());
    const longer = shorter.map((line) => (random() < 0.7 ? like(line) : text()));
    while (longer.length < Math.max(removed, added)) {
      longer.splice(below(longer.length + 1), 0, text());
    }
    const [olds, news] = removed < added ? [shorter, longer] : [longer, shorter];
    const change = [...olds.map((line) => ["-", line]), ...news.map((line) => ["+", line])];
    if (random() < 0.08) change.sort(() => random() - 0.5);
    lines.push(...change);
  }
  if (lines.length === 0) lines.push([" ", "a"]);
  return lines;
}

const NO_NEWLINE = "\\ No newline at end of file";

function file(index) {
  let patch = `diff --git a/f${index}.txt b/f${index}.txt\n--- a/f${index}.txt\n+++ b/f${index}.txt\n`;
  const hunks = 1 + below(3);
  let oldStart = 1 + below(5);
  let newStart = oldStart;
  for (let h = 0; h < hunks; h++) {
    const lines = hunkLines(1 + below(most));
    if (h === hunks - 1 && random() < 0.3) {
      // The last lines lack a line end on one side or both.
      const kind = pick(["old", "new", "each"]);
      if (kind === "new") {
        lines.push(["+", text() + "z"]);
        lines.push(["\\", NO_NEWLINE]);
      } else if (kind === "old") {
        lines.push(["-", text() + "y"]);
        lines.push(["\\", NO_NEWLINE]);
        const added = below(4);
        for (let i = 0; i < added; i++) lines.push(["+", text() + "w"]);
      } else {
        lines.push(["-", text() + "y"]);
        lines.push(["\\", NO_NEWLINE]);
        lines.push(["+", text() + "y"]);
        lines.push(["\\", NO_NEWLINE]);
      }
    }
    const oldCount = lines.filter(([m]) => m === " " || m === "-").length;
    const newCount = lines.filter(([m]) => m === " " || m === "+").length;
    const os = oldCount === 0 ? oldStart - 1 : oldStart;
    const ns = newCount === 0 ? newStart - 1 : newStart;
    patch += `@@ -${os},${oldCount} +${ns},${newCount} @@\n`;
    for (const [mark, body] of lines) patch += `${mark === "\\" ? "" : mark}${body}\n`;
    const gap = 3 + below(6);
    oldStart += oldCount + gap;
    newStart += newCount + gap;
  }
  return patch;
}

// A hunk's parts in order, each a part in context with its count of lines, or a change with
// its counts of lines removed and added. Each part starts where the one before it ends.
function parts(hunk) {
  let deletion = hunk.deletionLineIndex;
  let addition = hunk.additionLineIndex;
  return hunk.hunkContent.map((content) => {
    // A side with no lines starts before the first, which jsdiff's patch names as 0, so only
    // a side with lines is checked.
    const context = content.type === "context";
    if ((context || content.deletions > 0) && content.deletionLineIndex !== deletion) {
      throw new Error("out of order");
    }
    if ((context || content.additions > 0) && content.additionLineIndex !== addition) {
      throw new Error("out of order");
    }
    if (content.type === "context") {
      deletion += content.lines;
      addition += content.lines;
      return ["context", content.lines];
    }
    deletion += content.deletions;
    addition += content.additions;
    return ["change", content.deletions, content.additions];
  });
}

const cases = [];
for (let c = 0; c < count; c++) {
  let patch = "";
  const files = 1 + below(2);
  for (let f = 0; f < files; f++) patch += file(f);
  // The desktop trims the patch, so a last line of whitespace would lose it.
  if (patch.trim() + "\n" !== patch) continue;
  const parsed = parsePatchFiles(patch.trim(), `case-${c}`);
  const shown = [];
  const hidden = [];
  for (const parsedPatch of parsed) {
    for (const source of parsedPatch.files) {
      shown.push(source.hunks.map(parts));
      hidden.push(hideWhitespaceChanges(source).hunks.map(parts));
    }
  }
  cases.push({ patch, shown, hidden });
}
// One case to a line.
process.stdout.write(`[\n${cases.map((c) => JSON.stringify(c)).join(",\n")}\n]\n`);
