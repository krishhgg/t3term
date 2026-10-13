//! Model, reasoning-effort and mode choices, checked against the server's provider list.
//!
//! Models and option values come from `server.getConfig`, so new ones work without a t3term
//! update. Field names follow T3's contracts: `ServerProvider` and `ServerProviderModel` in
//! packages/contracts/src/server.ts, option descriptors in model.ts, `ModelSelection` in
//! modelSelection.ts and the mode values in providerPolicy.ts.

use anyhow::Result;
use serde_json::{Value, json};

use crate::error::{err_exit, exit};

/// Runtime modes with the labels the desktop app shows.
pub const RUNTIME_MODES: &[(&str, &str)] = &[
    ("approval-required", "Supervised"),
    ("auto-accept-edits", "Auto-accept edits"),
    ("auto", "Auto"),
    ("full-access", "Full access"),
];

/// Option ids that mean reasoning effort, checked in this order. Claude uses `effort`, Codex and
/// Grok use `reasoningEffort`, Cursor uses `reasoning`, and Pi uses a `thinking` select.
const EFFORT_IDS: &[&str] = &["effort", "reasoningEffort", "reasoning", "thinking"];

/// The desktop app sends this effort as a prefix on the message instead of as an option.
pub const ULTRATHINK: &str = "ultrathink";
const ULTRATHINK_PREFIX: &str = "Ultrathink:";

/// What the user asked to change. `None` keeps the thread's current value.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Choice {
    /// `instance/slug`, a slug, or a model name.
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Other options as `(id, value)`.
    pub options: Vec<(String, String)>,
    pub runtime_mode: Option<String>,
    pub interaction_mode: Option<String>,
}

impl Choice {
    pub fn is_empty(&self) -> bool {
        self.model.is_none()
            && self.effort.is_none()
            && self.options.is_empty()
            && self.runtime_mode.is_none()
            && self.interaction_mode.is_none()
    }
}

/// The commands a choice turns into for one thread. Each field is `None` when nothing changes.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Plan {
    /// The full `modelSelection` to send.
    pub model_selection: Option<Value>,
    pub runtime_mode: Option<String>,
    pub interaction_mode: Option<String>,
    /// An effort the desktop app applies by prefixing the message, such as Claude's ultrathink.
    pub prompt_effort: Option<String>,
}

impl Plan {
    pub fn changes_modes(&self) -> bool {
        self.runtime_mode.is_some() || self.interaction_mode.is_some()
    }

    /// The message text to send, with the ultrathink prefix when that effort was chosen.
    pub fn message_text(&self, text: &str) -> String {
        let text = text.trim();
        match self.prompt_effort.as_deref() {
            Some(ULTRATHINK) => ultrathink_prefix(text),
            _ => text.to_string(),
        }
    }
}

/// Matches `applyClaudePromptEffortPrefix` in packages/shared/src/model.ts: a leading slash
/// command stays unprefixed so Claude still runs it.
fn ultrathink_prefix(text: &str) -> String {
    let first = text.split_whitespace().next().unwrap_or("");
    let slash_command = first.len() > 1 && first.starts_with('/') && !first[1..].contains('/');
    if text.is_empty() || slash_command || text.starts_with(ULTRATHINK_PREFIX) {
        return text.to_string();
    }
    format!("{ULTRATHINK_PREFIX}\n{text}")
}

pub fn providers(config: &Value) -> &[Value] {
    config["providers"].as_array().map_or(&[], Vec::as_slice)
}

pub fn models(provider: &Value) -> &[Value] {
    provider["models"].as_array().map_or(&[], Vec::as_slice)
}

pub fn descriptors(model: &Value) -> &[Value] {
    model["capabilities"]["optionDescriptors"]
        .as_array()
        .map_or(&[], Vec::as_slice)
}

fn provider_by_instance<'a>(config: &'a Value, instance_id: &str) -> Option<&'a Value> {
    providers(config)
        .iter()
        .find(|p| p["instanceId"].as_str() == Some(instance_id))
}

fn model_by_slug<'a>(provider: &'a Value, slug: &str) -> Option<&'a Value> {
    models(provider)
        .iter()
        .find(|m| m["slug"].as_str() == Some(slug))
}

