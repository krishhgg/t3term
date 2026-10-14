//! The terminal drawer in the TUI: where it draws, which keys and clicks reach it, and the shell
//! Start terminal asks T3 for. tui/terminal/tests.rs tests the drawer's own work against a fake
//! T3, and the last tests here drive it from the TUI the same way.

use super::*;
use crate::tui::terminal::tests::{
    ATTACH, Fake, Socket, ack, chunk, expect_request, hostile_titles, interrupt, kind,
    lookup_error, recv, refusal, send, session, wire_label, within,
};

/// The TUI with `thread` listed and open, in the projects `details_shell` lists.
fn opened(thread: Value) -> App {
    let mut app = tui(&Settings::default());
    app.on_shell_event(details_shell(vec![thread.clone()]));
    app.open_thread(open_bound(thread));
    app
}

/// Shows the drawer with F4 and draws a frame `width` columns by `height` rows.
fn drawer_at(app: &mut App, width: u16, height: u16) -> Vec<String> {
    press(app, KeyCode::F(4));
    screen(app, width, height)
}

fn pane(app: &App) -> &terminal::Pane {
    app.terminal.as_ref().expect("the drawer shows")
}

#[test]
fn f4_shows_the_drawer_under_the_composer_and_hides_it_with_the_draft_kept() {
    let mut app = details_tui();
    screen(&mut app, 80, 24);
    let (transcript, composer) = (app.transcript_area, app.composer_area);
    assert_eq!(composer.bottom(), 23);
    press(&mut app, KeyCode::F(4));
    assert_eq!(app.focus, Focus::Terminal);
    assert!(app.layout_changed, "the next frame waits for nothing");
    let rows = screen(&mut app, 80, 24);
    // Seven rows, its header and six of screen, between the composer and the status line and
    // as wide as the composer. The composer keeps its size, and the transcript gives up the
    // rows.
    let drawn = pane(&app);
    assert_eq!(drawn.header, Rect::new(composer.x, 16, composer.width, 1));
    assert_eq!(drawn.content, Rect::new(composer.x, 17, composer.width, 6));
    assert_eq!(app.composer_area, Rect { y: 11, ..composer });
    let shorter = Rect {
        height: transcript.height - 7,
        ..transcript
    };
    assert_eq!(app.transcript_area, shorter);
    assert!(rows[16].contains(" Terminal · attaching"), "{rows:#?}");
    assert!(rows[16].trim_end().ends_with("F4 hide"), "{rows:#?}");
    let connecting = " Connecting to the thread's terminal…";
    assert!(rows[17].contains(connecting), "{rows:#?}");

    // F4 again hides it, and the composer has the keyboard and the draft.
    press(&mut app, KeyCode::F(4));
    assert!(app.terminal.is_none());
    assert_eq!(app.focus, Focus::Composer);
    screen(&mut app, 80, 24);
    assert_eq!(app.transcript_area, transcript);
    assert_eq!(app.composer_area, composer);
    assert_eq!(app.composer.text(), "draft");
}

#[test]
fn f4_with_no_thread_open_says_to_open_one() {
    let mut app = tui(&Settings::default());
    screen(&mut app, 80, 24);
    assert_eq!(app.terminal_button, Rect::default());
    press(&mut app, KeyCode::F(4));
    assert!(app.terminal.is_none());
    let note = "Open a thread first. F4 shows its terminal.";
    assert_eq!(app.message, Some((note.to_string(), false)));
}

#[test]
fn f4_held_down_shows_the_drawer_once() {
    let mut app = details_tui();
    let mut held = KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE);
    held.kind = KeyEventKind::Repeat;
    app.on_terminal_event(Event::Key(held));
    assert!(app.terminal.is_none());
    press(&mut app, KeyCode::F(4));
    for _ in 0..3 {
        app.on_terminal_event(Event::Key(held));
    }
    let mut up = held;
    up.kind = KeyEventKind::Release;
    app.on_terminal_event(Event::Key(up));
    assert!(app.terminal.is_some());
    assert_eq!(app.focus, Focus::Terminal);
}

