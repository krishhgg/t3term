//! The order the Code tab shows a slice's files in, as the desktop's `orderDiffFiles` reads
//! them (apps/web/src/components/pullRequest/pullRequestFileOrder.logic.ts at f570bd2): the
//! source first, each file after the changed files it imports, then the tests, each after the
//! source it covers, then the files a tool wrote. The desktop orders each slice on its own, so a
//! slice that comes later never moves the files above it.
//!
//! Imports are found as the desktop finds them, in every line of a file's hunks: a quoted
//! specifier after `from`, `import` or `require(`. A specifier names a changed file by its
//! path, with `./` and `../` resolved and a script's extension dropped, or by the file's name
//! where only one source file in the slice has it. Paths compare as the desktop's
//! `localeCompare` does in the root collation, as `collate` says.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, HashSet};

use crate::pull_request::patch::{Kind, Patch};

/// Lockfiles, which a tool writes.
const GENERATED_NAMES: [&str; 8] = [
    "pnpm-lock.yaml",
    "yarn.lock",
    "package-lock.json",
    "bun.lockb",
    "Cargo.lock",
    "go.sum",
    "composer.lock",
    "Gemfile.lock",
];
const GENERATED_DIRECTORIES: [&str; 5] =
    ["__snapshots__", "__generated__", "dist", "build", "vendor"];
const TEST_DIRECTORIES: [&str; 3] = ["__tests__", "tests", "test"];
/// The extensions a specifier may name that the path it means drops, as TypeScript's imports
/// name the `.js` beside a `.ts`.
const MODULE_EXTENSIONS: [&str; 6] = [".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"];

/// Which of the three readings a file belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tier {
    Source,
    Test,
    Generated,
}

/// The places of `patch`'s files in reading order, one for each path, the last file that has
/// it, as the desktop's map of the files by path keeps. `text` is the patch the lines are
/// ranges of.
pub(super) fn reading_order(text: &str, patch: &Patch) -> Vec<usize> {
    // Each path, where it first comes, with the place of the last file that has it.
    let mut places: HashMap<&str, usize> = HashMap::new();
    let mut paths: Vec<&str> = Vec::new();
    for (at, file) in patch.files.iter().enumerate() {
        if places.insert(file.path(), at).is_none() {
            paths.push(file.path());
        }
    }
    let tiers: HashMap<&str, Tier> = paths.iter().map(|&path| (path, tier(path))).collect();
    let tiers = &tiers;
    let of = |wanted: Tier| {
        paths
            .iter()
            .copied()
            .filter(move |path| tiers[path] == wanted)
    };

    let source: Vec<&str> = of(Tier::Source).collect();
    let mut modules: HashMap<&str, &str> = HashMap::new();
    let mut names: HashMap<&str, &str> = HashMap::new();
    let mut shared = HashSet::new();
    for &path in &source {
        modules.insert(strip_extension(path), path);
        let name = base_name(path);
        if names.insert(name, path).is_some() {
            shared.insert(name);
        }
    }
    // A name two files have could mean either, so it means neither.
    names.retain(|name, _| !shared.contains(name));
    let imports: HashMap<&str, HashSet<&str>> = source
        .iter()
        .map(|&path| {
            let file = &patch.files[places[path]];
            let lines = file
                .hunks
                .iter()
                .flat_map(|hunk| &hunk.lines)
                .filter(|line| line.kind != Kind::NoNewline)
                .map(|line| &text[line.text.clone()]);
            (path, imported(path, lines, &modules, &names))
        })
        .collect();
    let source = by_imports(source, &imports);

    // A test comes after the source file whose name it has, by where that file is.
    let mut positions: HashMap<&str, usize> = HashMap::new();
    for (at, &path) in source.iter().enumerate() {
        positions.entry(base_name(path)).or_insert(at);
    }
    let mut tests: Vec<&str> = of(Tier::Test).collect();
    tests.sort_by(|left, right| {
        let position = |path: &str| {
            positions
                .get(tested_name(path))
                .copied()
                .unwrap_or(usize::MAX)
        };
        position(left)
            .cmp(&position(right))
            .then_with(|| collate(left, right))
    });
    let mut generated: Vec<&str> = of(Tier::Generated).collect();
    generated.sort_by(|left, right| collate(left, right));

    source
        .into_iter()
        .chain(tests)
        .chain(generated)
        .map(|path| places[path])
        .collect()
}

