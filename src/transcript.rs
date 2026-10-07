//! Turns projection items into display blocks. The CLI prints them; the TUI styles them.

use serde_json::Value;

use crate::projection::ThreadState;

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
}

fn str_of<'a>(item: &'a Value, key: &str) -> &'a str {
    item.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn truncate_lines(text: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= max_lines {
        return text.trim_end().to_string();
    }
    let hidden = lines.len() - max_lines;
    format!(
        "{}\n… {hidden} more lines",
        lines[lines.len() - max_lines..].join("\n")
    )
}

pub fn describe(item: &Value) -> Option<Block> {
    let item_type = str_of(item, "type");
    let title = str_of(item, "title");
    let block = |kind, header: String, body: String| Block {
        item_id: str_of(item, "id").to_string(),
        kind,
        header,
        body,
        streaming: item
            .get("streaming")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        status: str_of(item, "status").to_string(),
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
            let steps = item
                .get("steps")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let body = steps
                .iter()
                .map(|step| {
                    let mark = match str_of(step, "status") {
                        "completed" => "[x]",
                        "inProgress" | "in_progress" => "[>]",
                        _ => "[ ]",
                    };
                    format!("{mark} {}", str_of(step, "step"))
                })
                .collect::<Vec<_>>()
                .join("\n");
            block(BlockKind::Plan, "Plan".into(), body)
        }
        "command_execution" => {
            let mut header = format!(
                "$ {}",
                str_of(item, "input").lines().next().unwrap_or_default()
            );
            if let Some(code) = item.get("exitCode").and_then(Value::as_i64) {
                header.push_str(&format!("  (exit {code})"));
            }
            block(
                BlockKind::Tool,
                header,
                truncate_lines(str_of(item, "output"), 12),
            )
        }
        "file_change" => {
            let mut header = format!("edit {}", str_of(item, "fileName"));
            let additions = item.get("additions").and_then(Value::as_u64);
            let deletions = item.get("deletions").and_then(Value::as_u64);
            if additions.is_some() || deletions.is_some() {
                header.push_str(&format!(
                    "  +{} -{}",
                    additions.unwrap_or(0),
                    deletions.unwrap_or(0)
                ));
            }
            block(BlockKind::Tool, header, String::new())
        }
        "file_search" => block(
            BlockKind::Tool,
            format!("search {}", str_of(item, "pattern")),
            String::new(),
        ),
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
            block(
                BlockKind::Tool,
                format!("web search {joined}"),
                String::new(),
            )
        }
        "dynamic_tool" | "subagent" => {
            let label = if title.is_empty() { item_type } else { title };
            block(BlockKind::Tool, label.to_string(), String::new())
        }
        "approval_request" => {
            let kind = str_of(item, "requestKind");
            let prompt = str_of(item, "prompt");
            block(
                BlockKind::Request,
                format!("Approval requested: {kind}"),
                prompt.to_string(),
            )
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