#[test]
fn the_drawer_sends_the_shell_every_key_but_f4_f6_and_ctrl_q() {
    let mut app = details_tui();
    drawer_at(&mut app, 80, 24);
    let keys = [
        (KeyCode::Char('a'), KeyModifiers::NONE),
        (KeyCode::Enter, KeyModifiers::NONE),
        (KeyCode::Tab, KeyModifiers::NONE),
        (KeyCode::BackTab, KeyModifiers::SHIFT),
        (KeyCode::Esc, KeyModifiers::NONE),
        (KeyCode::Char('c'), KeyModifiers::CONTROL),
        (KeyCode::Char('x'), KeyModifiers::CONTROL),
        (KeyCode::Char('b'), KeyModifiers::CONTROL),
        (KeyCode::Char('b'), KeyModifiers::ALT),
        (KeyCode::Char('m'), KeyModifiers::ALT),
        (KeyCode::PageUp, KeyModifiers::NONE),
    ];
    for (code, modifiers) in keys {
        app.on_terminal_event(Event::Key(KeyEvent::new(code, modifiers)));
        let at = format!("{modifiers:?} {code:?}");
        // The shell isn't attached yet, so the key goes nowhere and the status line says so.
        let note = "The terminal is still attaching.".to_string();
        assert_eq!(app.message, Some((note, true)), "{at}");
        assert_eq!(app.focus, Focus::Terminal, "{at}");
        assert!(
            !app.quit && !app.sidebar_hidden && !app.details.open,
            "{at}"
        );
        assert!(app.picker.is_none(), "{at}");
    }
    assert_eq!(app.composer.text(), "draft");
    // Shift+PgUp and Shift+PgDn scroll the drawer's history and send nothing.
    for code in [KeyCode::PageUp, KeyCode::PageDown] {
        app.on_terminal_event(Event::Key(KeyEvent::new(code, KeyModifiers::SHIFT)));
        assert_eq!(app.message, None, "{code:?}");
    }
    // A paste goes to the shell too.
    app.on_terminal_event(Event::Paste("ls\n".into()));
    let note = "The terminal is still attaching.".to_string();
    assert_eq!(app.message, Some((note, true)));
    assert_eq!(app.composer.text(), "draft");
    let quit = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
    app.on_terminal_event(Event::Key(quit));
    assert!(app.quit);
}

#[test]
fn f6_moves_through_the_drawer_and_tab_goes_round_it() {
    let mut app = details_tui();
    drawer_at(&mut app, 80, 24);
    app.focus = Focus::Composer;
    let order = focus_after(&mut app, &[KeyCode::Tab; 3]);
    assert_eq!(order, [Focus::Transcript, Focus::Sidebar, Focus::Composer]);
    let order = focus_after(&mut app, &[KeyCode::F(6); 4]);
    assert_eq!(
        order,
        [
            Focus::Terminal,
            Focus::Transcript,
            Focus::Sidebar,
            Focus::Composer
        ]
    );
    // Shift+F6 goes back, from the drawer too.
    press(&mut app, KeyCode::F(6));
    let back = KeyEvent::new(KeyCode::F(6), KeyModifiers::SHIFT);
    app.on_terminal_event(Event::Key(back));
    assert_eq!(app.focus, Focus::Composer);
    // F4 from the composer hides the drawer, which leaves the cycle.
    press(&mut app, KeyCode::F(4));
    assert!(app.terminal.is_none());
    let order = focus_after(&mut app, &[KeyCode::F(6); 3]);
    assert_eq!(order, [Focus::Transcript, Focus::Sidebar, Focus::Composer]);
}

#[test]
fn a_details_dialog_keeps_the_keyboard_and_the_mouse_from_the_drawer() {
    let mut app = details_tui();
    drawer_at(&mut app, 120, 36);
    app.focus = Focus::Composer;
    alt(&mut app, 'b');
    screen(&mut app, 120, 36);
    assert!(app.details_modal());
    // F4, F6 and a paste go to the dialog, which does nothing with them.
    press(&mut app, KeyCode::F(4));
    press(&mut app, KeyCode::F(6));
    app.on_terminal_event(Event::Paste("ls".into()));
    assert!(app.terminal.is_some());
    assert!(app.details_modal());
    assert_eq!(app.message, None);
    // A click on the drawer, beside the dialog, closes the dialog and leaves the drawer alone.
    let content = pane(&app).content;
    let dialog = app.details.drawn.expect("the dialog").area;
    assert!(content.x + 1 < dialog.x, "{content:?} {dialog:?}");
    assert!(mouse(&mut app, CLICK, content.x + 1, content.y));
    assert!(!app.details.open);
    assert_eq!(app.focus, Focus::Composer);
}

