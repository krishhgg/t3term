//! A diff's hunks with the changes to whitespace hidden, as the desktop's
//! `hideWhitespaceChanges` gives them (apps/web/src/lib/diffRendering.ts:87-138 at f570bd2).
//! Each hunk's lines before the change and after it are compared with every whitespace
//! character taken out, by jsdiff 9.0.0's line diff, which @pierre/diffs 1.5.2 runs
//! (libesm/diff/base.js). A line the same on both sides but for whitespace shows unchanged,
//! with its text and number after the change. Between two of those, the lines removed show
//! before the lines added, as the desktop's stacked view puts them.

use std::collections::HashMap;

use crate::pull_request::patch::{File, Hunk, Kind, Line};

/// The most steps comparing one slice's hunks takes: each diagonal tried and each pair of lines
/// compared. Past it, the hunks left show as the patch has them. A hunk that removes 1,000
/// lines and adds 1,000 others, none the same without whitespace, takes about 1,000,000.
pub(super) const MOST_STEPS: usize = 4_000_000;

/// A slice's files with the changes to whitespace hidden.
pub(super) struct Hidden {
    pub(super) files: Vec<File>,
    /// How many hunks show as the patch has them, as comparing them would pass `MOST_STEPS`.
    pub(super) kept: usize,
}

/// `files`, whose lines are ranges of `patch`, with the changes to whitespace hidden. The hunks
/// are compared from the one with the fewest lines up, so only the largest are left out once
/// the steps run out. A file's counts lose a line added and one removed for each pair that
/// shows unchanged, so those of hunks left out past the bound stay.
pub(super) fn hide(patch: &str, files: &[File]) -> Hidden {
    let mut order: Vec<(usize, usize)> = files
        .iter()
        .enumerate()
        .flat_map(|(at, file)| (0..file.hunks.len()).map(move |hunk| (at, hunk)))
        .collect();
    order.sort_by_key(|&(at, hunk)| files[at].hunks[hunk].lines.len());
    let mut compared: Vec<Vec<Option<Rebuilt>>> = files
        .iter()
        .map(|file| file.hunks.iter().map(|_| None).collect())
        .collect();
    let mut left = MOST_STEPS;
    for (at, hunk) in order {
        compared[at][hunk] = rebuilt(patch, &files[at].hunks[hunk], &mut left);
    }
    let mut kept = 0;
    let files = files
        .iter()
        .zip(compared)
        .map(|(file, compared)| {
            let mut joined = 0;
            let hunks = file
                .hunks
                .iter()
                .zip(compared)
                .map(|(hunk, compared)| {
                    let lines = match compared {
                        Some((lines, pairs)) => {
                            joined += pairs;
                            lines
                        }
                        None => {
                            kept += 1;
                            hunk.lines.clone()
                        }
                    };
                    Hunk {
                        lines,
                        section: hunk.section.clone(),
                        ..*hunk
                    }
                })
                .collect();
            File {
                hunks,
                additions: file.additions.saturating_sub(joined),
                deletions: file.deletions.saturating_sub(joined),
                old_path: file.old_path.clone(),
                new_path: file.new_path.clone(),
                ..*file
            }
        })
        .collect();
    Hidden { files, kept }
}

/// A hunk's lines compared without whitespace, and how many more show unchanged than in the
/// patch.
type Rebuilt = (Vec<Line>, u64);

