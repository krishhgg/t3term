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

/// The Comments section's label and rows for `conversation` at `width`.
fn section(conversation: Conversation, width: usize) -> (String, Vec<String>) {
    let mut conversation = conversation;
    conversation.lay_out(width, &theme());
    let (label, lines) = lines(
        &Answer::Read(Box::new(conversation)),
        width,
        now(),
        &theme(),
    );
    (label, rows_of(&lines))
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
    let shown: Vec<&str> = conversation
        .recent
        .iter()
        .map(|r| r.body.as_deref().unwrap())
        .collect();
    let newest: Vec<String> = (4..14).rev().map(|n| format!("Remark {n}")).collect();
    assert_eq!(shown, newest);
    assert_eq!(
        (conversation.older, conversation.bots, conversation.finished),
        (4, 2, 2)
    );

    let (label, rows) = section(conversation, 60);
    assert_eq!(label, "Comments (18)");
    let tail = [
        "",
        "4 older comments not shown.",
        "2 bot comments not shown.",
        "2 resolved or dismissed comments not shown.",
    ];
    assert_eq!(rows[rows.len() - 4..], tail);

    // One of each is counted in the singular, and with no human remark the counts come
    // first.
    let comments = vec![
        remark("b1", actor("ci[bot]"), NOW, None, "Bot"),
        remark("d1", actor("lee"), NOW, dismissed, ""),
    ];
    let (_, rows) = section(conversation_of(&activity(comments, vec![], vec![])), 60);
    let only = [
        "1 bot comment not shown.",
        "1 resolved or dismissed comment not shown.",
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
    let (label, rows) = section(
        conversation_of(&activity(comments, vec![thread], vec![])),
        40,
    );
    assert_eq!(label, "Comments (5)");
    let line = |left: &str, right: &str| {
        let gap = " ".repeat(40 - left.chars().count() - right.chars().count());
        format!("{left}{gap}{right}")
    };
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

    // Too narrow for the whole header: the author takes a row and the rest fit under it.
    let split = review("r", "mona", "CHANGES_REQUESTED", "Please split this.");
    let (_, rows) = section(conversation_of(&activity(vec![split], vec![], vec![])), 24);
    assert_eq!(
        rows,
        ["mona", "Changes requested 5m ago", "  Please split this."]
    );
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
    let theme = theme();
    let (label, rows) = lines(&Answer::Reading, 40, now(), &theme);
    assert_eq!(label, "Comments");
    assert_eq!(rows_of(&rows), ["Reading the conversation…"]);
    let failed = Answer::Failed("The host timed out.".to_string());
    let (label, rows) = lines(&failed, 40, now(), &theme);
    assert_eq!(label, "Comments");
    let expected = [
        "Could not load pull request activity",
        "The host timed out.",
    ];
    assert_eq!(rows_of(&rows), expected);

    let (label, rows) = section(conversation_of(&activity(vec![], vec![], vec![])), 40);
    assert_eq!(label, "Comments (0)");
    assert_eq!(rows, ["No comments yet."]);

    let mut value = activity(
        vec![remark("c", actor("mona"), NOW, None, "Hi")],
        vec![],
        vec![],
    );
    value["commentCount"] = json!(1_234);
    value["commentsTruncated"] = json!(true);
    let (label, rows) = section(conversation_of(&value), 200);
    assert_eq!(label, "Comments (1,234)");
    let note = "This conversation is longer than this page reads in one go. The most recent 1 \
                are here; open it on the host to read the rest.";
    assert_eq!(rows[0], note);

    // More reviewers than t3term keeps.
    let mut value = activity(vec![], vec![], vec![]);
    let many: Vec<Value> = (0..101).map(|n| actor(&format!("r{n}"))).collect();
    value["reviewers"] = json!(many);
    let (_, rows) = section(conversation_of(&value), 200);
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
        let (_, rows) = section(conversation_of(&value), width);
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
    let hostile = &conversation.recent[0];
    assert_eq!(
        hostile.author.chars().count(),
        NAME + 1,
        "cut with an ellipsis"
    );
    assert!(hostile.location.as_ref().unwrap().chars().count() <= PATH + 1);

    // A remark T3 sent longer than t3term keeps says so.
    let mut long = remark("l", actor("mona"), NOW, None, "");
    long["body"] = json!("y ".repeat(pull_request::BODY));
    let (_, rows) = section(conversation_of(&activity(vec![long], vec![], vec![])), 200);
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
    conversation.lay_out(30, &theme());
    let first = conversation.recent[0].laid_out.as_ref().unwrap().1.as_ptr();
    conversation.lay_out(30, &theme());
    assert_eq!(
        conversation.recent[0].laid_out.as_ref().unwrap().1.as_ptr(),
        first
    );
    conversation.lay_out(31, &theme());
    assert_eq!(conversation.recent[0].laid_out.as_ref().unwrap().0, 31);
}
