//! Turns projection items into display blocks. The CLI prints them; the TUI styles them.

use serde_json::Value;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::projection::ThreadState;

/// Lines of a tool's output one row shows. Enough to see what happened, little enough that a
/// long one doesn't bury the turn around it.
const MAX_OUTPUT_LINES: usize = 12;
/// What one row's output can weigh, since a single line has no length of its own to bound.
const MAX_OUTPUT_BYTES: usize = 4096;
/// Sources a handoff names before it counts the rest. T3 lists one for each provider and model
/// the handed-off runs used, so a real handoff has far fewer.
const MAX_HANDOFF_SOURCES: usize = 12;
/// Columns one end of a handoff can take. Model and provider ids are far shorter.
const MAX_ENDPOINT_WIDTH: usize = 64;
/// Bytes of a model or provider id that are read at all, enough for `MAX_ENDPOINT_WIDTH`
/// columns in any script.
const MAX_ENDPOINT_BYTES: usize = 1024;

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

/// `text` cut where a terminal starts a new character: before each character with a width,
/// except a skin tone and the character after a zero-width joiner. So an accent stays on its
/// letter, U+FE0F on its emoji, and a joined emoji such as 👩‍💻 stays whole.
pub(crate) fn clusters(text: &str) -> Vec<&str> {
    let mut clusters = Vec::new();
    let mut start = 0;
    let mut joined = false;
    for (index, c) in text.char_indices() {
        let width = UnicodeWidthChar::width(c).unwrap_or(0);
        let skin_tone = matches!(c, '\u{1F3FB}'..='\u{1F3FF}');
        if index > 0 && width > 0 && !joined && !skin_tone {
            clusters.push(&text[start..index]);
            start = index;
        }
        joined = c == '\u{200D}';
    }
    clusters.push(&text[start..]);
    clusters
}

/// The marker and detail of a context compaction, the `compaction` turn item
/// (`OrchestrationV2TurnItem` in the nightly's `packages/contracts/src/orchestrationV2.ts`),
/// whose title T3 never shows. The label names its state as the nightly's `V2LifecycleRow.tsx`
/// does. The desktop's timeline row uses `contextCompactionLabel` from
/// `packages/client-runtime/src/work-log/presentation.ts` instead, which calls a failed or
/// pending compaction compacted. Its wording is kept for a compaction that finished with both
/// token counts, `Context compacted 899K → 19K tokens`. Any other count goes on the first
/// detail line, with `?` for the one T3 didn't send, and the summary follows.
fn compaction(item: &Value) -> (String, String) {
    let label = match str_of(item, "status") {
        "failed" => "Context compaction failed",
        "cancelled" | "interrupted" => "Context compaction stopped",
        "pending" | "running" | "waiting" => "Compacting context",
        _ => "Context compacted",
    };
    // T3 sends each count as a whole number of tokens, when the provider reported it.
    let count = |key: &str| item.get(key).and_then(Value::as_u64);
    let (before, after) = (count("beforeTokenCount"), count("afterTokenCount"));
    let tokens = || {
        let side = |known: Option<u64>| known.map_or_else(|| "?".to_string(), token_count);
        format!("{} → {} tokens", side(before), side(after))
    };
    let mut detail = Vec::new();
    let header = match (before, after) {
        (Some(_), Some(_)) if label == "Context compacted" => format!("{label} {}", tokens()),
        (None, None) => label.to_string(),
        _ => {
            detail.push(tokens());
            label.to_string()
        }
    };
    let summary = summary_text(str_of(item, "summary"));
    if !summary.is_empty() {
        detail.push(summary);
    }
    (header, detail.join("\n"))
}

/// A token count as T3 writes it, after `formatTokens` in the nightly's
/// `packages/shared/src/usageFormat.ts`: three significant figures and a K, M, B or T, with two
/// decimals below 10, one below 100 and none from 100 up. Decimals that are all zero are
/// dropped and others are kept, so 1,500 is `1.50K`. The rounding is JavaScript's, so 999,999
/// is `1000K`.
fn token_count(count: u64) -> String {
    const UNITS: [(u64, &str); 4] = [
        (1_000_000_000_000, "T"),
        (1_000_000_000, "B"),
        (1_000_000, "M"),
        (1_000, "K"),
    ];
    let Some((unit, suffix)) = UNITS.into_iter().find(|(unit, _)| count >= *unit) else {
        return count.to_string();
    };
    // T3 divides in doubles, as JavaScript does.
    let value = count as f64 / unit as f64;
    let digits = if value >= 100.0 {
        0
    } else if value >= 10.0 {
        1
    } else {
        2
    };
    let scale = 10u64.pow(digits);
    let rounded = to_fixed(value, digits);
    let (whole, fraction) = (rounded / scale, rounded % scale);
    match fraction {
        0 => format!("{whole}{suffix}"),
        _ => format!(
            "{whole}.{fraction:0width$}{suffix}",
            width = digits as usize
        ),
    }
}

