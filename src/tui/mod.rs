//! The interactive client. It draws only after input or a server event, at most 30 times a second,
//! and never on a timer, so an idle TUI uses no CPU.

mod composer;
mod markdown;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use futures_util::StreamExt;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthStr;

use crate::client::{Client, IfBusy, WatchEvent};
use crate::projection::{Applied, ShellState, ThreadState, is_active_status, status};
use crate::transcript::{self, BlockKind};
use composer::Composer;

const FRAME: Duration = Duration::from_millis(33);
const DIM: Style = Style::new().fg(Color::DarkGray);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Sidebar,
    Transcript,
    Composer,
}

enum Row {
    Project(String),
    Thread(String),
}

/// Wrapped lines for one transcript block, valid for one width and content version.
struct Cached {
    key: (u64, u16),
    lines: Vec<Line<'static>>,
}

enum ActionResult {
    Info(String),
    Error(String),
}

struct OpenThread {
    id: String,
    state: Option<ThreadState>,
    events: mpsc::UnboundedReceiver<WatchEvent>,
    connection: String,
    /// Answers collected so far for the pending question request.
    answers: serde_json::Map<String, Value>,
}

struct App {
    client: Arc<Client>,
    shell: Option<ShellState>,
    shell_connection: String,
    rows: Vec<Row>,
    selected: usize,
    sidebar_offset: usize,
    open: Option<OpenThread>,
    focus: Focus,
    composer: Composer,
    /// Rows scrolled up from the bottom. Zero follows new output.
    scroll: usize,
    expand_tools: bool,
    cache: HashMap<String, Cached>,
    message: Option<(String, bool)>,
    actions: mpsc::UnboundedSender<ActionResult>,
    // Last drawn geometry, for mouse hit-testing and page sizes.
    sidebar_area: Rect,
    transcript_area: Rect,
    quit: bool,
}

pub async fn run(client: Arc<Client>) -> Result<()> {
    let mut terminal = ratatui::init();
    crossterm::execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste)?;
    let result = event_loop(&mut terminal, client).await;
    let _ = crossterm::execute!(
        std::io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste
    );
    ratatui::restore();
    result
}