#[test]
fn the_title_rows_button_shows_and_hides_the_drawer_once_drawn() {
    let mut app = details_tui();
    let buffer = cells(&mut app, 80, 24);
    // The button comes before the details' button.
    let details = app.details.button;
    let button = app.terminal_button;
    assert_eq!(button, Rect::new(details.x - 5, details.y, 4, 1));
    assert_eq!(text_in(&buffer, button), " >_ ");
    assert!(mouse(&mut app, CLICK, button.x, button.y));
    assert!(app.terminal.is_some());
    assert_eq!(app.focus, Focus::Terminal);
    // Until a frame draws the drawer, the button takes a click and does nothing with it.
    assert!(!mouse(&mut app, CLICK, button.x + 3, button.y));
    assert!(app.terminal.is_some());

    // A click on the drawer's header or screen gives it the keyboard.
    screen(&mut app, 80, 24);
    let (header, content) = (pane(&app).header, pane(&app).content);
    for (x, y) in [
        (header.x + 3, header.y),
        (content.right() - 1, content.bottom() - 1),
    ] {
        app.focus = Focus::Composer;
        assert!(mouse(&mut app, CLICK, x, y));
        assert_eq!(app.focus, Focus::Terminal);
    }

    // After a resize, until a frame draws them again, neither the button nor the drawer takes
    // a click.
    app.on_terminal_event(Event::Resize(80, 24));
    app.focus = Focus::Composer;
    mouse(&mut app, CLICK, button.x, button.y);
    mouse(&mut app, CLICK, content.x, content.y);
    assert!(app.terminal.is_some());
    assert_eq!(app.focus, Focus::Composer);

    // Drawn again, the button hides it, and the composer gets the keyboard back.
    screen(&mut app, 80, 24);
    app.focus = Focus::Terminal;
    assert!(mouse(&mut app, CLICK, button.x, button.y));
    assert!(app.terminal.is_none());
    assert_eq!(app.focus, Focus::Composer);

    // A title row too narrow for the breadcrumb and both buttons keeps the details' alone.
    cells(&mut app, 40, 24);
    assert!(app.details.button.width > 0);
    assert_eq!(app.terminal_button, Rect::default());
}

#[test]
fn the_drawer_takes_half_the_free_rows_up_to_seventeen_and_leaves_the_transcript_three() {
    let sizes = [
        (80, 24, 7),
        (120, 36, 13),
        (200, 60, 17),
        (80, 14, 3),
        (80, 10, 0),
    ];
    for (width, height, rows) in sizes {
        let at = format!("{width}x{height}");
        let mut app = details_tui();
        drawer_at(&mut app, width, height);
        let drawn = pane(&app);
        assert_eq!(drawn.header.height + drawn.content.height, rows, "{at}");
        if rows == 0 {
            // A drawer with no rows gives the keyboard to the composer.
            assert_eq!(app.focus, Focus::Composer, "{at}");
            continue;
        }
        assert_eq!(drawn.header.y, app.composer_area.bottom(), "{at}");
        assert_eq!(drawn.content.bottom(), height - 1, "{at}");
        assert!(app.transcript_area.height >= 3, "{at}");
        assert_eq!(app.focus, Focus::Terminal, "{at}");
    }
}