/// JavaScript's `value.toFixed(digits)` for a value of at least 1, as a whole number of the
/// last digit's unit: the nearest to the double's exact value, and the larger of two as near.
/// Rust's `{:.1}` takes the even one, which would write 12,250 tokens as `12.2K` where T3
/// shows `12.3K`.
fn to_fixed(value: f64, digits: u32) -> u64 {
    // A double of at least 1 is a whole number of 2^-52, which 52 decimals write exactly.
    let exact = format!("{value:.52}");
    let (whole, decimals) = exact.split_once('.').unwrap_or((exact.as_str(), ""));
    let mut decimals = decimals.chars();
    let kept: String = whole
        .chars()
        .chain(decimals.by_ref().take(digits as usize))
        .collect();
    let round_up = decimals.next().is_some_and(|digit| digit >= '5');
    kept.parse::<u64>().unwrap_or(0) + u64::from(round_up)
}

/// A compaction's summary as the transcript shows it: its first lines with text in them,
/// without control characters, then a line of `…` when there was more. The cut to
/// `MAX_OUTPUT_BYTES` comes first, so a long summary is never scanned or copied whole.
fn summary_text(summary: &str) -> String {
    let mut end = summary.len().min(MAX_OUTPUT_BYTES);
    while !summary.is_char_boundary(end) {
        end -= 1;
    }
    let text = without_controls(&summary[..end]);
    let mut lines = text
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty());
    let mut shown: Vec<&str> = lines.by_ref().take(MAX_OUTPUT_LINES).collect();
    if end < summary.len() || lines.next().is_some() {
        shown.push("…");
    }
    shown.join("\n")
}

/// One end of a context handoff: a provider instance and, when the item or the thread's runs
/// name it, the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Endpoint<'a> {
    instance_id: &'a str,
    model: Option<&'a str>,
}

impl Endpoint<'_> {
    /// The endpoint as the transcript names it: its model, or its provider id when no model is
    /// known or the model has no text, or `?` when neither has any.
    fn label(&self) -> String {
        self.model
            .and_then(endpoint_text)
            .or_else(|| endpoint_text(self.instance_id))
            .unwrap_or_else(|| "?".into())
    }
}

/// The ends of a handoff that its detail line names.
struct Endpoints<'a> {
    /// The first `MAX_HANDOFF_SOURCES` sources, in T3's order.
    from: Vec<Endpoint<'a>>,
    /// How many sources came after those. None of them is resolved.
    more: usize,
    to: Endpoint<'a>,
}

/// The first `MAX_HANDOFF_SOURCES` of a handoff's raw sources, each made an endpoint by
/// `resolve`, and how many came after them. The cut comes before `resolve`, which never sees
/// the rest, so a handoff with thousands of sources looks up at most twelve models. The count
/// is the array's length less those, not a walk over the rest.
fn shown_sources<'a>(
    raw: &'a [Value],
    resolve: impl FnMut(&'a Value) -> Endpoint<'a>,
) -> (Vec<Endpoint<'a>>, usize) {
    let shown = &raw[..raw.len().min(MAX_HANDOFF_SOURCES)];
    (shown.iter().map(resolve).collect(), raw.len() - shown.len())
}