async fn event_loop(terminal: &mut ratatui::DefaultTerminal, client: Arc<Client>) -> Result<()> {
    let (actions, mut action_results) = mpsc::unbounded_channel();
    let mut shell_events = client.watch_shell(None);
    let mut app = App {
        client,
        shell: None,
        shell_connection: "connecting".into(),
        rows: Vec::new(),
        selected: 0,
        sidebar_offset: 0,
        open: None,
        focus: Focus::Sidebar,
        composer: Composer::default(),
        scroll: 0,
        expand_tools: false,
        cache: HashMap::new(),
        message: None,
        actions,
        sidebar_area: Rect::default(),
        transcript_area: Rect::default(),
        quit: false,
    };
    let mut input = EventStream::new();
    let mut dirty = true;
    let mut last_draw = Instant::now() - FRAME;

    while !app.quit {
        if dirty && last_draw.elapsed() >= FRAME {
            terminal.draw(|frame| app.draw(frame))?;
            last_draw = Instant::now();
            dirty = false;
        }
        let redraw_at = tokio::time::Instant::from_std(last_draw + FRAME);
        let thread_event = async {
            match app.open.as_mut() {
                Some(open) => open.events.recv().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            event = input.next() => match event {
                Some(Ok(event)) => dirty |= app.on_terminal_event(event),
                Some(Err(e)) => return Err(e.into()),
                None => break,
            },
            Some(event) = shell_events.recv() => {
                app.on_shell_event(event);
                dirty = true;
            }
            Some(event) = thread_event => {
                app.on_thread_event(event);
                dirty = true;
            }
            Some(result) = action_results.recv() => {
                app.message = Some(match result {
                    ActionResult::Info(text) => (text, false),
                    ActionResult::Error(text) => (text, true),
                });
                dirty = true;
            }
            // Only armed while a draw is waiting out the frame budget.
            _ = tokio::time::sleep_until(redraw_at), if dirty => {}
        }
    }
    Ok(())
}

fn str_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn hash(parts: &[&str]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    parts.hash(&mut hasher);
    hasher.finish()
}

impl App {
    // ---- server events ----

    fn on_shell_event(&mut self, event: WatchEvent) {
        match event {
            WatchEvent::Item(item) => {
                self.shell
                    .get_or_insert_with(ShellState::default)
                    .apply(&item);
                if item["kind"] == "synchronized" {
                    self.shell_connection = "live".into();
                }
                self.rebuild_rows();
            }
            WatchEvent::Reconnecting { .. } => self.shell_connection = "reconnecting".into(),
            WatchEvent::Failed(message) => self.message = Some((message, true)),
        }
    }

    fn on_thread_event(&mut self, event: WatchEvent) {
        let Some(open) = self.open.as_mut() else {
            return;
        };
        match event {
            WatchEvent::Item(item) => {
                let state = open.state.get_or_insert_with(ThreadState::default);
                match state.apply(&item) {
                    Applied::Synchronized => open.connection = "live".into(),
                    Applied::Snapshot => self.cache.clear(),
                    _ => {}
                }
            }
            WatchEvent::Reconnecting { reason, .. } => {
                open.connection = format!("reconnecting: {reason}")
            }
            WatchEvent::Failed(message) => {
                open.connection = "closed".into();
                self.message = Some((message, true));
            }
        }
    }

    /// Projects ordered by their newest thread; threads newest first. Delegated child threads are hidden.
    fn rebuild_rows(&mut self) {
        let Some(shell) = self.shell.as_ref() else {
            return;
        };
        let selected_id = self.selected_thread_id().map(str::to_string);
        let mut by_project: HashMap<&str, Vec<&Value>> = HashMap::new();
        for thread in shell
            .threads
            .iter()
            .filter(|t| t["lineage"]["parentThreadId"].is_null())
        {
            by_project
                .entry(str_of(thread, "projectId"))
                .or_default()
                .push(thread);
        }
        let mut groups: Vec<(String, Vec<&Value>)> = by_project
            .into_iter()
            .map(|(project_id, mut threads)| {
                threads.sort_by(|a, b| str_of(b, "updatedAt").cmp(str_of(a, "updatedAt")));
                let title = shell
                    .projects
                    .iter()
                    .find(|p| str_of(p, "id") == project_id)
                    .map(|p| str_of(p, "title").to_string())
                    .unwrap_or_else(|| "Other".into());
                (title, threads)
            })
            .collect();
        groups.sort_by(|a, b| str_of(b.1[0], "updatedAt").cmp(str_of(a.1[0], "updatedAt")));
        self.rows.clear();
        for (title, threads) in groups {
            self.rows.push(Row::Project(title));
            for thread in threads {
                self.rows
                    .push(Row::Thread(str_of(thread, "id").to_string()));
            }
        }
        self.selected = selected_id
            .and_then(|id| {
                self.rows
                    .iter()
                    .position(|r| matches!(r, Row::Thread(t) if *t == id))
            })
            .or_else(|| self.rows.iter().position(|r| matches!(r, Row::Thread(_))))
            .unwrap_or(0);
    }

    fn selected_thread_id(&self) -> Option<&str> {
        match self.rows.get(self.selected) {
            Some(Row::Thread(id)) => Some(id),
            _ => None,
        }
    }

    fn shell_thread(&self, id: &str) -> Option<&Value> {
        self.shell
            .as_ref()?
            .threads
            .iter()
            .find(|t| str_of(t, "id") == id)
    }

    fn move_selection(&mut self, delta: isize) {
        let mut index = self.selected as isize;
        loop {
            index += delta;
            if index < 0 || index >= self.rows.len() as isize {
                return;
            }
            if matches!(self.rows[index as usize], Row::Thread(_)) {
                self.selected = index as usize;
                return;
            }
        }
    }

    fn open_selected(&mut self) {
        let Some(id) = self.selected_thread_id().map(str::to_string) else {
            return;
        };
        if self.open.as_ref().is_some_and(|o| o.id == id) {
            self.focus = Focus::Composer;
            return;
        }
        let events = self.client.watch_thread(&id, None);
        self.open = Some(OpenThread {
            id,
            state: None,
            events,
            connection: "connecting".into(),
            answers: Default::default(),
        });
        self.cache.clear();
        self.scroll = 0;
        self.focus = Focus::Composer;
    }

    // ---- input ----

    fn on_terminal_event(&mut self, event: Event) -> bool {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                self.on_key(key);
                true
            }
            Event::Paste(text) => {
                if self.focus == Focus::Composer {
                    self.composer.insert_str(&text);
                }
                true
            }
            Event::Mouse(mouse) => {
                let inside = |area: Rect| {
                    mouse.column >= area.x
                        && mouse.column < area.x + area.width
                        && mouse.row >= area.y
                        && mouse.row < area.y + area.height
                };
                match mouse.kind {
                    MouseEventKind::ScrollUp if inside(self.transcript_area) => self.scroll += 3,
                    MouseEventKind::ScrollDown if inside(self.transcript_area) => {
                        self.scroll = self.scroll.saturating_sub(3)
                    }
                    MouseEventKind::ScrollUp if inside(self.sidebar_area) => {
                        self.move_selection(-1)
                    }
                    MouseEventKind::ScrollDown if inside(self.sidebar_area) => {
                        self.move_selection(1)
                    }
                    MouseEventKind::Down(MouseButton::Left) if inside(self.sidebar_area) => {
                        let row = self.sidebar_offset + (mouse.row - self.sidebar_area.y) as usize;
                        if matches!(self.rows.get(row), Some(Row::Thread(_))) {
                            self.selected = row;
                            self.open_selected();
                        }
                    }
                    MouseEventKind::Down(MouseButton::Left) if inside(self.transcript_area) => {
                        self.focus = Focus::Transcript
                    }
                    MouseEventKind::Down(MouseButton::Left) => self.focus = Focus::Composer,
                    _ => return false,
                }
                true
            }
            Event::Resize(..) => {
                self.cache.clear();
                true
            }
            _ => false,
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char('c') | KeyCode::Char('q') if ctrl => {
                self.quit = true;
                return;
            }
            KeyCode::Char('x') if ctrl => {
                self.interrupt();
                return;
            }
            // Answer approvals from any pane. Terminals report Esc followed quickly by a letter as Alt.
            KeyCode::Char(c @ ('a' | 's' | 'd')) if alt => {
                self.respond(match c {
                    'a' => "accept",
                    's' => "acceptForSession",
                    _ => "decline",
                });
                return;
            }
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Sidebar => Focus::Composer,
                    Focus::Composer => Focus::Transcript,
                    Focus::Transcript => Focus::Sidebar,
                };
                return;
            }
            KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Sidebar => Focus::Transcript,
                    Focus::Composer => Focus::Sidebar,
                    Focus::Transcript => Focus::Composer,
                };
                return;
            }
            KeyCode::PageUp => {
                self.scroll += self.page();
                return;
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_sub(self.page());
                return;
            }
            _ => {}
        }
        match self.focus {
            Focus::Composer => match key.code {
                KeyCode::Esc => self.focus = Focus::Transcript,
                KeyCode::Enter if alt || key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.composer.insert('\n')
                }
                KeyCode::Char('j') if ctrl => self.composer.insert('\n'),
                KeyCode::Enter => self.submit(),
                KeyCode::Char('w') if ctrl => self.composer.delete_word(),
                KeyCode::Char('u') if ctrl => self.composer.clear(),
                KeyCode::Char('a') if ctrl => self.composer.home(),
                KeyCode::Char('e') if ctrl => self.composer.end(),
                KeyCode::Char(c) if !ctrl && !alt => self.composer.insert(c),
                KeyCode::Backspace => self.composer.backspace(),
                KeyCode::Delete => self.composer.delete(),
                KeyCode::Left => self.composer.left(),
                KeyCode::Right => self.composer.right(),
                KeyCode::Home => self.composer.home(),
                KeyCode::End => self.composer.end(),
                KeyCode::Up => {
                    if !self.composer.up() {
                        self.scroll += 1;
                    }
                }
                KeyCode::Down => {
                    if !self.composer.down() {
                        self.scroll = self.scroll.saturating_sub(1);
                    }
                }
                _ => {}
            },
            Focus::Sidebar => match key.code {
                KeyCode::Char('q') => self.quit = true,
                KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
                KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.open_selected(),
                _ => {}
            },
            Focus::Transcript => match key.code {
                KeyCode::Char('q') => self.quit = true,
                KeyCode::Up | KeyCode::Char('k') => self.scroll += 1,
                KeyCode::Down | KeyCode::Char('j') => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::Char('g') | KeyCode::Home => self.scroll = usize::MAX / 2,
                KeyCode::Char('G') | KeyCode::End => self.scroll = 0,
                KeyCode::Char('t') => {
                    self.expand_tools = !self.expand_tools;
                    self.cache.clear();
                }
                KeyCode::Char('a') => self.respond("accept"),
                KeyCode::Char('s') => self.respond("acceptForSession"),
                KeyCode::Char('d') => self.respond("decline"),
                KeyCode::Char('i') | KeyCode::Enter => self.focus = Focus::Composer,
                KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => self.focus = Focus::Sidebar,
                _ => {}
            },
        }
    }

    fn page(&self) -> usize {
        (self.transcript_area.height as usize)
            .saturating_sub(2)
            .max(1)
    }

    fn pending_request(&self) -> Option<(Value, Option<Value>)> {
        let state = self.open.as_ref()?.state.as_ref()?;
        let request = state.pending_requests().into_iter().next()?.clone();
        let item = state.request_item(str_of(&request, "id")).cloned();
        Some((request, item))
    }

    fn spawn_action<F>(&self, action: F)
    where
        F: std::future::Future<Output = Result<String>> + Send + 'static,
    {
        let results = self.actions.clone();
        tokio::spawn(async move {
            let _ = results.send(match action.await {
                Ok(text) => ActionResult::Info(text),
                Err(e) => ActionResult::Error(e.to_string()),
            });
        });
    }

    fn submit(&mut self) {
        if self.composer.is_empty() {
            return;
        }
        let Some(open) = self.open.as_mut() else {
            self.message = Some((
                "Open a thread first: pick one in the sidebar and press Enter.".into(),
                true,
            ));
            return;
        };
        let Some(state) = open.state.clone() else {
            return;
        };
        let text = self.composer.text();

        // A pending question takes the composer's text as its answer, one question at a time.
        if let Some(request) = state
            .pending_requests()
            .into_iter()
            .find(|r| r["kind"] == "user_input")
        {
            let request_id = str_of(request, "id").to_string();
            let questions = state
                .request_item(&request_id)
                .and_then(|i| i["questions"].as_array().cloned())
                .unwrap_or_default();
            if let Some(question) = questions
                .iter()
                .find(|q| !open.answers.contains_key(str_of(q, "id")))
            {
                let options = question["options"].as_array().cloned().unwrap_or_default();
                let answer = match text.trim().parse::<usize>() {
                    Ok(n) if (1..=options.len()).contains(&n) => {
                        let option = &options[n - 1];
                        option
                            .get("value")
                            .and_then(Value::as_str)
                            .unwrap_or(str_of(option, "label"))
                            .to_string()
                    }
                    _ => text.trim().to_string(),
                };
                let answer = if question["multiSelect"] == true {
                    json!([answer])
                } else {
                    json!(answer)
                };
                open.answers
                    .insert(str_of(question, "id").to_string(), answer);
                self.composer.clear();
                if open.answers.len() < questions.len() {
                    return;
                }
                let answers = Value::Object(std::mem::take(&mut open.answers));
                let (client, thread_id) = (self.client.clone(), open.id.clone());
                self.spawn_action(async move {
                    client.answer(&thread_id, &request_id, answers).await?;
                    Ok("Answer sent.".into())
                });
                return;
            }
        }

        self.composer.clear();
        self.scroll = 0;
        let client = self.client.clone();
        self.spawn_action(async move {
            let receipt = client.send_message(&state, &text, IfBusy::Queue).await?;
            Ok(match receipt.dispatch_mode {
                "queue_after_active" => "Queued after the running turn.".into(),
                _ => "Sent.".into(),
            })
        });
    }

    fn respond(&mut self, decision: &'static str) {
        let Some((request, _)) = self.pending_request() else {
            return;
        };
        if request["kind"] == "user_input" {
            self.focus = Focus::Composer;
            return;
        }
        let Some(open) = self.open.as_ref() else {
            return;
        };
        let (client, thread_id, request_id) = (
            self.client.clone(),
            open.id.clone(),
            str_of(&request, "id").to_string(),
        );
        self.spawn_action(async move {
            client.respond(&thread_id, &request_id, decision).await?;
            Ok(format!("Answered: {decision}."))
        });
    }

    fn interrupt(&mut self) {
        let Some(open) = self.open.as_ref() else {
            return;
        };
        let Some(run_id) = open
            .state
            .as_ref()
            .and_then(|s| s.active_run())
            .map(|r| str_of(r, "id").to_string())
        else {
            self.message = Some(("Nothing is running.".into(), false));
            return;
        };
        let (client, thread_id) = (self.client.clone(), open.id.clone());
        self.spawn_action(async move {
            client.interrupt(&thread_id, &run_id).await?;
            Ok("Interrupt sent.".into())
        });
    }

    // ---- drawing ----

    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let sidebar_width = (area.width / 4).clamp(24, 40);
        let [sidebar, main] =
            Layout::horizontal([Constraint::Length(sidebar_width), Constraint::Min(20)])
                .areas(area);
        self.draw_sidebar(frame, sidebar);

        let (composer_rows, cursor) = self.composer.layout(main.width.saturating_sub(4) as usize);
        let composer_height = (composer_rows.len() as u16).clamp(1, 8) + 2;
        let banner = self.request_banner(main.width as usize);
        let [header, body, banner_area, composer_area, status_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(banner.len() as u16),
            Constraint::Length(composer_height),
            Constraint::Length(1),
        ])
        .areas(main);

        frame.render_widget(Paragraph::new(self.header_line()), header);
        self.transcript_area = body;
        self.draw_transcript(frame, body);
        frame.render_widget(Paragraph::new(banner), banner_area);

        let focused = self.focus == Focus::Composer;
        let border = if focused {
            Style::new().fg(Color::Blue)
        } else {
            DIM
        };
        let title = if self
            .pending_request()
            .is_some_and(|(r, _)| r["kind"] == "user_input")
        {
            " Answer "
        } else {
            " Message "
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(border)
            .title(title);
        let inner = block.inner(composer_area);
        let visible = inner.height as usize;
        let first = (cursor.0 + 1).saturating_sub(visible);
        let lines: Vec<Line> = composer_rows
            .iter()
            .skip(first)
            .take(visible)
            .map(|r| Line::raw(format!(" {r}")))
            .collect();
        frame.render_widget(Paragraph::new(lines).block(block), composer_area);
        if focused {
            frame.set_cursor_position((
                inner.x + 1 + cursor.1 as u16,
                inner.y + (cursor.0 - first) as u16,
            ));
        }
        frame.render_widget(Paragraph::new(self.status_line()), status_area);
    }

    fn draw_sidebar(&mut self, frame: &mut Frame, area: Rect) {
        let focused = self.focus == Focus::Sidebar;
        let block = Block::default()
            .borders(Borders::RIGHT)
            .border_style(if focused {
                Style::new().fg(Color::Blue)
            } else {
                DIM
            });
        let inner = block.inner(area);
        frame.render_widget(block, area);
        self.sidebar_area = inner;
        let height = inner.height as usize;
        if self.selected < self.sidebar_offset {
            self.sidebar_offset = self.selected.saturating_sub(1);
        } else if self.selected >= self.sidebar_offset + height {
            self.sidebar_offset = self.selected + 1 - height;
        }
        let width = inner.width as usize;
        let open_id = self.open.as_ref().map(|o| o.id.as_str());
        let lines: Vec<Line> = self
            .rows
            .iter()
            .enumerate()
            .skip(self.sidebar_offset)
            .take(height)
            .map(|(index, row)| match row {
                Row::Project(title) => {
                    Line::styled(fit(title, width), Style::new().add_modifier(Modifier::BOLD))
                }
                Row::Thread(id) => {
                    let thread = self.shell_thread(id);
                    let title = thread.map(|t| str_of(t, "title")).unwrap_or("(untitled)");
                    let (glyph, color) = thread_glyph(thread);
                    let mut style = Style::new();
                    if index == self.selected {
                        style = style.add_modifier(Modifier::REVERSED);
                    }
                    if Some(id.as_str()) == open_id {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    Line::from(vec![
                        Span::styled(format!(" {glyph} "), Style::new().fg(color)),
                        Span::styled(fit(title, width.saturating_sub(3)), style),
                    ])
                }
            })
            .collect();
        if lines.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::styled(" Loading threads…", DIM)),
                inner,
            );
        } else {
            frame.render_widget(Paragraph::new(lines), inner);
        }
    }

    fn header_line(&self) -> Line<'static> {
        let Some(open) = self.open.as_ref() else {
            return Line::styled(" t3term · pick a thread", DIM);
        };
        let Some(state) = open.state.as_ref() else {
            return Line::styled(" Loading…", DIM);
        };
        let thread = state.thread();
        let run_status = state.active_run().map(status).unwrap_or("idle").to_string();
        let model = thread["modelSelection"]["model"]
            .as_str()
            .unwrap_or("")
            .to_string();
        Line::from(vec![
            Span::styled(
                format!(" {} ", str_of(thread, "title")),
                Style::new().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    "· {run_status} · {model} · {}",
                    str_of(thread, "runtimeMode")
                ),
                if is_active_status(&run_status) {
                    Style::new().fg(Color::Yellow)
                } else {
                    DIM
                },
            ),
        ])
    }

    fn draw_transcript(&mut self, frame: &mut Frame, area: Rect) {
        let Some(state) = self.open.as_ref().and_then(|o| o.state.as_ref()) else {
            let hint = if self.open.is_some() {
                " Loading thread…"
            } else {
                " Select a thread with ↑/↓ and press Enter."
            };
            frame.render_widget(Paragraph::new(Line::styled(hint, DIM)), area);
            return;
        };
        let width = area.width.saturating_sub(1).max(10);
        let blocks = transcript::blocks(state);
        let expand_tools = self.expand_tools;
        // Each block keeps its wrapped lines until its content or the width changes. Only a
        // streaming block is rewrapped per frame.
        let mut heights = Vec::with_capacity(blocks.len());
        for block in &blocks {
            let key = (
                hash(&[
                    &block.header,
                    &block.body,
                    &block.status,
                    if expand_tools { "1" } else { "0" },
                ]),
                width,
            );
            let fresh = self.cache.get(&block.item_id).is_none_or(|c| c.key != key);
            if fresh {
                let lines = render_block(block, width as usize, expand_tools);
                self.cache
                    .insert(block.item_id.clone(), Cached { key, lines });
            }
            heights.push(self.cache[&block.item_id].lines.len());
        }
        let total: usize = heights.iter().sum();
        let height = area.height as usize;
        self.scroll = self.scroll.min(total.saturating_sub(height));
        let end = total - self.scroll;
        let start = end.saturating_sub(height);

        // Copy only the rows in view.
        let mut lines = Vec::with_capacity(height);
        let mut offset = 0;
        for (block, block_height) in blocks.iter().zip(&heights) {
            if offset + block_height > start && offset < end {
                let cached = &self.cache[&block.item_id].lines;
                let from = start.saturating_sub(offset);
                let to = (end - offset).min(*block_height);
                lines.extend(cached[from..to].iter().cloned());
            }
            offset += block_height;
            if offset >= end {
                break;
            }
        }
        frame.render_widget(Paragraph::new(lines), area);
        if self.scroll > 0 {
            let label = format!(" ↓ {} more ", self.scroll);
            let x = area.x + area.width.saturating_sub(label.width() as u16 + 1);
            frame.render_widget(
                Paragraph::new(Line::styled(
                    label.clone(),
                    Style::new().fg(Color::Black).bg(Color::Yellow),
                )),
                Rect::new(x, area.y + area.height - 1, label.width() as u16, 1),
            );
        }
    }

    fn request_banner(&self, width: usize) -> Vec<Line<'static>> {
        let Some((request, item)) = self.pending_request() else {
            return Vec::new();
        };
        let accent = Style::new().fg(Color::Magenta).add_modifier(Modifier::BOLD);
        let item = item.unwrap_or(Value::Null);
        if request["kind"] == "user_input" {
            let answered = self.open.as_ref().map_or(0, |o| o.answers.len());
            let questions = item["questions"].as_array().cloned().unwrap_or_default();
            let Some(question) = questions.get(answered) else {
                return Vec::new();
            };
            let mut lines = vec![Line::styled(
                fit(
                    &format!(
                        " ? {} ({}/{})",
                        str_of(question, "question"),
                        answered + 1,
                        questions.len()
                    ),
                    width,
                ),
                accent,
            )];
            for (index, option) in question["options"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
                .take(6)
            {
                lines.push(Line::raw(fit(
                    &format!(
                        "   {}. {} — {}",
                        index + 1,
                        str_of(option, "label"),
                        str_of(option, "description")
                    ),
                    width,
                )));
            }
            lines.push(Line::styled(
                "   Type a number or your own answer, then Enter.",
                DIM,
            ));
            return lines;
        }
        let prompt = str_of(&item, "prompt");
        vec![
            Line::styled(
                fit(
                    &format!(
                        " ! Approval needed ({}): {prompt}",
                        str_of(&request, "kind")
                    ),
                    width,
                ),
                accent,
            ),
            Line::styled(
                "   Alt+A accept · Alt+S accept for session · Alt+D decline",
                DIM,
            ),
        ]
    }

    fn status_line(&self) -> Line<'static> {
        if let Some((text, error)) = &self.message {
            let style = if *error {
                Style::new().fg(Color::Red)
            } else {
                Style::new().fg(Color::Green)
            };
            return Line::styled(format!(" {text}"), style);
        }
        let connection = match self.open.as_ref() {
            Some(open) => open.connection.clone(),
            None => self.shell_connection.clone(),
        };
        let keys = match self.focus {
            Focus::Sidebar => "↑↓ select · Enter open · Tab focus · q quit",
            Focus::Composer => {
                "Enter send · Alt+Enter newline · Esc transcript · Ctrl+X interrupt · Ctrl+C quit"
            }
            Focus::Transcript => {
                "↑↓/PgUp scroll · G bottom · t tool output · Enter compose · Esc sidebar"
            }
        };
        Line::styled(format!(" {connection} · {keys}"), DIM)
    }
}

