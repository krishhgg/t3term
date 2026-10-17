//! A hunk's lines in the order the desktop stacks them, and in the pairs it shows side by side.
//! @pierre/diffs 1.5.2 takes every line removed or added between two in context as one change,
//! in whatever order they come (utils/parsePatchFiles.js:182-205). Where a change removes more
//! lines than it adds, or adds more than it removes, pierre can split it so the lines that pair
//! are the most alike, where otherwise the first line removed pairs with the first added
//! (utils/realignChangeContent.js, which parsePatchFiles.js:296 runs on every file). The longer
//! side's lines left over then show before the lines that pair, after them, or some before and
//! the rest after. Stacked, each part of a change shows its lines removed and then those it
//! adds, and side by side, it pairs them in order, with nothing beside the rest of its longer
//! side (utils/iterateOverDiff.js:213 and 440-446).

use super::Pair;
use super::whitespace::js_space;
use crate::pull_request::patch::{Kind, Line};

/// The most pairs of lines a change compares, as pierre's `MAX_ALIGNMENT_COMPARISONS`: each
/// pair at each place the lines that pair could start. A larger change pairs its lines in order.
const MOST_COMPARISONS: usize = 4096;

/// How much more alike each pair has to be on average for a change's lines to pair otherwise
/// than in order, as pierre's `MIN_IMPROVEMENT_PER_PAIR`.
const LEAST_GAIN: f64 = 0.5;

/// A line of a hunk, followed by the note that it has no line end where the patch has one.
type Noted<'a> = &'a [Line];

/// A part of a hunk that shows as one.
enum Part<'p, 'a> {
    Context(Noted<'a>),
    /// Lines removed and lines added, which pair in order.
    Change(&'p [Noted<'a>], &'p [Noted<'a>]),
    /// A note with no line before it, which shows only where the lines stack.
    Note(&'a Line),
}

/// The lines of a hunk's `lines`, whose text is in `text`, in the order they stack: each part
/// of a change with its lines removed before those it adds, and each note that a line has no
/// line end after that line.
pub(super) fn stacked<'a>(text: &str, lines: &'a [Line]) -> Vec<&'a Line> {
    let mut order = Vec::with_capacity(lines.len());
    parts(text, lines, &mut |part| match part {
        Part::Context(noted) => order.extend(noted),
        Part::Change(removed, added) => {
            order.extend(removed.iter().chain(added).copied().flatten())
        }
        Part::Note(note) => order.push(note),
    });
    order
}

/// The rows a hunk's `lines`, whose text is in `text`, take side by side: a line in context
/// beside itself, and in each part of a change, the lines removed beside those added in order,
/// with nothing beside the rest of the longer side. The notes that a line has no line end show
/// under the hunk, as `ends` says.
pub(super) fn pairs<'a>(text: &str, lines: &'a [Line]) -> Vec<Pair<'a>> {
    let mut rows = Vec::with_capacity(lines.len());
    parts(text, lines, &mut |part| match part {
        Part::Context(noted) => rows.push((Some(&noted[0]), Some(&noted[0]))),
        Part::Change(removed, added) => {
            let line = |side: Noted<'a>| &side[0];
            let count = removed.len().max(added.len());
            rows.extend((0..count).map(|at| {
                let removed = removed.get(at).copied().map(line);
                (removed, added.get(at).copied().map(line))
            }));
        }
        Part::Note(_) => {}
    });
    rows
}

/// Gives `each` the parts of a hunk's `lines`, whose text is in `text`, in order.
fn parts<'a>(text: &str, lines: &'a [Line], each: &mut impl FnMut(Part<'_, 'a>)) {
    let (mut removed, mut added) = (Vec::new(), Vec::new());
    let mut at = 0;
    while at < lines.len() {
        let notes = lines[at + 1..]
            .iter()
            .take_while(|line| line.kind == Kind::NoNewline)
            .count();
        let noted = &lines[at..at + 1 + notes];
        at += noted.len();
        match noted[0].kind {
            Kind::Removed => removed.push(noted),
            Kind::Added => added.push(noted),
            kind => {
                change(text, &removed, &added, each);
                removed.clear();
                added.clear();
                match kind {
                    Kind::Context => each(Part::Context(noted)),
                    // A note after another line goes with that line, so this one starts the
                    // hunk.
                    _ => noted.iter().for_each(|note| each(Part::Note(note))),
                }
            }
        }
    }
    change(text, &removed, &added, each);
}