/// Where a `handoff` item took the context from and to, worked out as the nightly's
/// `resolveHandoffEndpoints` (`packages/client-runtime/src/handoff.ts`) does for the desktop
/// and mobile timelines. T3 stamps the source models on the item in `fromModelSelections`, in
/// order and several for one provider when its runs used several, and the target's in
/// `toModel`. An item from before it did has only provider ids. Then the target's model is the
/// handoff run's, when that run is on the target's provider, and each source's is the model of
/// the newest run on its provider that came before the handoff run. An endpoint with no model
/// keeps its provider. Only the sources the line shows are resolved, through `shown_sources`.
fn endpoints<'a>(item: &'a Value, runs: &'a [Value]) -> Endpoints<'a> {
    let handoff_run = item
        .get("runId")
        .and_then(Value::as_str)
        .and_then(|id| runs.iter().find(|run| str_of(run, "id") == id));
    let to_instance = str_of(item, "toProviderInstanceId");
    // A `toModel` with no text still wins over the run's, as `??` lets it in the nightly.
    let to_model = item.get("toModel").and_then(Value::as_str).or_else(|| {
        handoff_run
            .filter(|run| str_of(run, "providerInstanceId") == to_instance)
            .and_then(run_model)
    });
    let stamped = item
        .get("fromModelSelections")
        .and_then(Value::as_array)
        .filter(|selections| !selections.is_empty());
    let (from, more) = match stamped {
        Some(selections) => shown_sources(selections, |selection| Endpoint {
            instance_id: str_of(selection, "instanceId"),
            model: selection.get("model").and_then(Value::as_str),
        }),
        None => {
            let before = handoff_run
                .and_then(|run| run.get("ordinal"))
                .and_then(Value::as_u64);
            let ids = item
                .get("fromProviderInstanceIds")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            shown_sources(ids, |id| {
                let instance_id = id.as_str().unwrap_or_default();
                Endpoint {
                    instance_id,
                    model: latest_model_before(runs, instance_id, before),
                }
            })
        }
    };
    Endpoints {
        from,
        more,
        to: Endpoint {
            instance_id: to_instance,
            model: to_model,
        },
    }
}

fn run_model(run: &Value) -> Option<&str> {
    run.pointer("/modelSelection/model").and_then(Value::as_str)
}

/// The model of the newest run on `instance_id` with an ordinal below `before`, or of the
/// newest run on it when the handoff's own run isn't known. Of two runs with one ordinal the
/// first wins, as in the nightly's `latestRunModelBefore`.
fn latest_model_before<'a>(
    runs: &'a [Value],
    instance_id: &str,
    before: Option<u64>,
) -> Option<&'a str> {
    let mut latest: Option<(u64, &Value)> = None;
    for run in runs {
        if str_of(run, "providerInstanceId") != instance_id {
            continue;
        }
        let ordinal = run.get("ordinal").and_then(Value::as_u64).unwrap_or(0);
        if before.is_some_and(|before| ordinal >= before) {
            continue;
        }
        if latest.is_none_or(|(newest, _)| ordinal > newest) {
            latest = Some((ordinal, run));
        }
    }
    latest.and_then(|(_, run)| run_model(run))
}

/// The fields of a run that a handoff's endpoints read: its ordinal, its provider and its
/// model. A run's status and times change far more often, and they move no endpoint.
pub fn handoff_fields(run: &Value) -> (Option<u64>, Option<String>, Option<String>) {
    (
        run.get("ordinal").and_then(Value::as_u64),
        run.get("providerInstanceId")
            .and_then(Value::as_str)
            .map(str::to_string),
        run_model(run).map(str::to_string),
    )
}

/// Whether a turn item is a handoff whose endpoints read the thread's runs, because T3 stamped
/// no source models or no target model on it.
pub fn reads_runs(item: &Value) -> bool {
    if str_of(item, "type") != "handoff" {
        return false;
    }
    let stamped_from = item
        .get("fromModelSelections")
        .and_then(Value::as_array)
        .is_some_and(|selections| !selections.is_empty());
    let stamped_to = item.get("toModel").and_then(Value::as_str).is_some();
    !(stamped_from && stamped_to)
}

/// A handoff's detail line: its sources, then `→` and its target, in the order of the
/// nightly's `V2LifecycleRow.tsx`, which also drops the arrow when there is no source. Past
/// `MAX_HANDOFF_SOURCES` sources a count such as `+3 more` stands for the rest. The desktop
/// names an endpoint by its model's catalog name, and by its provider's display name when it
/// has no model, both from T3's provider list. `t3term read` doesn't fetch that list, so
/// t3term names both ends by the ids T3 sent.
fn handoff_detail(item: &Value, runs: &[Value]) -> String {
    let Endpoints { from, more, to } = endpoints(item, runs);
    let mut sources: Vec<String> = from.iter().map(Endpoint::label).collect();
    if more > 0 {
        sources.push(format!("+{more} more"));
    }
    if sources.is_empty() {
        return to.label();
    }
    format!("{} → {}", sources.join(", "), to.label())
}