fn model_matches(model: &Value, query: &str) -> bool {
    model["slug"].as_str() == Some(query)
        || ["name", "shortName"].iter().any(|key| {
            model[*key]
                .as_str()
                .is_some_and(|n| n.eq_ignore_ascii_case(query))
        })
        || model["aliases"]
            .as_array()
            .is_some_and(|aliases| aliases.iter().any(|a| a.as_str() == Some(query)))
}

/// Finds a model by `instance/slug`, slug, name or alias. Returns the provider and the model.
pub fn find_model<'a>(config: &'a Value, query: &str) -> Result<(&'a Value, &'a Value)> {
    let query = query.trim();
    let not_found = || {
        err_exit(
            "MODEL_NOT_FOUND",
            exit::NOT_FOUND,
            format!("No model matches {query}. `t3term models` lists them."),
        )
    };
    if let Some((instance, model)) = query.split_once('/')
        && let Some(provider) = provider_by_instance(config, instance)
    {
        let found = models(provider).iter().find(|m| model_matches(m, model));
        return found.map(|m| (provider, m)).ok_or_else(not_found);
    }
    let mut found: Vec<(&Value, &Value)> = providers(config)
        .iter()
        .flat_map(|p| models(p).iter().map(move |m| (p, m)))
        .filter(|(_, m)| model_matches(m, query))
        .collect();
    // An exact slug beats a name or alias, and an enabled provider beats a disabled one.
    if found.iter().any(|(_, m)| m["slug"].as_str() == Some(query)) {
        found.retain(|(_, m)| m["slug"].as_str() == Some(query));
    }
    if found.iter().any(|(p, _)| enabled(p)) {
        found.retain(|(p, _)| enabled(p));
    }
    match found.as_slice() {
        [] => Err(not_found()),
        [one] => Ok(*one),
        several => Err(err_exit(
            "AMBIGUOUS_MODEL",
            exit::USAGE,
            format!(
                "{query} matches several models: {}. Pass provider/model.",
                several
                    .iter()
                    .map(|(p, m)| qualified(p, m))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
    }
}

pub fn enabled(provider: &Value) -> bool {
    provider["enabled"].as_bool() != Some(false)
}

/// `instance/slug`, the form `find_model` accepts without ambiguity.
pub fn qualified(provider: &Value, model: &Value) -> String {
    format!(
        "{}/{}",
        provider["instanceId"].as_str().unwrap_or("?"),
        model["slug"].as_str().unwrap_or("?")
    )
}

/// The select descriptor that holds reasoning effort, if the model has one.
pub fn effort_descriptor(model: &Value) -> Option<&Value> {
    EFFORT_IDS.iter().find_map(|id| {
        descriptors(model)
            .iter()
            .find(|d| d["id"].as_str() == Some(id) && d["type"] == "select")
    })
}

/// Checks a value against a descriptor and returns it in wire form.
fn option_value(descriptor: &Value, value: &str) -> Result<Value> {
    let id = descriptor["id"].as_str().unwrap_or("option");
    match descriptor["type"].as_str() {
        Some("boolean") => match value.to_ascii_lowercase().as_str() {
            "true" | "on" | "yes" => Ok(json!(true)),
            "false" | "off" | "no" => Ok(json!(false)),
            _ => Err(usage(format!("{id} takes on or off, not {value}."))),
        },
        Some("select") => {
            let choices = descriptor["options"]
                .as_array()
                .map_or(&[][..], Vec::as_slice);
            choices
                .iter()
                .find(|c| {
                    c["id"].as_str() == Some(value)
                        || c["label"]
                            .as_str()
                            .is_some_and(|l| l.eq_ignore_ascii_case(value))
                })
                .map(|c| c["id"].clone())
                .ok_or_else(|| {
                    usage(format!(
                        "{id} takes one of {}, not {value}.",
                        choices
                            .iter()
                            .filter_map(|c| c["id"].as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                })
        }
        _ => Err(usage(format!("t3term cannot set the {id} option."))),
    }
}

fn usage(message: String) -> anyhow::Error {
    err_exit("INVALID_CHOICE", exit::USAGE, message)
}

fn set_option(options: &mut Vec<Value>, id: &str, value: Value) {
    options.retain(|o| o["id"].as_str() != Some(id));
    options.push(json!({"id": id, "value": value}));
}

/// Whether T3 sends this select value through the message text instead of as an option.
fn prompt_injected(descriptor: &Value, value: &str) -> bool {
    descriptor["promptInjectedValues"]
        .as_array()
        .is_some_and(|values| values.iter().any(|v| v.as_str() == Some(value)))
}

/// Whether a saved value is already valid for a descriptor. T3 keeps a value across a model
/// switch only in this case: a boolean for a boolean, an offered choice id for a select.
fn fits(descriptor: &Value, value: &Value) -> bool {
    match (descriptor["type"].as_str(), value) {
        (Some("boolean"), Value::Bool(_)) => true,
        (Some("select"), Value::String(value)) => {
            !prompt_injected(descriptor, value)
                && descriptor["options"]
                    .as_array()
                    .is_some_and(|choices| choices.iter().any(|c| c["id"] == *value))
        }
        _ => false,
    }
}

/// Applies one `--effort` or `--option` value. A value T3 sends through the message text, such
/// as ultrathink, becomes the plan's prompt effort instead of an option. The last value given for
/// a descriptor wins, as in the desktop app's picker.
fn choose(plan: &mut Plan, options: &mut Vec<Value>, descriptor: &Value, raw: &str) -> Result<()> {
    let value = option_value(descriptor, raw)?;
    match value.as_str().filter(|v| prompt_injected(descriptor, v)) {
        Some(ULTRATHINK) => plan.prompt_effort = Some(ULTRATHINK.to_string()),
        Some(other) => {
            return Err(usage(format!(
                "T3 applies {other} through the message text in a way t3term does not support yet."
            )));
        }
        None => {
            // A regular value replaces an earlier message-only one for the same descriptor.
            if descriptor["promptInjectedValues"]
                .as_array()
                .is_some_and(|values| !values.is_empty())
            {
                plan.prompt_effort = None;
            }
            set_option(
                options,
                descriptor["id"].as_str().unwrap_or_default(),
                value,
            );
        }
    }
    Ok(())
}

/// Works out the commands that apply `choice` to `thread`, checking every value against `config`.
pub fn plan(config: &Value, thread: &Value, choice: &Choice) -> Result<Plan> {
    // Reading settings needs no provider, even one that is turned off.
    if choice.is_empty() {
        return Ok(Plan::default());
    }
    let current = &thread["modelSelection"];
    let current_instance = current["instanceId"]
        .as_str()
        .or_else(|| thread["providerInstanceId"].as_str())
        .unwrap_or_default();
    let current_model = current["model"].as_str().unwrap_or_default();
    let (provider, model) = match &choice.model {
        Some(query) => {
            let (provider, model) = find_model(config, query)?;
            (Some(provider), Some(model))
        }
        None => {
            let provider = provider_by_instance(config, current_instance);
            (
                provider,
                provider.and_then(|p| model_by_slug(p, current_model)),
            )
        }
    };
    if let Some(provider) = provider.filter(|p| !enabled(p)) {
        return Err(err_exit(
            "PROVIDER_DISABLED",
            exit::REJECTED,
            format!(
                "{} is turned off in T3's settings.",
                provider["displayName"].as_str().unwrap_or("That provider")
            ),
        ));
    }
    let unknown_model = || {
        usage(format!(
            "T3 lists no options for {current_instance}/{current_model}, so t3term cannot change them."
        ))
    };

    let mut result = Plan::default();
    let changes_model =
        choice.model.is_some() || choice.effort.is_some() || !choice.options.is_empty();
    if changes_model {
        let model = model.ok_or_else(unknown_model)?;
        let provider = provider.ok_or_else(unknown_model)?;
        let instance = provider["instanceId"].as_str().unwrap_or_default();
        let slug = model["slug"].as_str().unwrap_or_default();
        let same_model = instance == current_instance && slug == current_model;
        // Keep the thread's options that still fit the chosen model.
        let mut options: Vec<Value> = current["options"]
            .as_array()
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .filter(|o| {
                same_model
                    || descriptors(model)
                        .iter()
                        .any(|d| d["id"] == o["id"] && fits(d, &o["value"]))
            })
            .cloned()
            .collect();
        if let Some(effort) = &choice.effort {
            let descriptor = effort_descriptor(model).ok_or_else(|| {
                usage(format!(
                    "{} has no reasoning effort setting.",
                    qualified(provider, model)
                ))
            })?;
            choose(&mut result, &mut options, descriptor, effort)?;
        }
        for (id, value) in &choice.options {
            let descriptor = descriptors(model)
                .iter()
                .find(|d| d["id"].as_str() == Some(id.as_str()))
                .ok_or_else(|| {
                    usage(format!(
                        "{} has no {id} option. It has: {}.",
                        qualified(provider, model),
                        descriptors(model)
                            .iter()
                            .filter_map(|d| d["id"].as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                })?;
            choose(&mut result, &mut options, descriptor, value)?;
        }
        let selection = json!({"instanceId": instance, "model": slug, "options": options});
        // Send a selection only when it differs from the thread's, in any option's value
        // rather than in list order. A choice the thread already has, or an ultrathink-only
        // one, changes nothing here.
        let unchanged = same_model
            && descriptors(model)
                .iter()
                .all(|d| effective_value(d, &selection) == effective_value(d, current));
        if !unchanged {
            result.model_selection = Some(selection);
        }
    }

    if let Some(mode) = &choice.runtime_mode {
        let mode = RUNTIME_MODES
            .iter()
            .find(|(id, label)| id == mode || label.eq_ignore_ascii_case(mode))
            .map(|(id, _)| *id)
            .ok_or_else(|| {
                usage(format!(
                    "The runtime mode is one of {}, not {mode}.",
                    RUNTIME_MODES
                        .iter()
                        .map(|(id, _)| *id)
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })?;
        // When a provider lists its modes, T3 runs any other mode as approval-required.
        if let Some(supported) = provider.and_then(|p| p["supportedRuntimeModes"].as_array())
            && !supported.iter().any(|m| m.as_str() == Some(mode))
        {
            return Err(usage(format!(
                "{} supports only {}.",
                provider
                    .and_then(|p| p["displayName"].as_str())
                    .unwrap_or("This provider"),
                supported
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        if thread["runtimeMode"].as_str() != Some(mode) {
            result.runtime_mode = Some(mode.to_string());
        }
    }

    if let Some(mode) = &choice.interaction_mode {
        if mode == "plan" && provider.is_some_and(|p| p["showInteractionModeToggle"] == false) {
            return Err(usage(format!(
                "{} has no plan mode.",
                provider
                    .and_then(|p| p["displayName"].as_str())
                    .unwrap_or("This provider")
            )));
        }
        let current = thread["interactionMode"].as_str().unwrap_or("default");
        if current != mode {
            result.interaction_mode = Some(mode.clone());
        }
    }
    Ok(result)
}

/// The value an option has for a selection: the chosen value, else the descriptor's current
/// value, else the choice marked as default. Mirrors `packages/shared/src/model.ts`.
pub fn effective_value(descriptor: &Value, selection: &Value) -> Option<Value> {
    let id = descriptor["id"].as_str()?;
    if let Some(chosen) = selection["options"]
        .as_array()
        .and_then(|options| options.iter().find(|o| o["id"].as_str() == Some(id)))
    {
        return Some(chosen["value"].clone());
    }
    if !descriptor["currentValue"].is_null() {
        return Some(descriptor["currentValue"].clone());
    }
    descriptor["options"]
        .as_array()?
        .iter()
        .find(|c| c["isDefault"] == true)
        .map(|c| c["id"].clone())
}

pub fn runtime_mode_label(mode: &str) -> &str {
    RUNTIME_MODES
        .iter()
        .find(|(id, _)| *id == mode)
        .map_or(mode, |(_, label)| label)
}

/// A thread's model, options and modes as plain values, for display and `--json`.
pub fn thread_settings(config: &Value, thread: &Value) -> Value {
    let selection = &thread["modelSelection"];
    let instance = selection["instanceId"]
        .as_str()
        .or_else(|| thread["providerInstanceId"].as_str())
        .unwrap_or_default();
    let slug = selection["model"].as_str().unwrap_or_default();
    let provider = provider_by_instance(config, instance);
    let model = provider.and_then(|p| model_by_slug(p, slug));
    let options: serde_json::Map<String, Value> = model
        .map(descriptors)
        .unwrap_or_default()
        .iter()
        .filter_map(|d| {
            Some((
                d["id"].as_str()?.to_string(),
                effective_value(d, selection)?,
            ))
        })
        .collect();
    let runtime_mode = thread["runtimeMode"].as_str().unwrap_or("full-access");
    json!({
        "instanceId": instance,
        "provider": provider.and_then(|p| p["displayName"].as_str()),
        "model": slug,
        "modelName": model.and_then(|m| m["name"].as_str()),
        "options": options,
        "runtimeMode": runtime_mode,
        "runtimeModeLabel": runtime_mode_label(runtime_mode),
        "interactionMode": thread["interactionMode"].as_str().unwrap_or("default"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Value {
        json!({"providers": [
            {"instanceId": "claudeAgent", "displayName": "Claude", "enabled": true,
             "supportedRuntimeModes": null,
             "models": [
                {"slug": "claude-opus-5-5", "name": "Opus 5.5", "isCustom": false, "capabilities": {"optionDescriptors": [
                    {"id": "effort", "label": "Effort", "type": "select",
                     "options": [{"id": "low", "label": "Low"}, {"id": "medium", "label": "Medium", "isDefault": true},
                                 {"id": "high", "label": "High"}, {"id": "ultrathink", "label": "Ultrathink"}],
                     "promptInjectedValues": ["ultrathink"]},
                    {"id": "thinking", "label": "Thinking", "type": "boolean"},
                    {"id": "fastMode", "label": "Fast", "type": "boolean"}
                ]}},
                {"slug": "shared-slug", "name": "Shared", "isCustom": true, "capabilities": null}
             ]},
            {"instanceId": "codex", "displayName": "Codex", "enabled": true,
             "models": [
                {"slug": "gpt-6", "name": "GPT-6", "isCustom": false, "capabilities": {"optionDescriptors": [
                    {"id": "reasoningEffort", "label": "Reasoning", "type": "select",
                     "options": [{"id": "low", "label": "Low"}, {"id": "high", "label": "High", "isDefault": true}]}
                ]}},
                {"slug": "shared-slug", "name": "Shared", "isCustom": true, "capabilities": null}
             ]},
            {"instanceId": "pi", "displayName": "Pi", "enabled": false, "showInteractionModeToggle": false,
             "supportedRuntimeModes": ["approval-required", "full-access"],
             "models": [{"slug": "pi-1", "name": "Pi One", "capabilities": {"optionDescriptors": [
                {"id": "thinking", "label": "Thinking", "type": "select", "options": [{"id": "off", "label": "Off"}, {"id": "on", "label": "On"}]}
             ]}}]}
        ]})
    }

    fn thread() -> Value {
        json!({
            "providerInstanceId": "claudeAgent",
            "modelSelection": {"instanceId": "claudeAgent", "model": "claude-opus-5-5",
                               "options": [{"id": "effort", "value": "high"}, {"id": "fastMode", "value": true}]},
            "runtimeMode": "full-access",
            "interactionMode": "default"
        })
    }

    fn choice() -> Choice {
        Choice::default()
    }

    #[test]
    fn finds_models_by_slug_name_and_qualified_id() {
        let config = config();
        assert_eq!(find_model(&config, "gpt-6").unwrap().1["slug"], "gpt-6");
        assert_eq!(
            find_model(&config, "opus 5.5").unwrap().1["slug"],
            "claude-opus-5-5"
        );
        assert_eq!(
            find_model(&config, "codex/shared-slug").unwrap().0["instanceId"],
            "codex"
        );
        assert!(find_model(&config, "shared-slug").is_err());
        assert!(find_model(&config, "nope").is_err());
    }

    #[test]
    fn effort_maps_to_each_providers_option_id() {
        let (config, thread) = (config(), thread());
        let plan = plan(
            &config,
            &thread,
            &Choice {
                effort: Some("low".into()),
                ..choice()
            },
        )
        .unwrap();
        assert_eq!(
            plan.model_selection.unwrap()["options"],
            json!([{"id": "fastMode", "value": true}, {"id": "effort", "value": "low"}])
        );
        let switched = super::plan(
            &config,
            &thread,
            &Choice {
                model: Some("gpt-6".into()),
                effort: Some("High".into()),
                ..choice()
            },
        )
        .unwrap();
        // Options Codex does not describe are dropped when the model changes.
        assert_eq!(
            switched.model_selection.unwrap(),
            json!({"instanceId": "codex", "model": "gpt-6", "options": [{"id": "reasoningEffort", "value": "high"}]})
        );
    }

    #[test]
    fn ultrathink_prefixes_the_message_instead_of_changing_the_model() {
        let plan = plan(
            &config(),
            &thread(),
            &Choice {
                effort: Some("ultrathink".into()),
                ..choice()
            },
        )
        .unwrap();
        assert_eq!(plan.model_selection, None);
        assert_eq!(plan.message_text(" fix it "), "Ultrathink:\nfix it");
        assert_eq!(plan.message_text("/review now"), "/review now");
        assert_eq!(
            plan.message_text("/src/a.rs is wrong"),
            "Ultrathink:\n/src/a.rs is wrong"
        );
        assert_eq!(plan.message_text("Ultrathink: again"), "Ultrathink: again");
        // The label and `--option` reach the same prefix, never the thread's options.
        for choice in [
            Choice {
                effort: Some("Ultrathink".into()),
                ..choice()
            },
            Choice {
                options: vec![("effort".into(), "ultrathink".into())],
                ..choice()
            },
        ] {
            let plan = super::plan(&config(), &thread(), &choice).unwrap();
            assert_eq!(plan.model_selection, None);
            assert_eq!(plan.prompt_effort.as_deref(), Some(ULTRATHINK));
        }
    }

    #[test]
    fn the_last_effort_given_wins() {
        let efforts = |values: [&str; 2]| {
            plan(
                &config(),
                &thread(),
                &Choice {
                    options: values
                        .iter()
                        .map(|v| ("effort".to_string(), v.to_string()))
                        .collect(),
                    ..choice()
                },
            )
            .unwrap()
        };
        let regular_last = efforts(["ultrathink", "low"]);
        assert_eq!(regular_last.prompt_effort, None);
        assert_eq!(
            regular_last.model_selection.unwrap()["options"],
            json!([{"id": "fastMode", "value": true}, {"id": "effort", "value": "low"}])
        );
        // Like the desktop app, ultrathink after a regular effort keeps that effort and adds the prefix.
        let ultrathink_last = efforts(["low", "ultrathink"]);
        assert_eq!(ultrathink_last.prompt_effort.as_deref(), Some(ULTRATHINK));
        assert_eq!(
            ultrathink_last.model_selection.unwrap()["options"],
            json!([{"id": "fastMode", "value": true}, {"id": "effort", "value": "low"}])
        );
    }

    #[test]
    fn choices_the_thread_already_has_change_nothing() {
        let (config, thread) = (config(), thread());
        let same = |choice: Choice| plan(&config, &thread, &choice).unwrap();
        // The TUI keeps a draft until this holds, so it must hold once the thread catches up.
        assert_eq!(
            same(Choice {
                model: Some("claude-opus-5-5".into()),
                ..choice()
            }),
            Plan::default()
        );
        // Setting a value moves it to the end of the list. Order alone is no change.
        assert_eq!(
            same(Choice {
                effort: Some("high".into()),
                options: vec![("fastMode".into(), "true".into())],
                ..choice()
            }),
            Plan::default()
        );
        // A value the thread gets by default is no change either.
        let mut bare = thread.clone();
        bare["modelSelection"]["options"] = json!([]);
        assert_eq!(
            plan(
                &config,
                &bare,
                &Choice {
                    effort: Some("medium".into()),
                    ..choice()
                }
            )
            .unwrap(),
            Plan::default()
        );
        assert!(
            same(Choice {
                model: Some("claude-opus-5-5".into()),
                effort: Some("low".into()),
                ..choice()
            })
            .model_selection
            .is_some()
        );
    }

    #[test]
    fn model_switch_keeps_only_values_valid_for_the_new_model() {
        let pi_thread = json!({
            "modelSelection": {"instanceId": "pi", "model": "pi-1",
                               "options": [{"id": "thinking", "value": "on"}]},
        });
        let switched = plan(
            &config(),
            &pi_thread,
            &Choice {
                model: Some("claude-opus-5-5".into()),
                ..choice()
            },
        )
        .unwrap();
        // Claude's thinking is a boolean, so Pi's "on" string is dropped, not carried over.
        assert_eq!(switched.model_selection.unwrap()["options"], json!([]));
        let codex_thread = json!({
            "modelSelection": {"instanceId": "codex", "model": "gpt-6",
                               "options": [{"id": "effort", "value": "ultrathink"}, {"id": "fastMode", "value": true},
                                           {"id": "thinking", "value": "on"}]},
        });
        let to_claude = |effort: &str| {
            let mut thread = codex_thread.clone();
            thread["modelSelection"]["options"][0]["value"] = json!(effort);
            plan(
                &config(),
                &thread,
                &Choice {
                    model: Some("claude-opus-5-5".into()),
                    ..choice()
                },
            )
            .unwrap()
            .model_selection
            .unwrap()["options"]
                .clone()
        };
        // A message-only effort is never carried into the options; an offered one is.
        assert_eq!(
            to_claude("ultrathink"),
            json!([{"id": "fastMode", "value": true}])
        );
        assert_eq!(
            to_claude("high"),
            json!([{"id": "effort", "value": "high"}, {"id": "fastMode", "value": true}])
        );
    }

    #[test]
    fn settings_read_on_a_disabled_provider() {
        let pi_thread = json!({"modelSelection": {"instanceId": "pi", "model": "pi-1"}});
        assert_eq!(
            plan(&config(), &pi_thread, &choice()).unwrap(),
            Plan::default()
        );
        assert!(
            plan(
                &config(),
                &pi_thread,
                &Choice {
                    runtime_mode: Some("full-access".into()),
                    ..choice()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_values_the_model_does_not_offer() {
        let (config, thread) = (config(), thread());
        assert!(
            plan(
                &config,
                &thread,
                &Choice {
                    effort: Some("max".into()),
                    ..choice()
                }
            )
            .is_err()
        );
        assert!(
            plan(
                &config,
                &thread,
                &Choice {
                    options: vec![("speed".into(), "1".into())],
                    ..choice()
                }
            )
            .is_err()
        );
        assert!(
            plan(
                &config,
                &thread,
                &Choice {
                    options: vec![("fastMode".into(), "maybe".into())],
                    ..choice()
                }
            )
            .is_err()
        );
        assert!(
            plan(
                &config,
                &thread,
                &Choice {
                    model: Some("pi-1".into()),
                    ..choice()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn modes_are_sent_only_when_they_change() {
        let (config, thread) = (config(), thread());
        let same = plan(
            &config,
            &thread,
            &Choice {
                runtime_mode: Some("Full access".into()),
                interaction_mode: Some("default".into()),
                ..choice()
            },
        )
        .unwrap();
        assert_eq!(same, Plan::default());
        let changed = plan(
            &config,
            &thread,
            &Choice {
                runtime_mode: Some("supervised".into()),
                interaction_mode: Some("plan".into()),
                ..choice()
            },
        )
        .unwrap();
        assert_eq!(changed.runtime_mode.as_deref(), Some("approval-required"));
        assert_eq!(changed.interaction_mode.as_deref(), Some("plan"));
        assert!(
            plan(
                &config,
                &thread,
                &Choice {
                    runtime_mode: Some("yolo".into()),
                    ..choice()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn effective_values_fall_back_to_the_default_choice() {
        let settings = thread_settings(
            &config(),
            &json!({
                "modelSelection": {"instanceId": "claudeAgent", "model": "claude-opus-5-5", "options": []},
                "runtimeMode": "approval-required"
            }),
        );
        assert_eq!(settings["options"]["effort"], "medium");
        assert!(settings["options"].get("thinking").is_none());
        assert_eq!(settings["runtimeModeLabel"], "Supervised");
        assert_eq!(settings["interactionMode"], "default");
    }
}