#[test]
fn the_drawer_fits_any_screen_and_has_the_keyboard_only_where_it_shows() {
    let mut app = details_tui();
    press(&mut app, KeyCode::F(4));
    let small = [
        (0, 0),
        (1, 1),
        (2, 2),
        (3, 24),
        (10, 5),
        (19, 3),
        (26, 4),
        (80, 10),
    ];
    // Growing, then shrinking, so nothing from a larger frame is left behind.
    for (width, height) in small.into_iter().chain([(80, 24), (200, 60), (26, 4)]) {
        let at = format!("{width}x{height}");
        app.on_terminal_event(Event::Resize(width, height));
        app.focus = Focus::Terminal;
        for _ in 0..2 {
            screen(&mut app, width, height);
            let whole = Rect::new(0, 0, width, height);
            let drawn = pane(&app);
            for area in [
                drawn.header,
                drawn.content,
                drawn.start,
                app.terminal_button,
            ] {
                assert_eq!(area.intersection(whole), area, "{at}");
            }
            let shows = !drawn.header.is_empty();
            assert_eq!(app.focus == Focus::Terminal, shows, "{at}");
        }
        press(&mut app, KeyCode::F(6));
    }
}

#[test]
fn the_status_line_names_the_drawers_keys() {
    let mut app = details_tui();
    let status = chrome(&mut app, 200, 50).status;
    assert!(
        status.contains("Ctrl+X interrupt · F4 terminal"),
        "{status}"
    );
    press(&mut app, KeyCode::F(4));
    let keys =
        "Keys go to the shell · Shift+PgUp/PgDn scroll · F4 hide · F6 next pane · Ctrl+Q quit";
    let status = chrome(&mut app, 200, 50).status;
    assert!(status.contains(keys), "{status}");
    // With the sidebar hidden, Ctrl+B goes to the shell, so the status line doesn't offer it.
    app.focus = Focus::Composer;
    ctrl_b(&mut app);
    assert!(app.sidebar_hidden);
    app.focus = Focus::Terminal;
    let status = chrome(&mut app, 200, 50).status;
    assert!(status.contains(keys), "{status}");
    assert!(!status.contains("Ctrl+B"), "{status}");
}

#[test]
fn start_terminal_asks_for_the_shell_the_desktop_starts_and_never_guesses() {
    let worktree = "/repo/.t3/worktrees/feature-a";
    let launches = [
        (Value::Null, "p", Launch::for_thread("/repo/demo", None)),
        (
            json!(worktree),
            "p",
            Launch::for_thread("/repo/demo", Some(worktree)),
        ),
        // T3 trims both paths, and t3term sends them trimmed.
        (
            json!(format!(" {worktree}\n")),
            "q",
            Launch::for_thread("/repo/other", Some(worktree)),
        ),
    ];
    for (path, project, launch) in launches {
        let thread = bound("t", project, path, Value::Null);
        let at = thread.to_string();
        assert_eq!(
            opened(thread).terminal_launch(),
            Ok(launch.unwrap()),
            "{at}"
        );
    }

    let worktree_unknown =
        "T3 hasn't said where this thread's worktree is, so t3term can't start its terminal.";
    let project_unknown =
        "T3 hasn't said where this thread's project is, so t3term can't start its terminal.";
    let mut without = bound("t", "p", Value::Null, Value::Null);
    without.as_object_mut().unwrap().remove("worktreePath");
    let refusals = [
        (without, worktree_unknown),
        (bound("t", "p", json!("  "), Value::Null), worktree_unknown),
        (bound("t", "p", json!(7), Value::Null), worktree_unknown),
        (bound("t", "z", Value::Null, Value::Null), project_unknown),
    ];
    for (thread, reason) in refusals {
        let at = thread.to_string();
        let launch = opened(thread).terminal_launch();
        assert_eq!(launch, Err(reason.to_string()), "{at}");
    }

    // A project whose root is blank, and no thread at all.
    let mut app = tui(&Settings::default());
    let thread = bound("t", "p", Value::Null, Value::Null);
    let projects = json!([{"id": "p", "title": "Demo", "workspaceRoot": "  "}]);
    let threads = [thread.clone()];
    let snapshot = json!({"snapshotSequence": 1, "projects": projects, "threads": threads});
    app.on_shell_event(WatchEvent::Item(
        json!({"kind": "snapshot", "snapshot": snapshot}),
    ));
    app.open_thread(open_bound(thread));
    assert_eq!(app.terminal_launch(), Err(project_unknown.to_string()));
    let loading = "t3term hasn't loaded this thread yet, so it can't start its terminal.";
    let launch = tui(&Settings::default()).terminal_launch();
    assert_eq!(launch, Err(loading.to_string()));
}

