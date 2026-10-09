//! Turns projection items into display blocks. The CLI prints them; the TUI styles them.

use serde_json::Value;

use crate::projection::ThreadState;

/// Lines of a tool's output one row shows. Enough to see what happened, little enough that a
/// long one doesn't bury the turn around it.
const MAX_OUTPUT_LINES: usize = 12;
/// What one row's output can weigh, since a single line has no length of its own to bound.
const MAX_OUTPUT_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    User,
    Assistant,
    Reasoning,
    Tool,
    Request,
    Plan,
    Notice,
    Error,
}

#[derive(Debug, Clone)]
pub struct Block {
    pub item_id: String,
    pub kind: BlockKind,
    /// A short label, such as the command or file name.
    pub header: String,
    pub body: String,
    pub streaming: bool,
    pub status: String,
    /// The projection item type, such as `command_execution`, for picking an icon.
    pub item_type: String,
    /// The header without decoration: the command, file name, pattern or prompt.
    pub detail: String,
    /// T3's own one-line description of the item, when it has one.
    pub title: String,
    /// When T3 kept the tool's output out of the projection. It is fetched on demand.
    pub output_omitted: bool,
    /// The item's `updatedAt`, which identifies this version of it.
    pub updated_at: String,
    pub exit_code: Option<i64>,
    pub run_id: String,
    /// For `dynamic_tool` items: the provider's tool name, such as `Read`.
    pub tool_name: String,
    /// For request items: the runtime request they show.
    pub request_id: String,
}

fn str_of<'a>(item: &'a Value, key: &str) -> &'a str {
    item.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// Where one step of an agent's checklist stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Pending,
    Running,
    Completed,
}

/// One step of a `todo_list`, the checklist an agent keeps while it works.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskStep {
    pub text: String,
    pub status: StepStatus,
    /// How long the step took. T3 records it when a step completes, and only when it is more
    /// than zero.
    pub duration_ms: Option<f64>,
}

/// The steps of a `todo_list` plan or the turn item that shows it. Both carry V2 plan steps,
/// `{id, text, status: "pending" | "running" | "completed", durationMs?}`
/// (`OrchestrationV2PlanStep` in the nightly's `packages/contracts/src/orchestrationV2.ts`).
/// A status this build doesn't know reads as pending. An agent writes each step's text, so the
/// text comes without its control characters. The list itself, which `--json` prints, keeps
/// them.
pub fn task_steps(list: &Value) -> Vec<TaskStep> {
    list.get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|step| TaskStep {
            text: without_controls(str_of(step, "text")),
            status: match str_of(step, "status") {
                "completed" => StepStatus::Completed,
                "running" => StepStatus::Running,
                _ => StepStatus::Pending,
            },
            duration_ms: step.get("durationMs").and_then(Value::as_f64),
        })
        .collect()
}

/// `text` as it is safe to print. A terminal acts on a control character rather than show it:
/// ESC and the C1 CSI and OSC start sequences that clear the screen, move the cursor or set
/// the clipboard, and a carriage return lets the text after it overwrite the line. So a line
/// break stays, `\r\n` and a lone `\r` each read as one, a tab or other control that spaces
/// text reads as a space, and every other control, C0, DEL or C1, is dropped. What a sequence
/// leaves behind, such as `[2J`, is plain text. Combining marks, joiners and variation
/// selectors aren't controls, so they stay.
fn without_controls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' => out.push('\n'),
            '\r' if chars.peek() == Some(&'\n') => {}
            '\r' => out.push('\n'),
            c if c.is_control() && c.is_whitespace() => out.push(' '),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// The one value a tool call is about, for a transcript row: the file, pattern or query it