/// `hunk`'s lines as `Rebuilt` says. None where comparing them would take more steps than are
/// `left`.
fn rebuilt(patch: &str, hunk: &Hunk, left: &mut usize) -> Option<Rebuilt> {
    let old: Vec<&Line> = hunk
        .lines
        .iter()
        .filter(|line| matches!(line.kind, Kind::Context | Kind::Removed))
        .collect();
    let new: Vec<&Line> = hunk
        .lines
        .iter()
        .filter(|line| matches!(line.kind, Kind::Context | Kind::Added))
        .collect();
    // The note that a side's last line has no line end, as @pierre/diffs reads it: after a
    // line in context it is both sides', and after a line removed or added that side's
    // (utils/parsePatchFiles.js:225-230).
    let (mut old_end, mut new_end) = (None, None);
    let mut before = None;
    for line in &hunk.lines {
        match (line.kind, before) {
            (Kind::NoNewline, Some(Kind::Context)) => (old_end, new_end) = (Some(line), Some(line)),
            (Kind::NoNewline, Some(Kind::Removed)) => old_end = Some(line),
            (Kind::NoNewline, Some(Kind::Added)) => new_end = Some(line),
            (Kind::NoNewline, _) => {}
            (kind, _) => before = Some(kind),
        }
    }
    // Each line's text without whitespace, as a number shared by the lines with the same.
    let mut ids: HashMap<String, u32> = HashMap::new();
    let mut id = |line: &Line| {
        let text = patch.get(line.text.clone()).unwrap_or_default();
        let stripped: String = text.chars().filter(|&c| !js_space(c)).collect();
        let next = ids.len() as u32;
        *ids.entry(stripped).or_insert(next)
    };
    let a: Vec<u32> = old.iter().map(|line| id(line)).collect();
    let b: Vec<u32> = new.iter().map(|line| id(line)).collect();
    let steps = compare(&a, &b, left)?;

    let mut lines = Vec::with_capacity(hunk.lines.len());
    let (mut removed, mut added): (Vec<Line>, Vec<Line>) = (Vec::new(), Vec::new());
    let (mut at_old, mut at_new) = (0, 0);
    let mut same: usize = 0;
    for step in steps {
        match step {
            Step::Same => {
                lines.append(&mut removed);
                lines.append(&mut added);
                let (was, now) = (old[at_old], new[at_new]);
                lines.push(Line {
                    kind: Kind::Context,
                    old: was.old,
                    new: now.new,
                    text: now.text.clone(),
                });
                (at_old, at_new) = (at_old + 1, at_new + 1);
                same += 1;
            }
            Step::Removed => {
                let line = old[at_old];
                removed.push(Line {
                    kind: Kind::Removed,
                    new: None,
                    ..line.clone()
                });
                at_old += 1;
            }
            Step::Added => {
                let line = new[at_new];
                added.push(Line {
                    kind: Kind::Added,
                    old: None,
                    ..line.clone()
                });
                at_new += 1;
            }
        }
    }
    // A side's note follows its last line only where that line is in the last run, as the
    // desktop marks only the last run's rows (utils/iterateOverDiff.js:174-197 and 447-448).
    if removed.is_empty() && added.is_empty() {
        if let Some(end) = old_end.or(new_end).filter(|_| !lines.is_empty()) {
            lines.push(end.clone());
        }
    } else {
        let removes = !removed.is_empty();
        lines.append(&mut removed);
        lines.extend(old_end.filter(|_| removes).cloned());
        let adds = !added.is_empty();
        lines.append(&mut added);
        lines.extend(new_end.filter(|_| adds).cloned());
    }
    let context = hunk
        .lines
        .iter()
        .filter(|line| line.kind == Kind::Context)
        .count();
    Some((lines, same.saturating_sub(context) as u64))
}

/// Whether `c` is whitespace to JavaScript's `\s`: its white space and line terminators, which
/// take in U+FEFF and leave out U+0085, unlike `char::is_whitespace`.
fn js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// What happens to a line in going from one side to the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    Same,
    Removed,
    Added,
}

/// The end of a path through the edit graph: the last edit on it, where the path is after the
/// lines in common that follow the edit, and the path it extends.
#[derive(Clone, Copy)]
struct Node {
    /// The node before, or `ROOT` for the first.
    before: u32,
    /// The place in `old` after the path, plus one, with `REMOVES` set where the edit removes
    /// a line.
    at: u32,
}

const ROOT: u32 = u32::MAX;
const REMOVES: u32 = 1 << 31;