fn thread_glyph(thread: Option<&Value>) -> (&'static str, Color) {
    let Some(thread) = thread else {
        return ("·", Color::DarkGray);
    };
    if !thread["pendingRuntimeRequest"].is_null() {
        return ("!", Color::Magenta);
    }
    match str_of(thread, "status") {
        s if is_active_status(s) => ("●", Color::Yellow),
        "failed" => ("×", Color::Red),
        "queued" => ("○", Color::Yellow),
        _ => ("·", Color::DarkGray),
    }
}

fn fit(text: &str, width: usize) -> String {
    let text = text.replace('\n', " ");
    if text.width() <= width {
        return text;
    }
    let mut out = String::new();
    for c in text.chars() {
        if out.width() + 2 > width {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

fn render_block(block: &transcript::Block, width: usize, expand_tools: bool) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match block.kind {
        BlockKind::User => {
            lines.push(Line::styled(
                "You",
                Style::new().fg(Color::Blue).add_modifier(Modifier::BOLD),
            ));
            lines.extend(markdown::render(&block.body, width, Style::new(), 2));
        }
        BlockKind::Assistant => {
            lines.extend(markdown::render(&block.body, width, Style::new(), 0));
            if block.streaming {
                lines.push(Line::styled("▍", Style::new().fg(Color::Yellow)));
            }
        }
        BlockKind::Reasoning => {
            let first = block
                .body
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or_default();
            let style = DIM.add_modifier(Modifier::ITALIC);
            lines.push(Line::styled(
                fit(&format!("Thinking: {}", first.trim_matches('*')), width),
                style,
            ));
        }
        BlockKind::Tool => {
            let color = match block.status.as_str() {
                "running" | "pending" => Color::Yellow,
                "failed" | "error" => Color::Red,
                _ => Color::DarkGray,
            };
            lines.push(Line::from(vec![
                Span::styled("⏺ ", Style::new().fg(color)),
                Span::styled(fit(&block.header, width - 2), DIM),
            ]));
            if expand_tools && !block.body.is_empty() {
                for line in block.body.lines() {
                    lines.push(Line::styled(fit(&format!("  │ {line}"), width), DIM));
                }
            }
        }
        BlockKind::Request => {
            lines.push(Line::styled(
                fit(&block.header, width),
                Style::new().fg(Color::Magenta),
            ));
            lines.extend(markdown::render(&block.body, width, DIM, 2));
        }
        BlockKind::Plan => {
            lines.push(Line::styled(
                block.header.clone(),
                Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
            ));
            lines.extend(markdown::render(&block.body, width, Style::new(), 2));
        }
        BlockKind::Notice => lines.push(Line::styled(
            fit(&format!("— {}", block.header), width),
            DIM,
        )),
        BlockKind::Error => {
            lines.push(Line::styled(
                "Error",
                Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
            ));
            lines.extend(markdown::render(
                &block.body,
                width,
                Style::new().fg(Color::Red),
                2,
            ));
        }
    }
    // Tool rows stack tightly; everything else gets a blank line after it.
    if block.kind != BlockKind::Tool {
        lines.push(Line::default());
    }
    lines
}