/// Gives `each` the parts of a change of `removed` and `added` lines, whose text is in `text`:
/// the whole change, or where the lines that pair start past the first of the longer side's,
/// as `start` finds, the lines before them, the lines that pair and the lines after them, each
/// where it has any.
fn change<'p, 'a>(
    text: &str,
    removed: &'p [Noted<'a>],
    added: &'p [Noted<'a>],
    each: &mut impl FnMut(Part<'_, 'a>),
) {
    if removed.is_empty() && added.is_empty() {
        return;
    }
    let first = start(text, removed, added);
    if first == 0 {
        each(Part::Change(removed, added));
        return;
    }
    let pairs = removed.len().min(added.len());
    let (old, new) = match added.len() > removed.len() {
        true => (0, first),
        false => (first, 0),
    };
    let parts = [
        (&removed[..old], &added[..new]),
        (&removed[old..old + pairs], &added[new..new + pairs]),
        (&removed[old + pairs..], &added[new + pairs..]),
    ];
    for (removed, added) in parts {
        if !removed.is_empty() || !added.is_empty() {
            each(Part::Change(removed, added));
        }
    }
}

/// How many of the longer side's lines come before the lines that pair with the shorter side's,
/// as pierre's `realignChangeBlock` finds it: the place where the pairs are the most alike in
/// sum, as `alike` measures each, though the first place counts `LEAST_GAIN` more for each
/// pair, and of places as alike, the first. A change with as many lines on each side, or none on
/// one, or that would compare more than `MOST_COMPARISONS` pairs, pairs them from the first.
fn start(text: &str, removed: &[Noted], added: &[Noted]) -> usize {
    let pairs = removed.len().min(added.len());
    let surplus = removed.len().abs_diff(added.len());
    if pairs == 0 || surplus == 0 || pairs * (surplus + 1) > MOST_COMPARISONS {
        return 0;
    }
    // Each line's text without its whitespace, in UTF-16 code units, as JavaScript compares
    // strings.
    let bare = |lines: &[Noted]| -> Vec<Vec<u16>> {
        lines
            .iter()
            .map(|noted| {
                let line = text.get(noted[0].text.clone()).unwrap_or_default();
                let kept: String = line.chars().filter(|&c| !js_space(c)).collect();
                kept.encode_utf16().collect()
            })
            .collect()
    };
    let (removed, added) = (bare(removed), bare(added));
    let longer_added = added.len() > removed.len();
    let mut best = (0, 0.0);
    for first in 0..=surplus {
        let (old, new) = match longer_added {
            true => (0, first),
            false => (first, 0),
        };
        // Summed in order, as pierre sums them, so the floating point sums come out the same.
        let mut score = 0.0;
        for pair in 0..pairs {
            score += alike(&removed[old + pair], &added[new + pair]);
        }
        if first == 0 {
            best = (0, score + pairs as f64 * LEAST_GAIN);
        } else if score > best.1 {
            best = (first, score);
        }
    }
    best.0
}

/// How alike two lines are, as pierre's `lineSimilarity` measures it: 1 where they are the same,
/// and otherwise the code units they start with and end with in common, without counting one
/// twice, over the longer line's. An empty line is like no other.
fn alike(a: &[u16], b: &[u16]) -> f64 {
    if a == b {
        return 1.0;
    }
    let longer = a.len().max(b.len());
    let shorter = a.len().min(b.len());
    if shorter == 0 {
        return 0.0;
    }
    let prefix = a.iter().zip(b).take_while(|(a, b)| a == b).count();
    let suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take(shorter - prefix)
        .take_while(|(a, b)| a == b)
        .count();
    (prefix + suffix) as f64 / longer as f64
}

#[cfg(test)]
mod tests;