/// The steps from `old` to `new` that jsdiff's `diffLines` takes, one for each line of either:
/// Myers' diff with jsdiff's choice between paths of the same length, so the same lines pair.
/// None where it would take more steps than are `left`, which it takes them from.
pub(super) fn compare(old: &[u32], new: &[u32], left: &mut usize) -> Option<Vec<Step>> {
    let (n, m) = (old.len() as i64, new.len() as i64);
    if n + m >= i64::from(REMOVES) {
        return None;
    }
    // The furthest path on each diagonal, `old`'s place less `new`'s, from -m - 1 to n + 1: the
    // place in `old` it reached and its last node.
    let mut best: Vec<Option<(i64, u32)>> = vec![None; (n + m + 3) as usize];
    let slot = |diagonal: i64| usize::try_from(diagonal + m + 1).ok();
    let mut nodes: Vec<Node> = Vec::new();
    // Follows the lines in common from `at` in `old` on `diagonal`, as `extractCommon` does.
    let follow = |mut at: i64, diagonal: i64, left: &mut usize| {
        while at + 1 < n
            && at - diagonal + 1 < m
            && old[(at + 1) as usize] == new[(at - diagonal + 1) as usize]
        {
            *left = left.checked_sub(1)?;
            at += 1;
        }
        Some(at)
    };
    let at = follow(-1, 0, left)?;
    nodes.push(Node {
        before: ROOT,
        at: (at + 1) as u32,
    });
    if at + 1 >= n && at + 1 >= m {
        return Some(steps(&nodes, 0));
    }
    best[slot(0)?] = Some((at, 0));
    // Once a path reaches the end of one side, the diagonals past it can't end shorter.
    let (mut lowest, mut highest) = (i64::MIN, i64::MAX);
    for edits in 1..=n + m {
        let mut diagonal = lowest.max(-edits);
        while diagonal <= highest.min(edits) {
            *left = left.checked_sub(1)?;
            let remove = slot(diagonal - 1)
                .and_then(|at| best.get_mut(at))
                .and_then(Option::take);
            let add = slot(diagonal + 1)
                .and_then(|at| best.get(at))
                .copied()
                .flatten();
            let add = add.filter(|&(at, _)| (0..m).contains(&(at - diagonal)));
            let remove = remove.filter(|&(at, _)| at + 1 < n);
            // The path further along `old`, and the one that adds where both are as far.
            let (from, before, removes) = match (remove, add) {
                (None, None) => {
                    if let Some(path) = slot(diagonal).and_then(|at| best.get_mut(at)) {
                        *path = None;
                    }
                    diagonal += 2;
                    continue;
                }
                (Some((r, node)), Some((a, _))) if r >= a => (r + 1, node, true),
                (Some((r, node)), None) => (r + 1, node, true),
                (_, Some((a, node))) => (a, node, false),
            };
            let at = follow(from, diagonal, left)?;
            let node = nodes.len() as u32;
            nodes.push(Node {
                before,
                at: (at + 1) as u32 | if removes { REMOVES } else { 0 },
            });
            let at_new = at - diagonal;
            if at + 1 >= n && at_new + 1 >= m {
                return Some(steps(&nodes, node));
            }
            best[slot(diagonal)?] = Some((at, node));
            if at + 1 >= n {
                highest = highest.min(diagonal - 1);
            }
            if at_new + 1 >= m {
                lowest = lowest.max(diagonal + 1);
            }
            diagonal += 2;
        }
    }
    None
}

/// The steps of the path that ends at `last` among `nodes`, in order.
fn steps(nodes: &[Node], last: u32) -> Vec<Step> {
    let mut steps = Vec::new();
    let mut node = nodes[last as usize];
    loop {
        let at = i64::from(node.at & !REMOVES) - 1;
        if node.before == ROOT {
            steps.extend(std::iter::repeat_n(Step::Same, (at + 1) as usize));
            break;
        }
        let before = nodes[node.before as usize];
        let from = i64::from(before.at & !REMOVES) - 1;
        let removes = node.at & REMOVES != 0;
        let common = at - from - i64::from(removes);
        steps.extend(std::iter::repeat_n(Step::Same, common as usize));
        steps.push(if removes { Step::Removed } else { Step::Added });
        node = before;
    }
    steps.reverse();
    steps
}

#[cfg(test)]
mod tests;