/// Which reading `path` belongs to, from its name and the directories it is in.
fn tier(path: &str) -> Tier {
    let (directories, name) = match path.rsplit_once('/') {
        Some((directories, name)) => (Some(directories), name),
        None => (None, path),
    };
    let directories = directories.into_iter().flat_map(|path| path.split('/'));
    let within = |names: &[&str]| directories.clone().any(|segment| names.contains(&segment));
    if GENERATED_NAMES.contains(&name)
        || name.ends_with(".snap")
        || name.ends_with(".min.js")
        || name.ends_with(".min.css")
        || name.contains(".generated.")
        || within(&GENERATED_DIRECTORIES)
    {
        Tier::Generated
    } else if name.contains(".test.") || name.contains(".spec.") || within(&TEST_DIRECTORIES) {
        Tier::Test
    } else {
        Tier::Source
    }
}

/// `path` without the extension of its last segment.
fn strip_extension(path: &str) -> &str {
    match path.rfind('.') {
        Some(dot) if path.rfind('/').is_none_or(|slash| dot > slash) => &path[..dot],
        _ => path,
    }
}

/// The name of `path`'s last segment, without its extension.
fn base_name(path: &str) -> &str {
    strip_extension(path.rsplit('/').next().unwrap_or_default())
}

/// The name of the file a test covers: `foo.test.ts` and `__tests__/foo.ts` both cover `foo`.
fn tested_name(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or_default();
    let name = [".test.", ".spec."]
        .iter()
        .filter_map(|mark| name.find(mark))
        .min()
        .map_or(name, |at| &name[..at]);
    match name.rfind('.') {
        Some(dot) if dot + 1 < name.len() => &name[..dot],
        _ => name,
    }
}

/// `specifier` from the directory of `from`, its `.` and `..` segments resolved.
fn resolve_relative(from: &str, specifier: &str) -> String {
    let mut segments: Vec<&str> = from.split('/').collect();
    segments.pop();
    for part in specifier.split('/') {
        match part {
            "." | "" => {}
            ".." => {
                segments.pop();
            }
            _ => segments.push(part),
        }
    }
    segments.join("/")
}

/// The changed source files other than `path` that `lines` of it import, by path.
fn imported<'a, 'b>(
    path: &str,
    lines: impl Iterator<Item = &'b str>,
    modules: &HashMap<&str, &'a str>,
    names: &HashMap<&str, &'a str>,
) -> HashSet<&'a str> {
    let mut imported = HashSet::new();
    for line in lines {
        for specifier in specifiers(line) {
            let named = match specifier.starts_with('.') {
                true => resolve_relative(path, specifier),
                false => specifier.to_string(),
            };
            let resolved = MODULE_EXTENSIONS
                .iter()
                .find_map(|extension| named.strip_suffix(extension))
                .unwrap_or(&named);
            let target = modules
                .get(resolved)
                .or_else(|| modules.get(format!("{resolved}/index").as_str()))
                .or_else(|| names.get(base_name(resolved)));
            if let Some(&target) = target.filter(|&&target| target != path) {
                imported.insert(target);
            }
        }
    }
    imported
}