/// Runs the drawer's work as the event loop does until something other than a deadline
/// happens, and says what.
async fn run_drawer(app: &mut App) -> &'static str {
    loop {
        let drawer = app.terminal.as_mut().expect("the drawer shows");
        let update = within(drawer.next()).await;
        let happened = kind(&update);
        app.on_pane_update(update);
        if happened != "due" {
            return happened;
        }
    }
}

/// Runs the drawer until its first attach goes out to `fake`, then fails it as T3 does for a
/// thread with no terminal. Returns the server's end of the connection.
async fn attach_missing(fake: &Fake, app: &mut App, thread: &str) -> Socket {
    let serving = async {
        let mut socket = fake.accept().await;
        let request = recv(&mut socket).await;
        (socket, request)
    };
    let (happened, (mut socket, request)) = tokio::join!(run_drawer(app), serving);
    assert_eq!(happened, "attached");
    let named = json!({"threadId": thread, "terminalId": "term-1"});
    let id = expect_request(&request, ATTACH, named);
    send(&mut socket, refusal(&id, lookup_error(thread))).await;
    assert_eq!(run_drawer(app).await, "batch");
    assert!(pane(app).missing());
    socket
}

#[tokio::test]
async fn enter_starts_a_missing_terminal_and_another_thread_gets_its_own() {
    let fake = Fake::new().await;
    let mut app = opened(bound("t", "p", Value::Null, Value::Null));
    app.connect_terminal = fake.connect();
    drawer_at(&mut app, 80, 24);
    let mut socket = attach_missing(&fake, &mut app, "t").await;
    let rows = screen(&mut app, 80, 24);
    let start = pane(&app).start;
    assert_eq!(row_showing(&rows, "Start terminal   Enter"), start.y);
    assert!(rows[23].contains("Enter start terminal"), "{rows:#?}");

    // Enter starts it as the desktop does, in the project's root, on the same connection.
    press(&mut app, KeyCode::Enter);
    let (happened, request) = tokio::join!(run_drawer(&mut app), recv(&mut socket));
    assert_eq!(happened, "attached");
    let env = json!({"T3CODE_PROJECT_ROOT": "/repo/demo"});
    let launch = json!({"threadId": "t", "terminalId": "term-1", "cwd": "/repo/demo", "env": env});
    let id = expect_request(&request, ATTACH, launch);
    let running = json!({"type": "snapshot", "snapshot": session("t", 0, "running", "$ ")});
    send(&mut socket, chunk(&id, json!([running]))).await;
    assert_eq!(run_drawer(&mut app).await, "batch");
    assert_eq!(recv(&mut socket).await, ack(&id));
    let rows = screen(&mut app, 80, 24);
    assert!(rows[16].contains(" Terminal · zsh · running"), "{rows:#?}");

    // Another thread's drawer attaches to that thread's terminal by name. The shell left
    // behind goes on running: its stream is interrupted and nothing else.
    app.open_thread(open_bound(bound("u", "q", Value::Null, Value::Null)));
    assert_eq!(recv(&mut socket).await, interrupt(&id));
    let (happened, request) = tokio::join!(run_drawer(&mut app), recv(&mut socket));
    assert_eq!(happened, "attached");
    let named = json!({"threadId": "u", "terminalId": "term-1"});
    let id = expect_request(&request, ATTACH, named);

    // F4 hides the drawer, which interrupts its stream and nothing else.
    press(&mut app, KeyCode::F(4));
    assert!(app.terminal.is_none());
    assert_eq!(fake.sent(&mut socket).await, [interrupt(&id)]);
}

