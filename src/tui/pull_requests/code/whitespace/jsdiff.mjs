// Writes jsdiff.json: random patches and, for each hunk, its runs of context and change once
// the desktop's whitespace filter ran, which tests.rs checks t3term's against. The filter is
// `hideWhitespaceChanges` as apps/web/src/lib/diffRendering.ts:87-138 has it at f570bd2, and
// runs @pierre/diffs 1.5.2 and jsdiff 9.0.0, the versions the desktop pins. Link their packages
// into node_modules beside this file, as node_modules/@pierre/diffs and node_modules/diff, then:
//
//   node --preserve-symlinks jsdiff.mjs SEED CASES MOST_LINES > jsdiff.json
//
// jsdiff.json was written with SEED 42, CASES 80 and MOST_LINES 10. The runs of change the
// desktop splits by likeness (utils/realignChangeContent.js) are joined again, as t3term's
// `hide` keeps each run whole and `align` splits it after, which align/pierre.mjs checks.
import { parsePatchFiles } from "./node_modules/@pierre/diffs/dist/utils/parsePatchFiles.js";
import { parseDiffFromFile } from "./node_modules/@pierre/diffs/dist/utils/parseDiffFromFile.js";

function hideWhitespaceChanges(file) {
  let splitDelta = 0;
  let unifiedDelta = 0;
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
    const next = {
      ...hunk,
      additionLines: filtered?.additionLines ?? 0,
      deletionLines: filtered?.deletionLines ?? 0,
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
      splitLineStart: hunk.splitLineStart + splitDelta,
      unifiedLineStart: hunk.unifiedLineStart + unifiedDelta,
      splitLineCount: filtered?.splitLineCount ?? hunk.additionCount,
      unifiedLineCount: filtered?.unifiedLineCount ?? hunk.additionCount,
    };
    splitDelta += next.splitLineCount - hunk.splitLineCount;
    unifiedDelta += next.unifiedLineCount - hunk.unifiedLineCount;
    return next;
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

const WORDS = ["a", "b", "}", "{", "x=1", "if(a){", "return", "fn f()", "é", "日本", "😀"];
const SPACES = [" ", "  ", "\t", "\u00a0", "\u2003", "\ufeff", "\u3000", "\u000b", "\u000c", "\u2028", "\u1680", "\u202f"];
// Not whitespace to JavaScript.
const NOT_SPACES = ["\u0085", "\u200b", "\u180e"];

function text() {
  if (random() < 0.08) return "";
  if (random() < 0.06) return pick(SPACES).repeat(1 + below(3));
  let word = pick(WORDS);
  if (random() < 0.4) word = pick(SPACES) + word;
  if (random() < 0.3) word = word + pick(SPACES);
  if (random() < 0.15 && word.length > 1) {
    const at = 1 + below(word.length - 1);
    if (!/[\ud800-\udfff]/.test(word.slice(at - 1, at + 1))) {
      word = word.slice(0, at) + pick(SPACES) + word.slice(at);
    }
  }
  if (random() < 0.05) word = word + pick(NOT_SPACES);
  if (random() < 0.05) word = word + "\r";
  return word;
}

// A hunk's lines as git writes them: runs of context and of changes, each change's removed
// lines before its added ones, or now and then mixed.
function hunkLines(size) {
  const lines = [];
  while (lines.length < size) {
    const roll = random();
    if (roll < 0.4) {
      const n = 1 + below(4);
      for (let i = 0; i < n; i++) lines.push([" ", text()]);
    } else {
      const removed = below(5);
      const added = below(5);
      const change = [];
      for (let i = 0; i < removed; i++) change.push(["-", text()]);
      for (let i = 0; i < added; i++) {
        // Often the same line with its whitespace changed.
        if (i < removed && random() < 0.6) {
          const base = change[i][1].replace(/\s/g, "");
          let word = base;
          if (random() < 0.5) word = pick(SPACES) + word;
          if (random() < 0.5) word = word + pick(SPACES);
          change.push(["+", word]);
        } else {
          change.push(["+", text()]);
        }
      }
      if (random() < 0.1) change.sort(() => random() - 0.5);
      lines.push(...change);
    }
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
    let lines = hunkLines(1 + below(most));
    if (h === hunks - 1 && random() < 0.35) {
      // The last lines lack a line end on one side or both.
      const kind = pick(["both", "old", "new", "each"]);
      if (kind === "both") {
        lines.push([" ", pick(WORDS)]);
        lines.push(["\\", NO_NEWLINE]);
      } else if (kind === "new") {
        lines.push(["+", text() + "z"]);
        lines.push(["\\", NO_NEWLINE]);
      } else if (kind === "old") {
        lines.push(["-", text() + "y"]);
        lines.push(["\\", NO_NEWLINE]);
        const added = below(3);
        for (let i = 0; i < added; i++) lines.push(["+", text() + "w"]);
      } else {
        lines.push(["-", text() + "y"]);
        lines.push(["\\", NO_NEWLINE]);
        lines.push(["+", text() + "y"]);
        lines.push(["\\", NO_NEWLINE]);
      }
    }
    // A patch's last line can't be blank, as the desktop trims the patch.
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

const cases = [];
for (let c = 0; c < count; c++) {
  let patch = "";
  const files = 1 + below(3);
  for (let f = 0; f < files; f++) patch += file(f);
  // The desktop trims the patch, so a last line of whitespace would lose it.
  if (patch.trim() + "\n" !== patch) continue;
  const parsed = parsePatchFiles(patch.trim(), `case-${c}`);
  const out = [];
  for (const parsedPatch of parsed) {
    for (const source of parsedPatch.files) {
      const filtered = hideWhitespaceChanges(source);
      out.push(
        filtered.hunks.map((hunk, h) => {
          const base = source.hunks[h];
          const blocks = [];
          for (const content of hunk.hunkContent) {
            // A side with no lines starts before the first, which jsdiff's patch names as 0.
            const del = Math.max(0, content.deletionLineIndex - base.deletionLineIndex);
            const add = Math.max(0, content.additionLineIndex - base.additionLineIndex);
            const last = blocks.at(-1);
            if (content.type === "context") {
              blocks.push(["context", content.lines, del, add]);
            } else if (last && last[0] === "change") {
              last[1] += content.deletions;
              last[2] += content.additions;
            } else {
              blocks.push(["change", content.deletions, content.additions, del, add]);
            }
          }
          return { blocks, additions: hunk.additionLines, deletions: hunk.deletionLines };
        }),
      );
    }
  }
  cases.push({ patch, files: out });
}
process.stdout.write(JSON.stringify(cases));