/// The specifiers `line` names, as the desktop's `IMPORT_SPECIFIER` finds them:
/// `(?:\bfrom|\bimport|\brequire\s*\()\s*\(?\s*["']([^"']+)["']`, each search starting where
/// the last match ended.
fn specifiers(line: &str) -> Vec<&str> {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    // JavaScript's `\s` also takes the byte order mark.
    let space = |c: char| c.is_whitespace() || c == '\u{feff}';
    let skip = |at: usize, wanted: &dyn Fn(char) -> bool| {
        line[at..]
            .find(|c: char| !wanted(c))
            .map_or(line.len(), |gap| at + gap)
    };
    let mut found = Vec::new();
    let mut at = 0;
    while at < line.len() {
        let boundary = !line[..at].chars().next_back().is_some_and(word);
        let keyword = ["from", "import", "require"]
            .into_iter()
            .find(|keyword| boundary && line[at..].starts_with(keyword));
        let matched = keyword.and_then(|keyword| {
            let mut next = at + keyword.len();
            if keyword == "require" {
                next = skip(next, &space);
                next += line[next..].strip_prefix('(').map(|_| 1)?;
            }
            next = skip(next, &space);
            if line[next..].starts_with('(') {
                next += 1;
            }
            next = skip(next, &space);
            let quote = |c: char| c == '"' || c == '\'';
            next += line[next..].strip_prefix(quote).map(|_| 1)?;
            let end = skip(next, &|c| !quote(c));
            (end > next && end < line.len()).then(|| (&line[next..end], end + 1))
        });
        match matched {
            Some((specifier, end)) => {
                found.push(specifier);
                at = end;
            }
            None => at += line[at..].chars().next().map_or(1, char::len_utf8),
        }
    }
    found
}

/// The source files in reading order: each after the files it imports, and of those ready, the
/// lowest path first, so files of one directory stay together. Where a cycle leaves none
/// ready, the lowest path left comes next, as the desktop's Kahn's order does.
fn by_imports<'a>(paths: Vec<&'a str>, imports: &HashMap<&str, HashSet<&'a str>>) -> Vec<&'a str> {
    let mut sorted = paths;
    sorted.sort_by(|left, right| collate(left, right));
    let rank: HashMap<&str, usize> = sorted.iter().enumerate().map(|(at, &p)| (p, at)).collect();
    let mut waiting: Vec<usize> = vec![0; sorted.len()];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); sorted.len()];
    for (at, path) in sorted.iter().enumerate() {
        for dependency in imports.get(path).into_iter().flatten() {
            if let Some(&dependency) = rank.get(dependency) {
                waiting[at] += 1;
                dependents[dependency].push(at);
            }
        }
    }
    let mut left: BTreeSet<usize> = (0..sorted.len()).collect();
    let mut ready: BTreeSet<usize> = left
        .iter()
        .copied()
        .filter(|&at| waiting[at] == 0)
        .collect();
    let mut ordered = Vec::with_capacity(sorted.len());
    while let Some(next) = ready.pop_first().or_else(|| left.first().copied()) {
        left.remove(&next);
        ordered.push(sorted[next]);
        for &dependent in &dependents[next] {
            if left.contains(&dependent) {
                waiting[dependent] -= 1;
                if waiting[dependent] == 0 {
                    ready.insert(dependent);
                }
            }
        }
    }
    ordered
}

/// Compares two paths as `localeCompare` does in the root collation, which the desktop's
/// sorts use: first by each character's place, with ASCII letters in either case alike and
/// punctuation before digits before letters, then lowercase before uppercase. Other
/// characters come after the letters, by code point, where the collation puts an accented
/// letter with its base letter. The bytes settle a tie.
fn collate(left: &str, right: &str) -> Ordering {
    let upper = |path: &str| {
        path.chars()
            .map(|c| c.is_ascii_uppercase())
            .collect::<Vec<_>>()
    };
    left.chars()
        .map(weight)
        .cmp(right.chars().map(weight))
        .then_with(|| upper(left).cmp(&upper(right)))
        .then_with(|| left.cmp(right))
}

/// A character's place in the root collation, ASCII letters in either case alike.
fn weight(c: char) -> u32 {
    const ORDER: &str =
        "\t\n\r _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$0123456789abcdefghijklmnopqrstuvwxyz";
    let c = c.to_ascii_lowercase();
    match ORDER.find(c) {
        Some(at) => at as u32,
        None => ORDER.len() as u32 + c as u32,
    }
}

#[cfg(test)]
mod tests;