#[tokio::test]
async fn a_click_on_start_terminal_says_why_it_cant_and_sends_nothing() {
    let fake = Fake::new().await;
    // The shell lists Demo without its workspace root.
    let thread = bound("t", "p", Value::Null, Value::Null);
    let mut app = tui(&Settings::default());
    app.on_shell_event(shell_snapshot(vec![thread.clone()]));
    app.open_thread(open_bound(thread));
    app.connect_terminal = fake.connect();
    drawer_at(&mut app, 80, 24);
    let mut socket = attach_missing(&fake, &mut app, "t").await;
    screen(&mut app, 80, 24);
    app.focus = Focus::Composer;
    let start = pane(&app).start;
    assert!(mouse(&mut app, CLICK, start.x + 2, start.y));
    assert_eq!(app.focus, Focus::Terminal);
    let reason =
        "T3 hasn't said where this thread's project is, so t3term can't start its terminal.";
    assert_eq!(app.message, Some((reason.to_string(), true)));
    // The drawer says so under Start terminal, and still offers it.
    let rows = screen(&mut app, 80, 24);
    let below = &rows[usize::from(start.y) + 1];
    assert!(below.contains(" T3 hasn't said where"), "{rows:#?}");
    assert!(pane(&app).missing());
    assert_eq!(fake.sent(&mut socket).await, Vec::<Value>::new());
}

/// Runs the drawer until its first attach goes out to `fake`, and answers it with a running
/// shell's snapshot titled `title`. Returns the server's end of the connection and the
/// stream's request id.
async fn attach_running(fake: &Fake, app: &mut App, thread: &str, title: &str) -> (Socket, Value) {
    let serving = async {
        let mut socket = fake.accept().await;
        let request = recv(&mut socket).await;
        (socket, request)
    };
    let (happened, (mut socket, request)) = tokio::join!(run_drawer(app), serving);
    assert_eq!(happened, "attached");
    let named = json!({"threadId": thread, "terminalId": "term-1"});
    let id = expect_request(&request, ATTACH, named);
    let mut running = session(thread, 0, "running", "$ ");
    running["label"] = json!(title);
    let snapshot = json!({"type": "snapshot", "snapshot": running});
    send(&mut socket, chunk(&id, json!([snapshot]))).await;
    assert_eq!(run_drawer(app).await, "batch");
    assert_eq!(recv(&mut socket).await, ack(&id));
    (socket, id)
}

#[tokio::test]
async fn a_hostile_title_leaves_the_state_in_the_header_and_the_cells_around_it_alone() {
    for (width, height) in [(80, 24), (120, 36)] {
        // Each title as long as T3 sends one, still wide, joined, marked or full of controls.
        // A label over T3's 128 UTF-16 code units is malformed, which terminal.rs tests.
        for title in hostile_titles().iter().map(|title| wire_label(title)) {
            assert!(title.encode_utf16().count() <= 128, "{title:?}");
            let at = format!("{title:?} at {width}x{height}");
            let fake = Fake::new().await;
            let mut app = opened(bound("t", "p", Value::Null, Value::Null));
            app.connect_terminal = fake.connect();
            press(&mut app, KeyCode::F(4));
            // Until the snapshot the header has no title, so the cells beside it are the
            // ones a title must leave alone.
            let before = cells(&mut app, width, height);
            let header = pane(&app).header;
            let left = Rect {
                x: 0,
                width: header.x,
                ..header
            };
            let right = Rect {
                x: header.right(),
                width: width - header.right(),
                ..header
            };
            let (mut socket, id) = attach_running(&fake, &mut app, "t", &title).await;
            let exited = json!({"type": "exited", "threadId": "t", "terminalId": "term-1",
                "sequence": 1, "exitCode": 0, "exitSignal": null});
            let states = [
                (" · running", Some(exited)),
                (" · exited with 0, read-only", None),
            ];
            for (state, next) in states {
                let after = cells(&mut app, width, height);
                assert_eq!(pane(&app).header, header, "{at}");
                for area in [left, right] {
                    assert_eq!(text_in(&after, area), text_in(&before, area), "{at}");
                }
                let text = text_in(&after, header);
                assert!(text.starts_with(" Terminal · "), "{at}: {text:?}");
                assert!(text.contains(state), "{at}: {text:?}");
                if let Some(event) = next {
                    send(&mut socket, chunk(&id, json!([event]))).await;
                    assert_eq!(run_drawer(&mut app).await, "batch");
                }
            }
        }
    }
}