/// names. Tools differ, so this tries the keys they agree on before falling back to the whole
/// input.
fn tool_argument(item: &Value) -> String {
    const KEYS: [&str; 10] = [
        "file_path",
        "filePath",
        "path",
        "command",
        "pattern",
        "query",
        "url",
        "skill",
        "name",
        "prompt",
    ];
    match item.get("input") {
        Some(Value::String(text)) => text.lines().next().unwrap_or_default().to_string(),
        Some(Value::Object(input)) => {
            if let Some(text) = KEYS
                .iter()
                .find_map(|key| input.get(*key).and_then(Value::as_str))
            {
                return text.lines().next().unwrap_or_default().to_string();
            }
            // An unfamiliar tool: show its arguments as they came, on one line.
            match input.len() {
                0 => String::new(),
                _ => serde_json::to_string(input).unwrap_or_default(),
            }
        }
        _ => String::new(),
    }
}

/// What a tool printed, as text for the transcript. T3 keeps a tool's output out of the
/// projection so a large result can't stall the socket, and hands it over one item at a time,
/// in whatever shape the tool returned it.
pub fn tool_output(item: &Value) -> String {
    let lines = match item.get("output") {
        None | Some(Value::Null) => return String::new(),
        // A command prints its result at the end, so a long one keeps its tail. Anything
        // else, such as a file a reader returned, starts at the top.
        Some(value @ Value::String(_)) => {
            truncate_lines(output_text(value).trim_end(), MAX_OUTPUT_LINES)
        }
        Some(value) => head_lines(output_text(value).trim_end(), MAX_OUTPUT_LINES),
    };
    // A line has no length limit of its own: one long enough to matter is a record printed
    // as a single line. The row can only show a screen's worth of it anyway.
    truncate_bytes(lines, MAX_OUTPUT_BYTES)
}

/// The text cut to a byte budget, on a character boundary.
fn truncate_bytes(text: String, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// The text inside a tool's result. Tools answer with a string, with the content blocks an
/// MCP tool returns, or with a record such as a reader's `{"file": {"content": …}}`, so this
/// follows the keys that hold text and prints anything else as it came.
fn output_text(value: &Value) -> String {
    const KEYS: [&str; 6] = ["text", "content", "stdout", "output", "result", "file"];
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items.iter().map(output_text).collect::<Vec<_>>().join("\n"),
        Value::Object(fields) => {
            for key in KEYS {
                match fields.get(key) {
                    Some(Value::String(text)) => return text.clone(),
                    Some(nested @ (Value::Array(_) | Value::Object(_))) => {
                        return output_text(nested);
                    }
                    _ => {}
                }
            }
            serde_json::to_string(value).unwrap_or_default()
        }
        other => other.to_string(),
    }
}

/// The first lines of a long text, with a note where the rest was.
fn head_lines(text: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max_lines {
        return text.trim_end().to_string();
    }
    let hidden = lines.len() - max_lines;
    format!("{}\n… {hidden} more lines", lines[..max_lines].join("\n"))
}

fn truncate_lines(text: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max_lines {
        return text.trim_end().to_string();
    }
    let hidden = lines.len() - max_lines;
    // The tail is what a command's reader wants, so the note goes above the kept lines,
    // where the dropped ones were.
    format!(
        "… {hidden} earlier lines\n{}",
        lines[lines.len() - max_lines..].join("\n")
    )
}

