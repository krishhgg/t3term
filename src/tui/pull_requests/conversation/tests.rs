use serde_json::{Value, json};

use super::*;
use crate::tui::theme::Depth;

/// The time the tests read the conversation at.
const NOW: &str = "2026-10-14T12:00:00Z";

fn now() -> i64 {
    parse_iso_ms(NOW).unwrap()
}

fn theme() -> Theme {
    Theme::new(Depth::TrueColor)
}

/// Each line's text, the padding at its end dropped.
fn rows_of(lines: &[Line]) -> Vec<String> {
    lines
        .iter()
        .map(|line| line.to_string().trim_end().to_string())
        .collect()
}

fn actor(login: &str) -> Value {
    json!({"login": login, "name": null})
}

/// A remark by `author`, or by no one for null, at `at`.
fn remark(id: &str, author: Value, at: &str, state: Option<&str>, body: &str) -> Value {
    let kind = if state.is_some() {
        "review"
    } else {
        "issue-comment"
    };
    json!({"id": id, "kind": kind, "author": author, "body": body, "createdAt": at,
        "url": null, "path": null, "reviewState": state})
}

fn activity(comments: Vec<Value>, threads: Vec<Value>, commits: Vec<Value>) -> Value {
    let count = comments.len();
    json!({"comments": comments, "commentCount": count, "commentsTruncated": false,
        "reviewThreads": threads, "commits": commits})
}

fn conversation_of(value: &Value) -> Conversation {
    Conversation::of(&pull_request::Activity::read(value).unwrap())
}

/// The Comments section's heading row and the rows under it for `conversation` at `width`, as
/// it first shows.
fn read_section(conversation: Conversation, width: usize) -> (String, Vec<String>) {
    let mut answer = Answer::Read(Box::new(conversation));
    let (mut rows, _) = opened(&mut answer, &Shown::default(), width);
    let heading = rows.remove(0);
    (heading, rows)
}

/// The Comments section's rows for `answer` at `width`, opened as `shown` says, and the rows
/// of each control among them.
fn opened(
    answer: &mut Answer<Conversation>,
    shown: &Shown,
    width: usize,
) -> (Vec<String>, Vec<(Range<usize>, Control)>) {
    if let Answer::Read(conversation) = answer {
        conversation.lay_out(width, &theme(), shown);
    }
    let section = section(answer, shown, width, now(), &theme());
    (rows_of(&section.lines), section.targets)
}

/// `left` and `right` at the two ends of a row `width` columns wide.
fn ends(left: &str, right: &str, width: usize) -> String {
    let gap = width - left.chars().count() - right.chars().count();
    format!("{left}{}{right}", " ".repeat(gap))
}