/// A model or provider id as one line can show it: without control characters, each run of
/// blanks and line breaks read as one space, and cut with `…` past `MAX_ENDPOINT_WIDTH`
/// columns, at a place `clusters` finds. `None` when no text is left.
fn endpoint_text(raw: &str) -> Option<String> {
    // Only the start of a long id can show, so only that much is read.
    let mut end = raw.len().min(MAX_ENDPOINT_BYTES);
    while !raw.is_char_boundary(end) {
        end -= 1;
    }
    let text = without_controls(&raw[..end])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty() {
        return None;
    }
    let width: usize = clusters(&text).iter().map(|piece| piece.width()).sum();
    if end == raw.len() && width <= MAX_ENDPOINT_WIDTH {
        return Some(text);
    }
    // The … takes the last column.
    let mut cut = String::new();
    let mut used = 0;
    for piece in clusters(&text) {
        used += piece.width();
        if used >= MAX_ENDPOINT_WIDTH {
            break;
        }
        cut.push_str(piece);
    }
    Some(format!("{}…", cut.trim_end()))
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

/// The block for one turn item. `runs` are the runs of the item's thread, which
/// `ThreadState::runs_for` gives. A handoff that T3 stamped no models on reads its models from
/// them.
pub fn describe(item: &Value, runs: &[Value]) -> Option<Block> {
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
        "compaction" => {
            let (header, body) = compaction(item);
            block(BlockKind::Notice, header, body)
        }
        // The nightly's `V2LifecycleRow.tsx` labels every handoff this way, whatever its title,
        // and shows where it went rather than its summary.
        "handoff" => block(
            BlockKind::Notice,
            "Context handoff".into(),
            handoff_detail(item, runs),
        ),
        "system_notice" | "fork" | "thread_created" | "notification" => {
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
    state
        .items()
        .into_iter()
        .filter_map(|item| describe(item, state.runs_for(item)))
        .collect()
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

        let block = describe(&item, &[]).expect("a checklist has a row");
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
        let block = describe(state.items()[0], &[]).expect("a checklist has a row");
        let printed = plain_text(&state, None, false);
        for text in [&block.body, &printed] {
            let control = text.chars().find(|c| c.is_control() && *c != '\n');
            assert_eq!(control, None, "{text:?}");
        }
        assert!(
            printed.contains("    [ ] Fixed\n    Done\n    next line"),
            "{printed:?}"
        );
        let kept: Vec<&str> = state.list("turnItems")[0]["steps"]
            .as_array()
            .expect("steps")
            .iter()
            .filter_map(|step| step["text"].as_str())
            .collect();
        assert_eq!(kept, raw);
    }

    /// A `compaction` item as the nightly sends it (`OrchestrationV2TurnItem` in
    /// packages/contracts/src/orchestrationV2.ts), with no title and each count only when the
    /// provider reported one.
    fn compaction_item(status: &str, before: Option<Value>, after: Option<Value>) -> Value {
        let mut item = json!({
            "id": "compaction-1",
            "type": "compaction",
            "ordinal": 1,
            "status": status,
            "title": null,
            "driver": null,
            "updatedAt": "2026-10-08T10:00:00.000Z",
        });
        if let Some(count) = before {
            item["beforeTokenCount"] = count;
        }
        if let Some(count) = after {
            item["afterTokenCount"] = count;
        }
        item
    }

    #[test]
    fn a_compaction_names_its_state_and_counts_without_a_title() {
        // Status, counts before and after, then the marker and the detail under it. Counts
        // read as the nightly's `formatTokens` writes them, which node gave for these values.
        let cases = [
            ("pending", None, None, "Compacting context", ""),
            (
                "running",
                Some(json!(899_000)),
                None,
                "Compacting context",
                "899K → ? tokens",
            ),
            (
                "waiting",
                None,
                Some(json!(19_000)),
                "Compacting context",
                "? → 19K tokens",
            ),
            (
                "completed",
                Some(json!(899_000)),
                Some(json!(19_000)),
                "Context compacted 899K → 19K tokens",
                "",
            ),
            ("completed", None, None, "Context compacted", ""),
            (
                "completed",
                Some(json!(0)),
                Some(json!(999)),
                "Context compacted 0 → 999 tokens",
                "",
            ),
            // 12.25 is a tie, which JavaScript rounds up. 1.005 is a double just under
            // 1.005, and 999.999 rounds past 999. Decimals that aren't all zero stay.
            (
                "completed",
                Some(json!(12_250)),
                Some(json!(1_500)),
                "Context compacted 12.3K → 1.50K tokens",
                "",
            ),
            (
                "completed",
                Some(json!(999_999)),
                Some(json!(1_005)),
                "Context compacted 1000K → 1K tokens",
                "",
            ),
            (
                "completed",
                Some(json!(9_007_199_254_740_991_u64)),
                Some(json!(1_234_567)),
                "Context compacted 9007T → 1.23M tokens",
                "",
            ),
            // A count that isn't a whole number of tokens is one T3 didn't send.
            (
                "completed",
                Some(json!(-5)),
                Some(json!(19_000)),
                "Context compacted",
                "? → 19K tokens",
            ),
            (
                "failed",
                Some(json!(899_000)),
                Some(json!(19_000)),
                "Context compaction failed",
                "899K → 19K tokens",
            ),
            ("cancelled", None, None, "Context compaction stopped", ""),
            (
                "interrupted",
                Some(json!(999)),
                None,
                "Context compaction stopped",
                "999 → ? tokens",
            ),
            // A state the lifecycle row doesn't name reads as compacted, as it does there.
            ("idle", None, None, "Context compacted", ""),
        ];
        for (status, before, after, header, body) in cases {
            let item = compaction_item(status, before, after);
            let block = describe(&item, &[]).expect("a compaction has a row");
            assert_eq!(block.kind, BlockKind::Notice, "{item}");
            assert_eq!(
                (block.header.as_str(), block.body.as_str()),
                (header, body),
                "{item}"
            );
        }

        // A title the provider gave the item doesn't replace the state.
        let mut titled = compaction_item("running", None, None);
        titled["title"] = json!("Auto-compact");
        assert_eq!(
            describe(&titled, &[]).expect("a row").header,
            "Compacting context"
        );
    }

    #[test]
    fn a_compaction_summary_is_bounded_and_never_moves_the_cursor() {
        // A made-up summary: an Esc that clears the screen, a C1 CSI, BEL, DEL, a carriage
        // return, a tab and blank lines, then CJK, a joined emoji and a combining accent.
        let summary = "Kept\u{1b}[2J the plan\r\nDropped\u{9b}31m the\u{7} logs\tand\u{7f} notes\n\n \t\n日本語 👩\u{200d}💻 cafe\u{301}";
        let mut item = compaction_item("completed", Some(json!(899_000)), Some(json!(19_000)));
        item["summary"] = json!(summary);
        let state = ThreadState::from_snapshot(&json!({
            "snapshotSequence": 1,
            "projection": {"thread": {"id": "t"}, "turnItems": [item]},
        }))
        .expect("a snapshot");
        let block = describe(state.items()[0], &[]).expect("a compaction has a row");
        assert_eq!(block.header, "Context compacted 899K → 19K tokens");
        assert_eq!(
            block.body,
            "Kept[2J the plan\nDropped31m the logs and notes\n日本語 👩\u{200d}💻 cafe\u{301}"
        );
        // `t3term read` prints the marker and its detail, and `--json` prints the projection,
        // which keeps the summary as T3 sent it.
        assert_eq!(
            plain_text(&state, None, false),
            "  · Context compacted 899K → 19K tokens\n    Kept[2J the plan\n    Dropped31m the logs and notes\n    日本語 👩\u{200d}💻 cafe\u{301}\n"
        );
        assert_eq!(state.list("turnItems")[0]["summary"], summary);

        // Counts the marker leaves out come before the summary.
        let mut item = compaction_item("failed", Some(json!(899_000)), None);
        item["summary"] = json!("Ran out of room");
        assert_eq!(
            describe(&item, &[]).expect("a row").body,
            "899K → ? tokens\nRan out of room"
        );

        // Past twelve lines with text, a line of … says there was more.
        let lines = (1..=20)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut item = compaction_item("completed", None, None);
        item["summary"] = json!(lines);
        let body = describe(&item, &[]).expect("a row").body;
        assert!(body.starts_with("line 1\nline 2\n"), "{body}");
        assert!(body.ends_with("\nline 12\n…"), "{body}");

        // One line longer than a row's byte budget is cut on a character boundary. 界 takes
        // three bytes, so 1,365 of them fit in 4,096.
        item["summary"] = json!("界".repeat(5_000));
        let body = describe(&item, &[]).expect("a row").body;
        assert_eq!(body, format!("{}\n…", "界".repeat(1_365)));
    }

    /// A `handoff` item as the nightly sends it (`OrchestrationV2TurnItem` in
    /// packages/contracts/src/orchestrationV2.ts and `Orchestrator.ts:1636`), from codex_personal
    /// to claudeAgent in run `target`, without the stamped models of newer servers. `fields`
    /// replaces or adds fields.
    fn handoff_item(fields: Value) -> Value {
        let mut item = json!({
            "id": "handoff-1",
            "threadId": "t",
            "runId": "target",
            "type": "handoff",
            "ordinal": 299,
            "status": "completed",
            "title": "Provider handoff",
            "contextHandoffId": "context-handoff-1",
            "fromProviderThreadIds": ["provider-thread-1"],
            "toProviderThreadId": "provider-thread-2",
            "fromProviderInstanceIds": ["codex_personal"],
            "toProviderInstanceId": "claudeAgent",
            "strategy": "full_thread_summary",
            "summary": "Full conversation context for provider handoff.",
            "updatedAt": "2026-10-08T10:00:00.000Z",
        });
        if let (Some(item), Some(fields)) = (item.as_object_mut(), fields.as_object()) {
            item.extend(fields.clone());
        }
        item
    }

    /// A finished run on `instance` with `model`.
    fn run(id: &str, ordinal: u64, instance: &str, model: &str) -> Value {
        json!({"id": id, "ordinal": ordinal, "status": "completed", "providerInstanceId": instance,
            "modelSelection": {"instanceId": instance, "model": model}})
    }

    fn detail(item: &Value, runs: &[Value]) -> String {
        describe(item, runs).expect("a handoff has a row").body
    }

    #[test]
    fn every_handoff_is_a_context_handoff_marker_whatever_its_title() {
        // Untitled, and with the titles T3 gives a provider switch and imported context. None
        // shows its title or its summary.
        let stamped = json!({
            "fromModelSelections": [{"instanceId": "codex_personal", "model": "gpt-5.6-sol"}],
            "toModel": "claude-fable-5",
        });
        for title in [
            Value::Null,
            json!("Provider handoff"),
            json!("Imported context"),
        ] {
            let mut item = handoff_item(stamped.clone());
            item["title"] = title;
            let block = describe(&item, &[]).expect("a handoff has a row");
            assert_eq!(block.kind, BlockKind::Notice, "{item}");
            assert_eq!(
                (block.header.as_str(), block.body.as_str()),
                ("Context handoff", "gpt-5.6-sol → claude-fable-5"),
                "{item}"
            );
        }

        // `t3term read` prints each marker and its endpoints, a failed handoff's too. The
        // projection that `--json` prints keeps the items as T3 sent them.
        let mut untitled = handoff_item(stamped);
        untitled["title"] = Value::Null;
        let failed = handoff_item(json!({
            "id": "handoff-2",
            "ordinal": 399,
            "status": "failed",
            "runId": null,
        }));
        let state = ThreadState::from_snapshot(&json!({
            "snapshotSequence": 1,
            "projection": {"thread": {"id": "t"}, "turnItems": [untitled.clone(), failed.clone()]},
        }))
        .expect("a snapshot");
        assert_eq!(
            plain_text(&state, None, false),
            "  · Context handoff\n    gpt-5.6-sol → claude-fable-5\n  · Context handoff\n    codex_personal → claudeAgent\n"
        );
        assert_eq!(state.list("turnItems"), [untitled, failed]);
    }

    #[test]
    fn stamped_endpoints_keep_every_source_model_and_win_over_the_runs() {
        // As in the nightly's handoff.test.ts: two models from one provider, and a target that
        // the handoff run would name differently.
        let runs = [run("target", 2, "claudeAgent", "later-model")];
        let two_models = json!([
            {"instanceId": "codex_personal", "model": "source-a"},
            {"instanceId": "codex_personal", "model": "source-b"},
        ]);
        let item = handoff_item(json!({
            "fromModelSelections": two_models,
            "toModel": "destination",
        }));
        assert_eq!(detail(&item, &runs), "source-a, source-b → destination");

        // Sources keep T3's order, a repeat included, and one with a blank model is named by
        // its provider.
        let item = handoff_item(json!({
            "fromModelSelections": [
                {"instanceId": "cursor", "model": "composer-2"},
                {"instanceId": "codex_personal", "model": "source-a"},
                {"instanceId": "opencode", "model": " \t"},
                {"instanceId": "cursor", "model": "composer-2"},
            ],
            "toModel": "destination",
        }));
        assert_eq!(
            detail(&item, &runs),
            "composer-2, source-a, opencode, composer-2 → destination"
        );

        // A stamped target model with no text still stands, so the provider names the target,
        // as the nightly's `??` keeps it.
        let item = handoff_item(json!({"fromModelSelections": two_models, "toModel": "  "}));
        assert_eq!(detail(&item, &runs), "source-a, source-b → claudeAgent");

        // With no stamped sources, the runs name them, and the stamped target still wins.
        let runs = [
            run("source", 1, "codex_personal", "source-model"),
            run("target", 2, "claudeAgent", "later-model"),
        ];
        let item = handoff_item(json!({"fromModelSelections": [], "toModel": "destination"}));
        assert_eq!(detail(&item, &runs), "source-model → destination");
    }

    #[test]
    fn a_handoff_without_stamped_models_reads_them_from_the_runs() {
        // As in the nightly's handoff.test.ts: the target's model is the handoff run's, and the
        // source's is that of the newest run on its provider before the handoff run, wherever
        // the runs come in the list.
        let runs = [
            run("later", 4, "codex_personal", "wrong-later-model"),
            run("old", 1, "codex_personal", "old-model"),
            run("target", 3, "claudeAgent", "destination"),
            run("source", 2, "codex_personal", "source-model"),
        ];
        let item = handoff_item(json!({}));
        assert_eq!(detail(&item, &runs), "source-model → destination");
        // Stamped sources alone still leave the target to the handoff run.
        let item = handoff_item(json!({
            "fromModelSelections": [{"instanceId": "codex_personal", "model": "gpt-5.5"}],
        }));
        assert_eq!(detail(&item, &runs), "gpt-5.5 → destination");
        // With no handoff run, nothing bounds the sources, so the newest run on a source's
        // provider names it, as in the nightly, and the target keeps its provider.
        let item = handoff_item(json!({"runId": null}));
        assert_eq!(detail(&item, &runs), "wrong-later-model → claudeAgent");
        // With no source at all, the target stands alone, without the arrow.
        let item = handoff_item(json!({"fromProviderInstanceIds": []}));
        assert_eq!(detail(&item, &runs), "destination");

        // Several sources keep T3's order, and one with no run before the handoff run keeps its
        // provider.
        let runs = [
            run("cursor", 1, "cursor", "composer-2"),
            run("source", 2, "codex_personal", "source-model"),
            run("target", 3, "claudeAgent", "destination"),
            run("too-late", 4, "opencode", "too-late-model"),
        ];
        let item = handoff_item(json!({
            "fromProviderInstanceIds": ["cursor", "opencode", "codex_personal"],
        }));
        assert_eq!(
            detail(&item, &runs),
            "composer-2, opencode, source-model → destination"
        );

        // A handoff run on another provider names no target, and without runs both ends keep
        // their providers.
        let item = handoff_item(json!({}));
        let runs = [run("target", 2, "codex_personal", "wrong-model")];
        assert_eq!(detail(&item, &runs), "codex_personal → claudeAgent");
        assert_eq!(detail(&item, &[]), "codex_personal → claudeAgent");
    }

    #[test]
    fn an_inherited_handoff_keeps_its_providers() {
        // A fork inherits its parent's items but not its runs, so t3term gives an inherited
        // handoff no runs and names both its ends by provider. The nightly differs: its
        // timeline passes the fork's own runs, where a source can take the model of a fork run
        // that came after the handoff. Here the fork's runs would name both ends of the
        // parent's handoff, yet only the fork's own handoff reads them.
        let runs = [
            run("source", 1, "codex_personal", "source-model"),
            run("target", 2, "claudeAgent", "destination"),
        ];
        let mut inherited = handoff_item(json!({}));
        inherited["threadId"] = json!("parent");
        let local = handoff_item(json!({"id": "handoff-2", "ordinal": 399}));
        let state = ThreadState::from_snapshot(&json!({
            "snapshotSequence": 1,
            "projection": {
                "thread": {"id": "t"},
                "runs": runs,
                "turnItems": [local],
                "visibleTurnItems": [
                    {"position": 0, "visibility": "inherited", "sourceThreadId": "parent",
                        "sourceItemId": "handoff-1", "item": inherited},
                    {"position": 1, "visibility": "local", "sourceThreadId": "t",
                        "sourceItemId": "handoff-2", "item": local},
                ],
            },
        }))
        .expect("a snapshot");
        let bodies: Vec<String> = blocks(&state).into_iter().map(|b| b.body).collect();
        assert_eq!(
            bodies,
            ["codex_personal → claudeAgent", "source-model → destination"]
        );
    }

    #[test]
    fn a_handoffs_endpoints_are_bounded_and_never_move_the_cursor() {
        let stamped = |source: &str, target: &str| {
            handoff_item(json!({
                "fromModelSelections": [{"instanceId": "codex_personal", "model": source}],
                "toModel": target,
            }))
        };
        // Made-up ids: an Esc that clears the screen, a C1 CSI, BEL, a line break and a tab,
        // then CJK, a joined emoji and a combining accent, which a terminal draws as they are.
        let item = stamped(
            "gpt\u{1b}[2J-5.6\u{9b}31m\u{7}\n\tsol",
            "日本語 👩\u{200d}💻 cafe\u{301}",
        );
        assert_eq!(
            detail(&item, &[]),
            "gpt[2J-5.631m sol → 日本語 👩\u{200d}💻 cafe\u{301}"
        );

        // A model that is only controls or blanks gives way to the provider id, which is
        // cleaned the same way. With nothing left of either, the end reads `?`.
        let mut item = stamped("\u{1b}\u{7}", " ");
        item["fromModelSelections"][0]["instanceId"] = json!("codex\u{9b}2J\r\npersonal");
        item["toProviderInstanceId"] = json!("\u{7f}");
        assert_eq!(detail(&item, &[]), "codex2J personal → ?");

        // Past 64 columns an id is cut where a terminal starts a character, and … ends it. 界
        // and 👩‍💻 take two columns each.
        let coder = "👩\u{200d}💻";
        let cases = [
            ("m".repeat(200), format!("{}…", "m".repeat(63))),
            ("界".repeat(100), format!("{}…", "界".repeat(31))),
            (
                format!("a{}", coder.repeat(40)),
                format!("a{}…", coder.repeat(31)),
            ),
            // Only the first 1,024 bytes are read, so … also says there was more after them.
            (format!("a{}", "\u{7}".repeat(2_000)), "a…".to_string()),
        ];
        for (raw, kept) in cases {
            assert!(kept.width() <= MAX_ENDPOINT_WIDTH, "{kept}");
            assert_eq!(detail(&stamped(&raw, "x"), &[]), format!("{kept} → x"));
        }
    }

    #[test]
    fn only_the_first_twelve_sources_reach_the_resolver() {
        // `endpoints` turns both kinds of source list into endpoints through `shown_sources`,
        // and the legacy kind calls `latest_model_before` only in the closure it passes. So a
        // source that closure never receives never reaches a model lookup. The closure here
        // records each raw source it receives.
        let mut raw: Vec<Value> = (1..=1_000).map(|n| json!(format!("p{n}"))).collect();
        raw[2] = json!("p1");
        let cases = [
            (0, 0, 0),
            (11, 11, 0),
            (12, 12, 0),
            (13, 12, 1),
            (1_000, 12, 988),
        ];
        for (count, resolved, more) in cases {
            let mut seen = Vec::new();
            let (from, rest) = shown_sources(&raw[..count], |source| {
                seen.push(source.clone());
                Endpoint {
                    instance_id: source.as_str().unwrap_or_default(),
                    model: None,
                }
            });
            // The first sources come in T3's order, the repeat of p1 included.
            assert_eq!(seen, raw[..resolved], "{count} sources");
            assert_eq!(from.len(), resolved, "{count} sources");
            assert_eq!(rest, more, "{count} sources");
        }
    }

    #[test]
    fn past_twelve_sources_both_kinds_of_handoff_count_the_rest() {
        // Fifteen providers, each with an old run, a run before the handoff run and a run
        // after it, so every source would name a model if it were resolved.
        let providers: Vec<String> = (1..=15).map(|n| format!("p{n}")).collect();
        let mut runs = vec![run("target", 100, "claudeAgent", "destination")];
        for (n, provider) in (1..).zip(&providers) {
            runs.push(run(&format!("{provider}-old"), n, provider, "old-model"));
            runs.push(run(
                &format!("{provider}-new"),
                50 + n,
                provider,
                &format!("m{n}"),
            ));
            runs.push(run(
                &format!("{provider}-late"),
                100 + n,
                provider,
                "too-late",
            ));
        }
        let shown = (1..=12)
            .map(|n| format!("m{n}"))
            .collect::<Vec<_>>()
            .join(", ");
        for (count, more, expected) in [
            (12, 0, format!("{shown} → destination")),
            (13, 1, format!("{shown}, +1 more → destination")),
            (15, 3, format!("{shown}, +3 more → destination")),
        ] {
            let ids = &providers[..count];
            let legacy = handoff_item(json!({"fromProviderInstanceIds": ids}));
            assert_eq!(detail(&legacy, &runs), expected, "{count} legacy");

            let selections: Vec<Value> = (1..=count)
                .map(|n| json!({"instanceId": format!("p{n}"), "model": format!("m{n}")}))
                .collect();
            let stamped = handoff_item(json!({
                "fromModelSelections": selections,
                "toModel": "destination",
            }));
            assert_eq!(detail(&stamped, &runs), expected, "{count} stamped");

            // Either way the endpoints hold only the twelve shown, and the count of the rest
            // comes from the array's length.
            for item in [&legacy, &stamped] {
                let ends = endpoints(item, &runs);
                assert_eq!((ends.from.len(), ends.more), (12, more), "{item}");
            }
        }
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