pub fn describe(item: &Value) -> Option<Block> {
    let item_type = str_of(item, "type");
    let title = str_of(item, "title");
    let block = |kind, header: String, body: String| Block {
        item_id: str_of(item, "id").to_string(),
        kind,
        detail: header.clone(),
        title: title.to_string(),
        output_omitted: item
            .get("outputOmitted")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        updated_at: str_of(item, "updatedAt").to_string(),
        header,
        body,
        streaming: item
            .get("streaming")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        status: str_of(item, "status").to_string(),
        item_type: item_type.to_string(),
        exit_code: item.get("exitCode").and_then(Value::as_i64),
        run_id: str_of(item, "runId").to_string(),
        tool_name: str_of(item, "toolName").to_string(),
        request_id: str_of(item, "requestId").to_string(),
    };
    Some(match item_type {
        "user_message" => block(
            BlockKind::User,
            "You".into(),
            str_of(item, "text").to_string(),
        ),
        "assistant_message" => block(
            BlockKind::Assistant,
            "Assistant".into(),
            str_of(item, "text").to_string(),
        ),
        "reasoning" => {
            let text = str_of(item, "text");
            if text.trim().is_empty() {
                return None;
            }
            block(BlockKind::Reasoning, "Thinking".into(), text.to_string())
        }
        "proposed_plan" => block(
            BlockKind::Plan,
            "Proposed plan".into(),
            str_of(item, "markdown").to_string(),
        ),
        "todo_list" => {
            let body = task_steps(item)
                .iter()
                .map(|step| {
                    let mark = match step.status {
                        StepStatus::Completed => "[x]",
                        StepStatus::Running => "[>]",
                        StepStatus::Pending => "[ ]",
                    };
                    format!("{mark} {}", step.text)
                })
                .collect::<Vec<_>>()
                .join("\n");
            block(BlockKind::Plan, "Plan".into(), body)
        }
        "command_execution" => {
            let command = str_of(item, "input")
                .lines()
                .next()
                .unwrap_or_default()
                .to_string();
            let mut header = format!("$ {command}");
            if let Some(code) = item.get("exitCode").and_then(Value::as_i64) {
                header.push_str(&format!("  (exit {code})"));
            }
            Block {
                detail: command,
                ..block(
                    BlockKind::Tool,
                    header,
                    truncate_lines(str_of(item, "output"), MAX_OUTPUT_LINES),
                )
            }
        }
        "file_change" => {
            let name = str_of(item, "fileName").to_string();
            let mut header = format!("edit {name}");
            let additions = item.get("additions").and_then(Value::as_u64);
            let deletions = item.get("deletions").and_then(Value::as_u64);
            if additions.is_some() || deletions.is_some() {
                header.push_str(&format!(
                    "  +{} -{}",
                    additions.unwrap_or(0),
                    deletions.unwrap_or(0)
                ));
            }
            Block {
                detail: name,
                ..block(BlockKind::Tool, header, String::new())
            }
        }
        "file_search" => Block {
            detail: str_of(item, "pattern").to_string(),
            ..block(
                BlockKind::Tool,
                format!("search {}", str_of(item, "pattern")),
                String::new(),
            )
        },
        "web_search" => {
            let patterns = item
                .get("patterns")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let joined = patterns
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            Block {
                detail: joined.clone(),
                ..block(
                    BlockKind::Tool,
                    format!("web search {joined}"),
                    String::new(),
                )
            }
        }
        "dynamic_tool" | "subagent" => {
            let label = if title.is_empty() { item_type } else { title };
            Block {
                detail: tool_argument(item),
                ..block(BlockKind::Tool, label.to_string(), String::new())
            }
        }
        "approval_request" => {
            let kind = str_of(item, "requestKind");
            let prompt = str_of(item, "prompt");
            Block {
                detail: prompt.to_string(),
                ..block(
                    BlockKind::Request,
                    format!("Approval requested: {kind}"),
                    prompt.to_string(),
                )
            }
        }
        "user_input_request" => {
            let questions = item
                .get("questions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let body = questions
                .iter()
                .map(|q| str_of(q, "question").to_string())
                .collect::<Vec<_>>()
                .join("\n");
            block(BlockKind::Request, "Question".into(), body)
        }
        "error" => {
            let message = item
                .get("failure")
                .and_then(|f| f.get("message"))
                .and_then(Value::as_str)
                .unwrap_or(title);
            block(BlockKind::Error, "Error".into(), message.to_string())
        }
        "run_interrupt_result" => block(BlockKind::Notice, "Interrupted".into(), String::new()),
        "system_notice" | "compaction" | "handoff" | "fork" | "thread_created" | "notification" => {
            if title.is_empty() {
                return None;
            }
            block(BlockKind::Notice, title.to_string(), String::new())
        }
        // Checkpoints, interrupt requests and secret requests carry nothing worth a line.
        "checkpoint" | "run_interrupt_request" | "secret_request" => return None,
        _ => {
            if title.is_empty() {
                return None;
            }
            block(BlockKind::Tool, title.to_string(), String::new())
        }
    })
}

pub fn blocks(state: &ThreadState) -> Vec<Block> {
    state.items().into_iter().filter_map(describe).collect()
}

/// Plain text for `threads read`.
pub fn plain_text(state: &ThreadState, last: Option<usize>, include_reasoning: bool) -> String {
    let all: Vec<Block> = blocks(state)
        .into_iter()
        .filter(|b| include_reasoning || b.kind != BlockKind::Reasoning)
        .collect();
    let start = last.map_or(0, |n| all.len().saturating_sub(n));
    let mut out = String::new();
    for block in &all[start..] {
        let marker = match block.kind {
            BlockKind::User => "## You",
            BlockKind::Assistant => "## Assistant",
            _ => "",
        };
        if marker.is_empty() {
            out.push_str(&format!("  · {}\n", block.header));
            if !block.body.is_empty() && block.kind != BlockKind::Tool {
                for line in block.body.lines() {
                    out.push_str(&format!("    {line}\n"));
                }
            }
        } else {
            out.push_str(&format!("\n{marker}\n\n{}\n", block.body.trim_end()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_tools_output_reads_as_text_whatever_shape_it_arrives_in() {
        // A command's output is a string.
        assert_eq!(tool_output(&json!({"output": "one\ntwo"})), "one\ntwo");
        // A reader answers with the file it read.
        let read = json!({"output": {
            "type": "text",
            "file": { "filePath": "/tmp/a.py", "content": "import sys\n" },
        }});
        assert_eq!(tool_output(&read), "import sys");
        // An MCP tool answers with content blocks.
        let blocks = json!({"output": [
            { "type": "text", "text": "first" },
            { "type": "text", "text": "second" },
        ]});
        assert_eq!(tool_output(&blocks), "first\nsecond");
        // A record holding no text is shown as it came.
        assert_eq!(
            tool_output(&json!({"output": {"success": true}})),
            r#"{"success":true}"#
        );
        // An item whose output T3 has not handed over yet.
        assert_eq!(tool_output(&json!({"status": "completed"})), "");
    }

    #[test]
    fn a_checklist_shows_each_steps_text_and_marks_the_running_one() {
        // A nightly checklist, as T3 sends it in a `todo_list` item and its plan artifact.
        let item = json!({
            "id": "todo-1",
            "type": "todo_list",
            "planId": "plan-1",
            "steps": [
                {"id": "s1", "text": "Read the log", "status": "completed", "durationMs": 1200},
                {
                    "id": "s2",
                    "text": "Patch the parser",
                    "status": "running",
                    "durationAnchorAt": "2026-10-08T10:00:00.000Z",
                },
                {"id": "s3", "text": "Run the tests", "status": "pending"},
            ],
        });
        let steps = task_steps(&item);
        let statuses: Vec<StepStatus> = steps.iter().map(|step| step.status).collect();
        assert_eq!(
            statuses,
            [
                StepStatus::Completed,
                StepStatus::Running,
                StepStatus::Pending
            ]
        );
        assert_eq!(steps[0].duration_ms, Some(1200.0));
        // A running step has only the anchor its clock started from.
        assert_eq!(steps[1].duration_ms, None);

        let block = describe(&item).expect("a checklist has a row");
        assert_eq!(block.kind, BlockKind::Plan);
        assert_eq!(
            block.body,
            "[x] Read the log\n[>] Patch the parser\n[ ] Run the tests"
        );

        // `threads read` prints the same rows.
        let state = ThreadState::from_snapshot(&json!({
            "snapshotSequence": 1,
            "projection": {"thread": {"id": "t"}, "turnItems": [item.clone()]},
        }))
        .expect("a snapshot");
        assert_eq!(
            plain_text(&state, None, false),
            "  · Plan\n    [x] Read the log\n    [>] Patch the parser\n    [ ] Run the tests\n"
        );
    }

    #[test]
    fn a_step_in_a_state_this_build_doesnt_know_reads_as_pending() {
        let item = json!({"steps": [{"id": "s1", "text": "Wait", "status": "blocked"}]});
        assert_eq!(task_steps(&item)[0].status, StepStatus::Pending);
        // A list with no steps has no rows.
        assert!(task_steps(&json!({"type": "todo_list"})).is_empty());
        assert!(task_steps(&json!({"steps": null})).is_empty());
    }

    #[test]
    fn a_steps_control_characters_never_reach_the_terminal() {
        // Made-up payloads: a CSI that clears the screen and homes the cursor, an OSC 52
        // clipboard write ended by BEL and another ended by ST, the C1 forms of CSI, OSC and
        // ST, a backspace and DEL, a carriage return that would write over the line, and the
        // tab, VT, FF and NEL that space text.
        let raw = [
            "Clear\u{1b}[2J\u{1b}[Hthe screen",
            "Copy\u{1b}]52;c;Zm9v\u{7} and\u{1b}]52;c;YmFy\u{1b}\\ text",
            "C1\u{9b}31m csi and \u{9d}0;title\u{9c} osc",
            "Fix\u{8}\u{7f}ed\rDone\r\nnext\tline\u{b}and\u{c}more\u{85}end",
            // Text a terminal draws as it is: a joined emoji, an emoji with U+FE0F, a combining
            // accent and a skin tone.
            "Ship 👩\u{200d}💻 ⚠\u{fe0f} cafe\u{301} 👍\u{1f3fd}",
        ];
        let steps: Vec<Value> = raw
            .iter()
            .enumerate()
            .map(|(n, text)| json!({"id": n, "text": text, "status": "pending"}))
            .collect();
        let item = json!({"id": "todo-1", "type": "todo_list", "ordinal": 1, "steps": steps});
        let texts: Vec<String> = task_steps(&item).into_iter().map(|s| s.text).collect();
        assert_eq!(
            texts,
            [
                "Clear[2J[Hthe screen",
                "Copy]52;c;Zm9v and]52;c;YmFy\\ text",
                "C131m csi and 0;title osc",
                "Fixed\nDone\nnext line and more end",
                raw[4],
            ]
        );

        // The transcript row and `t3term read` print the cleaned text, and the projection that
        // `--json` prints keeps the text as T3 sent it.
        let state = ThreadState::from_snapshot(&json!({
            "snapshotSequence": 1,
            "projection": {"thread": {"id": "t"}, "turnItems": [item]},
        }))
        .expect("a snapshot");
        let block = describe(&state.items()[0]).expect("a checklist has a row");
        let printed = plain_text(&state, None, false);
        for text in [&block.body, &printed] {
            let control = text.chars().find(|c| c.is_control() && *c != '\n');
            assert_eq!(control, None, "{text:?}");
        }
        assert!(printed.contains("    [ ] Fixed\n    Done\n    next line"), "{printed:?}");
        let kept: Vec<&str> = state.list("turnItems")[0]["steps"]
            .as_array()
            .expect("steps")
            .iter()
            .filter_map(|step| step["text"].as_str())
            .collect();
        assert_eq!(kept, raw);
    }

    #[test]
    fn a_long_output_says_what_it_dropped_and_keeps_the_end_that_matters() {
        let lines = (1..=30)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        // A command's last lines are its result, so those are the ones kept.
        let command = tool_output(&json!({ "output": lines.clone() }));
        assert!(
            command.starts_with("… 18 earlier lines\n19\n20\n"),
            "{command}"
        );
        assert!(command.ends_with("\n30"), "{command}");
        // A file a reader returned is read from the top.
        let file = tool_output(&json!({"output": {"file": {"content": lines}}}));
        assert!(file.starts_with("1\n2\n"), "{file}");
        assert!(file.ends_with("\n12\n… 18 more lines"), "{file}");
    }
}