/// `minutes` before `NOW`, as a host writes it. No test goes back past the day before.
fn ago_at(minutes: i64) -> String {
    let start = parse_iso_ms("2026-10-14T00:00:00Z").unwrap();
    let at = now() - minutes * 60_000;
    let (day, into) = match at >= start {
        true => (14, at - start),
        false => (13, at - start + 86_400_000),
    };
    let seconds = into / 1000;
    let (hours, minutes, seconds) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);
    format!("2026-10-{day}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

fn reviewer(login: &str, bot: bool) -> Reviewer {
    let actor = pull_request::Actor {
        login: login.to_string(),
        name: None,
        is_bot: bot,
    };
    Reviewer::of(&actor)
}

#[test]
fn verdicts_and_states_read_as_the_desktop_reads_them() {
    let cases = [
        ("APPROVED", Some(Outcome::Approved)),
        (" approved ", Some(Outcome::Approved)),
        ("CHANGES_REQUESTED", Some(Outcome::ChangesRequested)),
        ("changes_requested", Some(Outcome::ChangesRequested)),
        ("Changes-Requested", Some(Outcome::ChangesRequested)),
        ("DISMISSED", Some(Outcome::Dismissed)),
        ("COMMENTED", None),
        ("PENDING", None),
        ("", None),
    ];
    for (state, outcome) in cases {
        assert_eq!(Outcome::of(state), outcome, "{state:?}");
    }
    assert_eq!(state_label("CHANGES_REQUESTED"), "Changes requested");
    assert_eq!(state_label("COMMENTED"), "Commented");
    assert_eq!(state_label("éCLAIR"), "Éclair");
    assert_eq!(state_label(""), "");
}

#[test]
fn a_hosts_times_read_with_their_offsets() {
    let utc = parse_iso_ms("2026-07-05T00:30:00Z").unwrap();
    assert_eq!(instant("2026-07-05T00:30:00Z"), Some(utc));
    assert_eq!(instant("2026-07-05T00:30:00z"), Some(utc));
    assert_eq!(instant("2026-07-05T00:30:00"), Some(utc));
    assert_eq!(instant("2026-07-05T00:30:00.250Z"), Some(utc + 250));
    assert_eq!(instant("2026-07-05T00:30:00.250123+00:00"), Some(utc + 250));
    // An hour and a half before, though it sorts after as text.
    let east = instant("2026-07-05T01:00:00+02:00").unwrap();
    assert_eq!(east, utc - 90 * 60_000);
    assert_eq!(instant("2026-07-05T01:00:00+0200"), Some(east));
    assert_eq!(instant("2026-07-05T01:00:00+02"), Some(utc - 90 * 60_000));
    assert_eq!(
        instant("2026-07-04T19:00:00-05:30"),
        Some(utc),
        "west of UTC"
    );
    for broken in [
        "yesterday",
        "2026-07-05",
        "2026-07-05T00:30:00Zulu",
        "2026-07-05T00:30:00+2",
        "2026-07-05T00:30:00+02:0",
        "2026-07-05T00:30:00+0é:00",
        "2026-07-05T00:30:00 +02:00",
    ] {
        assert_eq!(instant(broken), None, "{broken}");
    }
}

#[test]
fn html_comments_go_except_in_fenced_code() {
    let cases = [
        ("Looks good.<!-- greptile_comment -->", "Looks good."),
        ("a<!-- one\ntwo -->b", "ab"),
        ("<!-- a --><!-- b -->left", "left"),
        (
            "```html\n<!-- kept -->\n```\n<!-- gone -->after",
            "```html\n<!-- kept -->\n```\nafter",
        ),
        // A fence closes only on its own mark.
        (
            "~~~\n```\n<!-- kept -->\n~~~\n<!--x-->y",
            "~~~\n```\n<!-- kept -->\n~~~\ny",
        ),
        // A fence closes only on one at least as long, with nothing after it.
        (
            "````md\n```\n<!-- kept -->\n```\n````\n<!--x-->y",
            "````md\n```\n<!-- kept -->\n```\n````\ny",
        ),
        (
            "```\n``` rust\n<!-- kept -->\n```  \n<!--x-->y",
            "```\n``` rust\n<!-- kept -->\n```  \ny",
        ),
        // Backticks with a backtick after them are inline code, not a fence.
        ("```a`b```<!--x-->y", "```a`b```y"),
        ("<!-- never closed\nmore", ""),
        ("no comment <!- here", "no comment <!- here"),
    ];
    for (text, kept) in cases {
        assert_eq!(without_html_comments(text), kept, "{text:?}");
    }
}

#[test]
fn remarks_split_into_the_recent_window_bots_and_finished_ones() {
    let mut comments: Vec<Value> = (0..14)
        .map(|n| {
            let at = format!("2026-10-14T0{}:{:02}:00Z", n / 10, n % 10);
            remark(
                &format!("c{n}"),
                actor("mona"),
                &at,
                None,
                &format!("Remark {n}"),
            )
        })
        .collect();
    let bot = json!({"login": "greptile-apps", "name": null, "isBot": true});
    comments.push(remark(
        "b1",
        bot.clone(),
        "2026-10-14T03:00:00Z",
        None,
        "Bot",
    ));
    comments.push(remark(
        "b2",
        actor("ci[bot]"),
        "2026-10-14T03:01:00Z",
        None,
        "Bot",
    ));
    // A bot's remark on a resolved thread is finished, and so is a dismissed review.
    comments.push(remark("t1", bot, "2026-10-14T03:02:00Z", None, "Fixed"));
    let dismissed = Some("DISMISSED");
    comments.push(remark(
        "d1",
        actor("lee"),
        "2026-10-14T03:03:00Z",
        dismissed,
        "",
    ));
    let thread = json!({"id": "th", "path": "src/a.rs", "line": 3, "side": "right",
        "isResolved": true, "isOutdated": false,
        "comments": [{"id": "t1", "author": null, "body": "", "createdAt": NOW, "url": null}]});
    let conversation = conversation_of(&activity(comments, vec![thread], vec![]));
    let ids = |remarks: &[Remark]| remarks.iter().map(|r| r.id.clone()).collect::<Vec<_>>();
    let active: Vec<String> = (0..14).map(|n| format!("c{n}")).collect();
    assert_eq!(ids(&conversation.active), active);
    assert_eq!(ids(&conversation.bots), ["b1", "b2"]);
    assert_eq!(ids(&conversation.finished), ["t1", "d1"]);

    let (heading, rows) = read_section(conversation, 60);
    assert_eq!(heading, ends("Comments (18)", "Newest first", 60));
    let shown: Vec<&str> = rows
        .iter()
        .filter(|row| row.starts_with("  Remark"))
        .map(|row| row.trim())
        .collect();
    let newest: Vec<String> = (4..14).rev().map(|n| format!("Remark {n}")).collect();
    assert_eq!(shown, newest);
    let tail = [
        "",
        "+ Show 4 older comments (4 hidden)",
        "",
        "› 2 bot comments",
        "2 authors · Latest 8h ago",
        "",
        "› 2 resolved or dismissed comments",
        "2 authors · Latest 8h ago",
    ];
    assert_eq!(rows[rows.len() - 8..], tail);

    // One of each is counted in the singular, and with no human remark the groups come
    // first.
    let mut on_file = remark("b1", actor("ci[bot]"), &ago_at(5), None, "Bot");
    on_file["path"] = json!("src/a.rs");
    let comments = vec![on_file, remark("d1", actor("lee"), "never", dismissed, "")];
    let (_, rows) = read_section(conversation_of(&activity(comments, vec![], vec![])), 60);
    let only = [
        "› 1 bot comment",
        "1 author · 1 file · Latest 5m ago",
        "",
        "› 1 resolved or dismissed comment",
        "1 author",
    ];
    assert_eq!(rows, only);
}

#[test]
fn a_remark_shows_its_author_time_verdict_place_and_text() {
    let five = ago_at(5);
    let review = |id, who, state, body| remark(id, actor(who), &five, Some(state), body);
    let mut on_line = remark(
        "rc",
        actor("octocat"),
        &ago_at(90),
        None,
        "Nit: *rename* it.",
    );
    on_line["kind"] = json!("review-comment");
    on_line["path"] = json!("src/old.rs");
    let mut on_file = remark("f", actor("hubot"), &ago_at(60 * 25), None, "On the file.");
    on_file["path"] = json!("README.md");
    let comments = vec![
        on_file,
        on_line,
        remark(
            "g",
            Value::Null,
            "not a time",
            None,
            "From a deleted account.",
        ),
        review("c", "lee", "COMMENTED", "<!-- only bookkeeping -->"),
        review("r", "mona", "CHANGES_REQUESTED", "Please split this."),
    ];
    let thread = json!({"id": "th", "path": "src/new.rs", "line": 42, "side": "right",
        "isResolved": false, "isOutdated": true,
        "comments": [{"id": "rc", "author": null, "body": "", "createdAt": NOW, "url": null}]});
    let (heading, rows) = read_section(
        conversation_of(&activity(comments, vec![thread], vec![])),
        40,
    );
    assert_eq!(heading, ends("Comments (5)", "Newest first", 40));
    let line = |left: &str, right: &str| ends(left, right, 40);
    let expected = [
        line("mona  Changes requested", "5m ago"),
        "  Please split this.".to_string(),
        String::new(),
        line("lee  Commented", "5m ago"),
        String::new(),
        "ghost".to_string(),
        "  From a deleted account.".to_string(),
        String::new(),
        line("octocat", "1h ago"),
        "src/new.rs:42 · Outdated".to_string(),
        "  Nit: rename it.".to_string(),
        String::new(),
        line("hubot", "1d ago"),
        "README.md".to_string(),
        "  On the file.".to_string(),
    ];
    assert_eq!(rows, expected);

    // Too narrow for the whole header: the author takes a row and the rest fit under it. The
    // order switch doesn't fit beside the heading either, so it takes the row under it.
    let split = review("r", "mona", "CHANGES_REQUESTED", "Please split this.");
    let (heading, rows) = read_section(conversation_of(&activity(vec![split], vec![], vec![])), 24);
    assert_eq!(heading, "Comments (1)");
    let expected = [
        "Newest first",
        "mona",
        "Changes requested 5m ago",
        "  Please split this.",
    ];
    assert_eq!(rows, expected);
}

#[test]
fn each_reviewer_shows_their_last_verdict() {
    let at = |hour: u32| format!("2026-10-14T{hour:02}:00:00Z");
    let review =
        |id: &str, author: Value, at: &str, state: &str| remark(id, author, at, Some(state), "");
    let comments = vec![
        // Sent out of order: the newest wins.
        review("o2", actor("octocat"), &at(10), "APPROVED"),
        review("o1", actor("octocat"), &at(9), "CHANGES_REQUESTED"),
        // Given before the newest commit.
        review("m1", actor("mona"), &at(8), "CHANGES_REQUESTED"),
        // Taken back, by the same login in another casing.
        review("h1", actor("hubot"), &at(10), "APPROVED"),
        review("h2", actor("Hubot"), &at(11), "DISMISSED"),
        // A time that doesn't parse can't stand against a later one.
        review("l1", actor("lee"), "never", "APPROVED"),
        review("l2", actor("lee"), &at(10), "CHANGES_REQUESTED"),
        // Two reviews with no author are two reviewers, and one asked of no one shows too.
        review("g1", Value::Null, &at(10), "APPROVED"),
        review("g2", Value::Null, &at(10), "APPROVED"),
        review("z1", actor("zed"), &at(10), "APPROVED"),
        review("z2", actor("Zed"), &at(11), "CHANGES_REQUESTED"),
        review("c1", actor("kim"), &at(10), "COMMENTED"),
    ];
    let commits = vec![json!({"oid": "1", "messageHeadline": "x", "committedDate": at(9)})];
    let conversation = conversation_of(&activity(comments, vec![], commits));
    let asked = [
        reviewer("Octocat", false),
        reviewer("mona", false),
        reviewer("hubot", false),
        reviewer("lee", false),
        reviewer("ci-bot", true),
    ];
    let rows = rows_of(&reviewer_lines(&asked, Some(&conversation), 48, &theme()));
    let line = |left: &str, right: &str| {
        let gap = " ".repeat(48 - left.chars().count() - right.chars().count());
        format!("{left}{gap}{right}")
    };
    let expected = [
        line("Octocat", "Approved"),
        line("mona", "Changes requested earlier changes"),
        "hubot".to_string(),
        line("lee", "Changes requested"),
        "ci-bot (bot)".to_string(),
        line("ghost", "Approved"),
        line("ghost", "Approved"),
        line("Zed", "Changes requested"),
    ];
    assert_eq!(rows, expected);

    // The conversation's reviewers take the detail's place, and with neither there are none.
    let mut value = activity(vec![], vec![], vec![]);
    value["reviewers"] = json!([actor("kim")]);
    let kim = conversation_of(&value);
    assert_eq!(
        rows_of(&reviewer_lines(&asked, Some(&kim), 48, &theme())),
        ["kim"]
    );
    let none = conversation_of(&activity(vec![], vec![], vec![]));
    assert_eq!(
        rows_of(&reviewer_lines(&[], Some(&none), 48, &theme())),
        ["None"]
    );
    assert_eq!(
        rows_of(&reviewer_lines(&asked[..1], None, 48, &theme())),
        ["Octocat"]
    );

    // A verdict too long for its row goes under the login.
    let comments = vec![review("m1", actor("mona"), &at(8), "CHANGES_REQUESTED")];
    let commits = vec![json!({"oid": "1", "messageHeadline": "x", "committedDate": at(9)})];
    let mona = conversation_of(&activity(comments, vec![], commits));
    let rows = rows_of(&reviewer_lines(&asked[1..2], Some(&mona), 20, &theme()));
    assert_eq!(rows, ["mona", "Changes requested", "earlier changes"]);
}

#[test]
fn the_comments_section_says_what_it_waits_for_and_what_it_left_out() {
    let (rows, targets) = opened(&mut Answer::Reading, &Shown::default(), 40);
    assert_eq!(rows, ["Comments", "Reading the conversation…"]);
    assert!(targets.is_empty());
    let mut failed = Answer::Failed("The host timed out.".to_string());
    let (rows, targets) = opened(&mut failed, &Shown::default(), 40);
    let expected = [
        "Comments",
        "Could not load pull request activity",
        "The host timed out.",
    ];
    assert_eq!(rows, expected);
    assert!(targets.is_empty());

    // With nothing to order, there is no switch.
    let mut empty = Answer::Read(Box::new(conversation_of(&activity(vec![], vec![], vec![]))));
    let (rows, targets) = opened(&mut empty, &Shown::default(), 40);
    assert_eq!(rows, ["Comments (0)", "No comments yet."]);
    assert!(targets.is_empty());

    let mut value = activity(
        vec![remark("c", actor("mona"), NOW, None, "Hi")],
        vec![],
        vec![],
    );
    value["commentCount"] = json!(1_234);
    value["commentsTruncated"] = json!(true);
    let (heading, rows) = read_section(conversation_of(&value), 200);
    assert_eq!(heading, ends("Comments (1,234)", "Newest first", 200));
    let note = "This conversation is longer than this page reads in one go. The most recent 1 \
                are here; open it on the host to read the rest.";
    assert_eq!(rows[0], note);

    // More reviewers than t3term keeps.
    let mut value = activity(vec![], vec![], vec![]);
    let many: Vec<Value> = (0..101).map(|n| actor(&format!("r{n}"))).collect();
    value["reviewers"] = json!(many);
    let (_, rows) = read_section(conversation_of(&value), 200);
    let cut = "T3 sent more comments, review threads, commits or reviewers than t3term keeps.";
    assert_eq!(rows.last().map(String::as_str), Some(cut));
}

#[test]
fn hostile_and_huge_remarks_are_cleaned_bounded_and_fit_every_width() {
    let author = json!({"login": format!("evil\u{1b}]52;c;x\u{7}{}", "w".repeat(5000)),
        "name": null});
    let mut hostile = remark(
        "h",
        author,
        NOW,
        Some("WEIRD\u{1b}[31m_STATE"),
        "# Big\u{1b}[2J\n\n界界界 text",
    );
    hostile["path"] = json!(format!("src/\u{9b}31m{}", "p".repeat(5000)));
    let value = activity(vec![hostile], vec![], vec![]);
    for width in [1, 2, 3, 10, 40, 80] {
        let (heading, mut rows) = read_section(conversation_of(&value), width);
        rows.insert(0, heading);
        for row in &rows {
            // The Markdown renderer keeps 8 columns for text, and the details' page cuts what
            // is wider than it shows.
            assert!(row.width() <= width.max(8), "{width}: {row:?}");
            assert!(
                !row.contains('\u{1b}') && !row.contains('\u{9b}'),
                "{row:?}"
            );
        }
        let joined = rows.concat();
        assert!(joined.contains("evil]52;c;x"), "{joined}");
        assert!(joined.contains("Weird[31m state") || width < 20, "{joined}");
    }
    let conversation = conversation_of(&value);
    let hostile = &conversation.active[0];
    assert_eq!(
        hostile.author.chars().count(),
        NAME + 1,
        "cut with an ellipsis"
    );
    assert!(hostile.location.as_ref().unwrap().chars().count() <= PATH + 1);

    // A remark T3 sent longer than t3term keeps says so.
    let mut long = remark("l", actor("mona"), NOW, None, "");
    long["body"] = json!("y ".repeat(pull_request::BODY));
    let (_, rows) = read_section(conversation_of(&activity(vec![long], vec![], vec![])), 200);
    let note = "Cut short: T3 sent a comment longer than the 262,144 bytes t3term keeps.";
    assert_eq!(rows.last().map(String::as_str), Some(note));
}

#[test]
fn each_remark_is_laid_out_once_for_each_width() {
    let comments = vec![remark(
        "c",
        actor("mona"),
        NOW,
        None,
        "Some **bold** words.",
    )];
    let mut conversation = conversation_of(&activity(comments, vec![], vec![]));
    let shown = Shown::default();
    conversation.lay_out(30, &theme(), &shown);
    let first = conversation.active[0].laid_out.as_ref().unwrap().1.as_ptr();
    conversation.lay_out(30, &theme(), &shown);
    assert_eq!(
        conversation.active[0].laid_out.as_ref().unwrap().1.as_ptr(),
        first
    );
    conversation.lay_out(31, &theme(), &shown);
    assert_eq!(conversation.active[0].laid_out.as_ref().unwrap().0, 31);
}

/// `count` remarks by `login`, the newest last, each `Remark n` on `path` where there is one.
fn by(login: &str, count: usize, path: Option<&str>) -> Vec<Value> {
    (0..count)
        .map(|n| {
            let mut remark = remark(
                &format!("{login}{n}"),
                actor(login),
                &ago_at(100 - n as i64),
                None,
                &format!("Remark {n}"),
            );
            remark["path"] = json!(path);
            remark
        })
        .collect()
}

/// The remarks among `rows`, by their text.
fn remarks_in(rows: &[String]) -> Vec<&str> {
    rows.iter()
        .filter(|row| row.starts_with("  Remark"))
        .map(|row| row.trim())
        .collect()
}

/// The rows of `control` among `targets`, which it has once.
fn rows_of_control(targets: &[(Range<usize>, Control)], control: Control) -> Range<usize> {
    let mut found = targets.iter().filter(|(_, c)| *c == control);
    let (rows, _) = found.next().expect("the control");
    assert!(found.next().is_none(), "{control:?} twice");
    rows.clone()
}

/// `Remark from` to `Remark to`, newest first unless `oldest_first`.
fn names(from: usize, to: usize, oldest_first: bool) -> Vec<String> {
    let mut names: Vec<String> = (from..=to).map(|n| format!("Remark {n}")).collect();
    if !oldest_first {
        names.reverse();
    }
    names
}

#[test]
fn show_older_and_the_order_switch_page_the_active_remarks_as_the_desktop_does() {
    let value = activity(by("mona", 25, None), vec![], vec![]);
    let conversation = conversation_of(&value);
    let at = |shown: &Shown| {
        let mut answer = Answer::Read(Box::new(conversation_of(&value)));
        opened(&mut answer, shown, 60)
    };
    let mut shown = Shown::default();
    assert_eq!(conversation.action(Control::Order, &shown), "oldest first");
    assert_eq!(conversation.action(Control::Older, &shown), "show 10 older");
    assert!(!conversation.has(Control::Recent, &shown));

    // The 10 most recent, newest first, with Show older after them. Only the switch and the
    // button do anything.
    let (rows, targets) = at(&shown);
    assert_eq!(remarks_in(&rows), names(15, 24, false));
    assert_eq!(rows[0], ends("Comments (25)", "Newest first", 60));
    assert_eq!(rows_of_control(&targets, Control::Order), 0..1);
    let older = rows_of_control(&targets, Control::Older);
    assert_eq!(
        rows[older.clone()],
        ["+ Show 10 older comments (15 hidden)"]
    );
    assert_eq!((older.end, targets.len()), (rows.len(), 2));

    // 10 more, then the last 5, then the 10 most recent again.
    shown.act(Control::Older, &conversation);
    assert_eq!(conversation.action(Control::Older, &shown), "show 5 older");
    let recent_action = conversation.action(Control::Recent, &shown);
    assert_eq!(recent_action, "show only 10 recent");
    let (rows, targets) = at(&shown);
    assert_eq!(remarks_in(&rows), names(5, 24, false));
    let older = rows_of_control(&targets, Control::Older);
    let recent = rows_of_control(&targets, Control::Recent);
    assert_eq!(rows[older.clone()], ["+ Show 5 older comments (5 hidden)"]);
    assert_eq!(rows[recent.clone()], ["Show only 10 recent comments"]);
    assert_eq!(recent.start, older.end + 1);
    shown.act(Control::Older, &conversation);
    assert!(!conversation.has(Control::Older, &shown));
    let (rows, _) = at(&shown);
    assert_eq!(remarks_in(&rows), names(0, 24, false));
    // Another press past the last changes nothing.
    let all = shown.clone();
    shown.act(Control::Older, &conversation);
    assert_eq!(shown, all);
    shown.act(Control::Recent, &conversation);
    assert_eq!(shown, Shown::default());

    // Oldest first, Show older comes before the remarks, which read from the oldest shown.
    shown.act(Control::Order, &conversation);
    assert_eq!(conversation.action(Control::Order, &shown), "newest first");
    let (rows, targets) = at(&shown);
    assert_eq!(rows[0], ends("Comments (25)", "Oldest first", 60));
    assert_eq!(rows_of_control(&targets, Control::Older), 1..2);
    assert_eq!(remarks_in(&rows), names(15, 24, true));
    shown.act(Control::Older, &conversation);
    let (rows, targets) = at(&shown);
    assert_eq!(remarks_in(&rows), names(5, 24, true));
    assert_eq!(rows_of_control(&targets, Control::Recent).end, rows.len());
}

#[test]
fn the_bots_group_opens_to_its_recent_remarks_and_shows_older_ones() {
    let mut comments = by("mona", 1, None);
    let mut bots = by("ci[bot]", 12, Some("src/a.rs"));
    bots[0]["path"] = json!("src/b.rs");
    bots[1]["path"] = json!("");
    bots[2]["author"] = json!({"login": "greptile-apps", "name": null, "isBot": true});
    comments.extend(bots);
    let value = activity(comments, vec![], vec![]);
    let mut conversation = conversation_of(&value);
    let at = |shown: &Shown| {
        let mut answer = Answer::Read(Box::new(conversation_of(&value)));
        opened(&mut answer, shown, 60)
    };
    let mut shown = Shown::default();
    assert_eq!(
        conversation.action(Control::Bots, &shown),
        "show bot comments"
    );

    // Closed, the group says who wrote what, on how many files, and when the latest came.
    // None of its remarks is laid out.
    let (rows, targets) = at(&shown);
    let group = rows_of_control(&targets, Control::Bots);
    let closed = ["› 12 bot comments", "2 authors · 2 files · Latest 1h ago"];
    assert_eq!(rows[group.clone()], closed);
    assert_eq!(group.end, rows.len());
    conversation.lay_out(60, &theme(), &shown);
    assert!(conversation.bots.iter().all(|r| r.laid_out.is_none()));

    // Open, its 10 most recent show under it with Show older after them.
    shown.act(Control::Bots, &conversation);
    assert_eq!(
        conversation.action(Control::Bots, &shown),
        "hide bot comments"
    );
    let (rows, targets) = at(&shown);
    assert_eq!(rows[group.start], "⌄ 12 bot comments");
    let mut bots = names(2, 11, false);
    bots.insert(0, "Remark 0".to_string());
    assert_eq!(remarks_in(&rows), bots);
    let older = rows_of_control(&targets, Control::OlderBots);
    assert_eq!(
        rows[older.clone()],
        ["+ Show 2 older bot comments (2 hidden)"]
    );
    assert_eq!(older.end, rows.len());
    assert_eq!(
        conversation.action(Control::OlderBots, &shown),
        "show 2 older"
    );
    conversation.lay_out(60, &theme(), &shown);
    let laid_out: Vec<bool> = conversation
        .bots
        .iter()
        .map(|r| r.laid_out.is_some())
        .collect();
    let mut expected = vec![false, false];
    expected.extend([true; 10]);
    assert_eq!(laid_out, expected);

    // Every one, then the 10 most recent again, and closed the buttons go.
    shown.act(Control::OlderBots, &conversation);
    let (rows, targets) = at(&shown);
    assert_eq!(remarks_in(&rows).len(), 13);
    let recent = rows_of_control(&targets, Control::RecentBots);
    assert_eq!(rows[recent], ["Show only 10 recent bot comments"]);
    assert!(!conversation.has(Control::OlderBots, &shown));
    shown.act(Control::RecentBots, &conversation);
    assert!(conversation.has(Control::OlderBots, &shown));
    shown.act(Control::Bots, &conversation);
    assert!(!conversation.has(Control::OlderBots, &shown));
    assert!(!conversation.has(Control::RecentBots, &shown));
    let (rows, _) = at(&shown);
    assert_eq!(rows[group], closed);
}

#[test]
fn finished_remarks_open_one_at_a_time_from_a_row_of_their_text() {
    let mut on_thread = remark(
        "r1",
        actor("octocat"),
        &ago_at(30),
        None,
        "> [!NOTE]\n> See **[the docs](https://example.com)** for `parse`.",
    );
    on_thread["kind"] = json!("review-comment");
    let dismissed = remark(
        "d1",
        actor("lee"),
        &ago_at(10),
        Some("DISMISSED"),
        "<!-- bot -->",
    );
    let thread = json!({"id": "th", "path": "src/a.rs", "line": 3, "side": "right",
        "isResolved": true, "isOutdated": false,
        "comments": [{"id": "r1", "author": null, "body": "", "createdAt": NOW, "url": null}]});
    let value = activity(vec![on_thread, dismissed], vec![thread], vec![]);
    let conversation = conversation_of(&value);
    let at = |shown: &Shown| {
        let mut answer = Answer::Read(Box::new(conversation_of(&value)));
        opened(&mut answer, shown, 40)
    };
    let mut shown = Shown::default();
    let (rows, targets) = at(&shown);
    let group = rows_of_control(&targets, Control::Finished);
    let closed = [
        "› 2 resolved or dismissed comments",
        "2 authors · Latest 10m ago",
    ];
    assert_eq!(rows[group.clone()], closed);
    assert!(!conversation.has(Control::Remark(0), &shown));
    assert_eq!(
        conversation.action(Control::Finished, &shown),
        "show resolved"
    );

    // Open, each shows why it is finished, and its text on one row. Each of its rows opens it.
    shown.act(Control::Finished, &conversation);
    assert_eq!(
        conversation.action(Control::Finished, &shown),
        "hide resolved"
    );
    let (rows, targets) = at(&shown);
    let expected = [
        "⌄ 2 resolved or dismissed comments",
        "2 authors · Latest 10m ago",
        "",
        &ends("lee  › Review dismissed", "10m ago", 40),
        "",
        &ends("octocat  › Resolved", "30m ago", 40),
        "src/a.rs:3",
        "See the docs for parse.",
    ];
    assert_eq!(rows[group.start..], expected);
    let dismissed = rows_of_control(&targets, Control::Remark(1));
    let resolved = rows_of_control(&targets, Control::Remark(0));
    assert_eq!(dismissed, group.start + 3..group.start + 4);
    assert_eq!(resolved, group.start + 5..group.start + 8);
    assert_eq!(
        conversation.action(Control::Remark(0), &shown),
        "show comment"
    );

    // Open, its whole text shows and only its header closes it again.
    shown.act(Control::Remark(0), &conversation);
    assert_eq!(
        conversation.action(Control::Remark(0), &shown),
        "hide comment"
    );
    let (rows, targets) = at(&shown);
    let resolved = rows_of_control(&targets, Control::Remark(0));
    assert_eq!(resolved.len(), 1);
    assert_eq!(
        rows[resolved.start],
        ends("octocat  ⌄ Resolved", "30m ago", 40)
    );
    assert_eq!(rows[resolved.start + 1], "src/a.rs:3");
    // The Markdown renderer shows the alert as a quote and the link with its address.
    let text = rows[resolved.start + 2..].join(" ");
    assert!(text.contains("▎ [!NOTE]"), "{text}");
    assert!(
        text.contains("See the docs (https://example.com)"),
        "{text}"
    );
    shown.act(Control::Remark(0), &conversation);
    assert_eq!(
        at(&shown).0.last().map(String::as_str),
        Some("See the docs for parse.")
    );
    // Oldest first, the resolved one comes first.
    shown.act(Control::Order, &conversation);
    let (rows, _) = at(&shown);
    assert_eq!(
        rows[group.start + 3],
        ends("octocat  › Resolved", "30m ago", 40)
    );
}

#[test]
fn a_finished_remarks_row_reads_as_the_desktops_does() {
    let cases = [
        ("<!-- bot -->Looks **good**", "Looks good"),
        ("> [!WARNING]\n> Careful here.", "Careful here."),
        ("![logo](x.png) and [docs](https://a/b)", "logo and docs"),
        (
            "# Title\n\n- one\n* two\n\n```rust\nlet x = 1;\n```",
            "Title one two rust let x = 1;",
        ),
        // A link's text may span rows, and empty or unfinished ones stay.
        ("[a\nb](c) [](x) [y] [z](", "a b [](x) [y] [z]("),
        ("[a [b](c)", "a [b"),
        ("```\n<!-- even in code -->\n```after", "after"),
        ("<!-- never closed", "<!-- never closed"),
        ("[!NOTE]", ""),
        ("> [!NOTÉ]", "[!NOTÉ]"),
        ("  >> ## -* Deep", "Deep"),
    ];
    for (text, row) in cases {
        assert_eq!(preview(text), row, "{text:?}");
    }
    // Each step takes time in proportion to the text, however hostile.
    let huge = pull_request::BODY;
    let brackets = format!("{}]x", "[".repeat(huge));
    assert_eq!(preview(&brackets), brackets);
    let opens = "[a](".repeat(huge / 4);
    assert_eq!(preview(&opens), opens);
    let comments = "<!--".repeat(huge / 4);
    assert_eq!(preview(&comments), comments);
}

/// When a pull request opened `minutes` before `NOW`, who opened it, and when it merged or
/// closed, as its detail says.
fn lifecycle(opened: &str, author: Option<&str>, merged: Value, closed: Value) -> Lifecycle {
    let detail = json!({
        "provider": "github",
        "capabilities": {"diff": true, "comment": true, "actions": [], "mergeMethods": [],
            "search": true, "review": {"inlineComment": false, "reply": false,
            "resolve": false, "verdicts": []},
            "reviewers": {"request": false, "listCandidates": false}},
        "viewerPermissions": {"actions": [], "comment": false, "resolve": false,
            "verdicts": [], "requestReviewers": false},
        "projectId": "p", "projectTitle": "Demo", "workspaceRoot": "/repo",
        "repository": "krishhgg/t3term", "number": 31, "title": "Change 31", "body": " ",
        "url": "https://github.com/krishhgg/t3term/pull/31",
        "author": author.map(actor),
        "state": "open", "isDraft": false, "mergeability": "unknown", "additions": 0,
        "deletions": 0, "changedFiles": 0, "headBranch": "feature", "baseBranch": "master",
        "createdAt": opened, "updatedAt": opened,
        "mergedAt": merged, "closedAt": closed, "reviewers": [], "labels": [], "checks": [],
        "mergeCapabilities": {"merge": false, "squash": false, "rebase": false}
    });
    Lifecycle::of(&pull_request::Detail::read(&detail).unwrap())
}

/// A commit, with the lines it adds and removes and its first author where given.
fn commit(
    oid: &str,
    headline: &str,
    at: &str,
    counts: Option<(u64, u64)>,
    by: Option<&str>,
) -> Value {
    let mut commit = json!({"oid": oid, "messageHeadline": headline, "committedDate": at});
    if let Some((additions, deletions)) = counts {
        commit["additions"] = json!(additions);
        commit["deletions"] = json!(deletions);
    }
    if let Some(by) = by {
        commit["authors"] = json!([actor(by)]);
    }
    commit
}

/// The Timeline's rows for `answer` at `width`, opened as `shown` says, and the rows of each
/// control among them.
fn timeline_rows(
    answer: &mut Answer<Conversation>,
    shown: &Shown,
    width: usize,
) -> (Vec<String>, Vec<(Range<usize>, Control)>) {
    if let Answer::Read(conversation) = answer {
        conversation.lay_out_timeline(width, &theme(), shown);
    }
    let section = timeline_section(answer, shown, width, now(), &theme());
    (rows_of(&section.lines), section.targets)
}

/// A merged pull request with two commits, three verdicts and four other remarks.
fn merged() -> Answer<Conversation> {
    merged_with(vec![], vec![])
}

/// `merged()`, read again once `comments` and `commits` came as well.
fn merged_with(more_comments: Vec<Value>, more_commits: Vec<Value>) -> Answer<Conversation> {
    let review =
        |id, who, minutes, state, body| remark(id, actor(who), &ago_at(minutes), Some(state), body);
    let comment = |id, who, minutes, body| remark(id, actor(who), &ago_at(minutes), None, body);
    let mut nit = comment("k2", "Lee", 250, "Nit.");
    nit["kind"] = json!("review-comment");
    nit["path"] = json!("src/a.rs");
    let mut comments = vec![
        review(
            "r1",
            "octocat",
            400,
            "CHANGES_REQUESTED",
            "Please split this.",
        ),
        comment("k1", "hubot", 300, "Looks close."),
        nit,
        review("k3", "lee", 200, "COMMENTED", "One more."),
        review("a1", "octocat", 60, "APPROVED", ""),
        comment("k4", "hubot", 30, "Ship it."),
    ];
    comments.extend(more_comments);
    let mut commits = vec![
        commit(
            "a1b2c3d4e5f6",
            "Add the parser",
            &ago_at(500),
            Some((1200, 4)),
            Some("mona"),
        ),
        commit("0f0f0f0f0f0f", "", &ago_at(120), Some((0, 0)), None),
    ];
    commits.extend(more_commits);
    let mut conversation = conversation_of(&activity(comments, vec![], commits));
    let ended = json!(ago_at(5));
    conversation.follow(&lifecycle(&ago_at(600), Some("mona"), ended.clone(), ended));
    Answer::Read(Box::new(conversation))
}

#[test]
fn the_timeline_reads_as_the_desktops_does() {
    let mut answer = merged();
    let mut shown = Shown::default();
    let (rows, targets) = timeline_rows(&mut answer, &shown, 60);
    let expected = [
        ends("6 comments · 2 commits · 1 approval", "Newest first", 60),
        String::new(),
        "● Pull request merged".to_string(),
        "  5m ago".to_string(),
        String::new(),
        "› 1 comment".to_string(),
        "  1 author · 30m ago".to_string(),
        String::new(),
        "✓ octocat approved".to_string(),
        "  1h ago".to_string(),
        String::new(),
        "○ Untitled commit".to_string(),
        "  0f0f0f0 · 2h ago".to_string(),
        String::new(),
        // `Lee` and `lee` are one author.
        "› 3 comments".to_string(),
        "  2 authors · 3h ago".to_string(),
        String::new(),
        // Given before the newest commit, so grey and saying so.
        "✗ octocat changes requested, before the latest commits".to_string(),
        "  6h ago".to_string(),
        "  Please split this.".to_string(),
        String::new(),
        ends("○ Add the parser", "+1,200 -4", 60),
        "  a1b2c3d · mona · 8h ago".to_string(),
        String::new(),
        "● mona Pull request opened".to_string(),
        "  10h ago".to_string(),
    ];
    assert_eq!(rows, expected);
    let controls = [
        (0..1, Control::TimelineOrder),
        (5..7, Control::Run(0)),
        (14..16, Control::Run(1)),
    ];
    assert_eq!(targets, controls);
    let Answer::Read(conversation) = &answer else {
        unreachable!()
    };
    assert_eq!(
        conversation.action(Control::TimelineOrder, &shown),
        "oldest first"
    );
    assert_eq!(
        conversation.action(Control::Run(1), &shown),
        "show comments"
    );
    assert!(!conversation.has(Control::Run(2), &shown));

    // A run opens to each of its remarks, and stays open the other way round, where the
    // desktop's closes. Oldest first, a run's time is its oldest remark's.
    shown.act(Control::Run(1), conversation);
    shown.act(Control::TimelineOrder, conversation);
    assert_eq!(
        conversation.action(Control::TimelineOrder, &shown),
        "newest first"
    );
    assert_eq!(
        conversation.action(Control::Run(1), &shown),
        "hide comments"
    );
    let (rows, targets) = timeline_rows(&mut answer, &shown, 60);
    let expected = [
        ends("6 comments · 2 commits · 1 approval", "Oldest first", 60),
        String::new(),
        "● mona Pull request opened".to_string(),
        "  10h ago".to_string(),
        String::new(),
        ends("○ Add the parser", "+1,200 -4", 60),
        "  a1b2c3d · mona · 8h ago".to_string(),
        String::new(),
        "✗ octocat changes requested, before the latest commits".to_string(),
        "  6h ago".to_string(),
        "  Please split this.".to_string(),
        String::new(),
        "⌄ 3 comments".to_string(),
        "  2 authors · 5h ago".to_string(),
        String::new(),
        "  hubot commented".to_string(),
        "  5h ago".to_string(),
        "  Looks close.".to_string(),
        String::new(),
        "  Lee commented".to_string(),
        "  4h ago · src/a.rs".to_string(),
        "  Nit.".to_string(),
        String::new(),
        "  lee reviewed  Commented".to_string(),
        "  3h ago".to_string(),
        "  One more.".to_string(),
        String::new(),
        "○ Untitled commit".to_string(),
        "  0f0f0f0 · 2h ago".to_string(),
        String::new(),
        "✓ octocat approved".to_string(),
        "  1h ago".to_string(),
        String::new(),
        "› 1 comment".to_string(),
        "  1 author · 30m ago".to_string(),
        String::new(),
        "● Pull request merged".to_string(),
        "  5m ago".to_string(),
    ];
    assert_eq!(rows, expected);
    let controls = [
        (0..1, Control::TimelineOrder),
        (12..14, Control::Run(1)),
        (33..35, Control::Run(0)),
    ];
    assert_eq!(targets, controls);
}

#[test]
fn an_open_run_stays_open_when_the_timeline_is_read_again_until_it_grows() {
    let mut shown = Shown::default();
    let answer = merged();
    let Answer::Read(conversation) = &answer else {
        unreachable!()
    };
    shown.act(Control::Run(0), conversation);
    shown.act(Control::Run(1), conversation);
    let open = |answer: &Answer<Conversation>, shown: &Shown| {
        let Answer::Read(conversation) = answer else {
            unreachable!()
        };
        let runs = conversation.timeline.as_ref().map_or(0, |t| t.runs());
        (0..runs)
            .map(|run| conversation.action(Control::Run(run), shown))
            .collect::<Vec<_>>()
    };
    assert_eq!(open(&answer, &shown), ["hide comments", "hide comments"]);

    // Read again with a commit and a remark since, the runs move down one and stay open, and
    // the new one is closed, as the desktop keys each run by its first remark newest first.
    let comment = remark("k5", actor("hubot"), &ago_at(10), None, "Merged.");
    let since = commit("1e1e1e1e1e1e", "Bump", &ago_at(20), None, None);
    let mut again = merged_with(vec![comment.clone()], vec![since]);
    assert_eq!(
        open(&again, &shown),
        ["show comments", "hide comments", "hide comments"]
    );
    let (rows, _) = timeline_rows(&mut again, &shown, 60);
    assert!(rows.contains(&"  Ship it.".to_string()), "{rows:#?}");

    // A remark that joins the newest run makes it another run, which is closed.
    let again = merged_with(vec![comment], vec![]);
    assert_eq!(open(&again, &shown), ["show comments", "hide comments"]);
}

#[test]
fn the_timeline_fits_every_width() {
    let mut answer = merged();
    let (rows, targets) = timeline_rows(&mut answer, &Shown::default(), 30);
    // The counts and the switch don't fit on one row. A verdict too wide for its row puts each
    // piece on rows of its own, and a commit's counts still fit beside its headline.
    let expected = [
        "6 comments · 2 commits",
        "1 approval",
        "Newest first",
        "",
        "● Pull request merged",
        "  5m ago",
    ];
    assert_eq!(rows[..6], expected);
    assert_eq!(targets[0], (2..3, Control::TimelineOrder));
    let verdict = rows.iter().position(|row| row == "✗ octocat").unwrap();
    let expected = [
        "✗ octocat",
        "  changes requested, before",
        "  the latest commits",
        "  6h ago",
        "  Please split this.",
        "",
        "○ Add the parser     +1,200 -4",
    ];
    assert_eq!(rows[verdict..verdict + 7], expected);

    let mut shown = Shown::default();
    if let Answer::Read(conversation) = &answer {
        shown.act(Control::Run(0), conversation);
        shown.act(Control::Run(1), conversation);
    }
    for width in 0..=70 {
        let (rows, targets) = timeline_rows(&mut answer, &shown, width);
        for row in &rows {
            // The Markdown renderer keeps 8 columns for text, and the details' page cuts what
            // is wider than it shows.
            assert!(row.width() <= width.max(8), "{width}: {row:?}");
            // A row of parts wraps between them, so none ends on a dot or splits `6h ago`.
            assert!(!row.ends_with('·'), "{width}: {row:?}");
            assert!(width < 12 || row.trim() != "ago", "{width}: {rows:?}");
        }
        assert!(targets.iter().all(|(rows_of, _)| rows_of.end <= rows.len()));
        assert_eq!(targets.len(), if width == 0 { 0 } else { 3 }, "{width}");
    }

    // A commit's text is cleaned and bounded once.
    let evil = format!("\u{1b}]52;c;x\u{7}{}", "w".repeat(5000));
    let hostile = json!({"oid": "\u{9b}2Jabcdef0123", "messageHeadline": evil,
        "committedDate": ago_at(5), "authors": [{"login": evil, "name": null}]});
    let mut conversation = conversation_of(&activity(vec![], vec![], vec![hostile]));
    conversation.follow(&lifecycle(&ago_at(60), None, Value::Null, Value::Null));
    let mut answer = Answer::Read(Box::new(conversation));
    for width in [1, 2, 3, 10, 40, 80] {
        let (rows, _) = timeline_rows(&mut answer, &Shown::default(), width);
        for row in &rows {
            assert!(row.width() <= width, "{width}: {row:?}");
            assert!(
                !row.contains('\u{1b}') && !row.contains('\u{9b}'),
                "{row:?}"
            );
        }
    }
    let (rows, _) = timeline_rows(&mut answer, &Shown::default(), 80);
    assert!(rows[2].starts_with("○ ]52;c;xwww"), "{}", rows[2]);
    assert_eq!(rows[3], "  2Jabcde", "the id cleaned, then cut");
    // The author too wide for the row beside the id starts a row of its own.
    assert!(rows[4].starts_with("  ]52;c;xwww"), "{}", rows[4]);
    // Two columns are too few to spare for the marks.
    let (rows, _) = timeline_rows(&mut answer, &Shown::default(), 2);
    assert!(rows.windows(2).any(|pair| pair == ["Pu", "ll"]), "{rows:?}");
    assert!(
        rows.iter().all(|row| !row.contains(['●', '○', '›'])),
        "{rows:?}"
    );
}

#[test]
fn the_timeline_reads_how_the_pull_request_opened_and_ended() {
    let none = Value::Null;
    let at = |minutes| json!(ago_at(minutes));
    let ends = |lifecycle: &Lifecycle, author: Option<&str>| {
        let mut value = activity(vec![], vec![], vec![]);
        if let Some(author) = author {
            value["author"] = actor(author);
        }
        let mut conversation = conversation_of(&value);
        conversation.follow(lifecycle);
        let mut answer = Answer::Read(Box::new(conversation));
        let (rows, _) = timeline_rows(&mut answer, &Shown::default(), 40);
        rows[2..]
            .iter()
            .filter(|row| !row.is_empty())
            .cloned()
            .collect::<Vec<_>>()
    };
    // Merged wins over closed, as GitHub sets both on a merge.
    let both = lifecycle(&ago_at(90), None, at(10), at(10));
    let rows = [
        "● Pull request merged",
        "  10m ago",
        "● Pull request opened",
        "  1h ago",
    ];
    assert_eq!(ends(&both, None), rows);
    let closed = lifecycle(&ago_at(90), Some("mona"), none.clone(), at(10));
    let rows = [
        "● Pull request closed",
        "  10m ago",
        "● mona Pull request opened",
        "  1h ago",
    ];
    assert_eq!(ends(&closed, None), rows);
    // The conversation's author stands in for the detail's.
    let rows = [
        "● Pull request closed",
        "  10m ago",
        "● hubot Pull request opened",
        "  1h ago",
    ];
    assert_eq!(ends(&closed, Some("hubot")), rows);
    // An empty time is no event, and a time that doesn't parse is one at no time, which comes
    // last newest first.
    let open = lifecycle(&ago_at(90), None, json!(""), json!(""));
    assert_eq!(ends(&open, None), ["● Pull request opened", "  1h ago"]);
    let odd = lifecycle("not a time", None, none, at(10));
    let rows = [
        "● Pull request closed",
        "  10m ago",
        "● Pull request opened",
    ];
    assert_eq!(ends(&odd, None), rows);
}

#[test]
fn the_timeline_says_what_it_waits_for_and_what_it_left_out() {
    let shown = Shown::default();
    let (rows, targets) = timeline_rows(&mut Answer::Reading, &shown, 40);
    assert_eq!(rows, ["Reading the conversation…"]);
    assert!(targets.is_empty());
    let mut failed = Answer::Failed("The host timed out.".to_string());
    let (rows, _) = timeline_rows(&mut failed, &shown, 40);
    assert_eq!(
        rows,
        [
            "Could not load pull request activity",
            "The host timed out."
        ]
    );
    // Until the detail came there is no Timeline.
    let empty = conversation_of(&activity(vec![], vec![], vec![]));
    let mut empty = Answer::Read(Box::new(empty));
    assert!(timeline_rows(&mut empty, &shown, 40).0.is_empty());

    // An approval older than what T3 read would be missing, so none are counted.
    let approved = remark("a", actor("octocat"), &ago_at(5), Some("APPROVED"), "");
    let mut value = activity(vec![approved], vec![], vec![]);
    value["commentsTruncated"] = json!(true);
    value["commentCount"] = json!(1_234);
    // More reviewers than t3term keeps.
    let many: Vec<Value> = (0..101).map(|n| actor(&format!("r{n}"))).collect();
    value["reviewers"] = json!(many);
    let mut conversation = conversation_of(&value);
    conversation.follow(&lifecycle(&ago_at(60), None, Value::Null, Value::Null));
    let (rows, _) = timeline_rows(&mut Answer::Read(Box::new(conversation)), &shown, 60);
    assert_eq!(
        rows[0],
        ends("1,234 comments · 0 commits", "Newest first", 60)
    );
    let cut = "T3 sent more comments, review threads, commits or reviewers than t3term keeps.";
    assert_eq!(rows[rows.len() - 2..].join(" "), cut);
}
