//! The composer's model, effort and mode pickers. As in the desktop app, a choice stays in a
//! draft for the thread and reaches T3 with the next message.

use serde_json::{Value, json};

use crate::models::{self, Choice, Plan};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Model,
    Traits,
    Mode,
}

/// What choosing a picker entry changes.
#[derive(Clone, Debug, PartialEq)]
pub enum Pick {
    /// `instance/slug`.
    Model(String),
    /// An option and a value `--option` accepts.
    Option(String, String),
    /// A value T3 sends through the message text, such as ultrathink.
    PromptEffort(String, String),
    Mode(String),
    Plan(bool),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// A section label. `provider` is the instance id when the section is a provider.
    Heading {
        text: String,
        provider: Option<String>,
    },
    Entry {
        label: String,
        current: bool,
        pick: Pick,
    },
}

impl Item {
    pub fn is_entry(&self) -> bool {
        matches!(self, Item::Entry { .. })
    }
}

/// The thread's settings with the draft applied.
pub struct View {
    pub selection: Value,
    pub runtime_mode: String,
    pub interaction_mode: String,
    pub prompt_effort: Option<String>,
    /// What sending now would change.
    pub plan: Plan,
}

pub fn view(config: Option<&Value>, thread: &Value, draft: &Choice) -> View {
    let plan = match config {
        Some(config) if !draft.is_empty() => {
            models::plan(config, thread, draft).unwrap_or_default()
        }
        _ => Plan::default(),
    };
    View {
        selection: plan
            .model_selection
            .clone()
            .unwrap_or_else(|| thread["modelSelection"].clone()),
        runtime_mode: plan.runtime_mode.clone().unwrap_or_else(|| {
            thread["runtimeMode"]
                .as_str()
                .unwrap_or("full-access")
                .to_string()
        }),
        interaction_mode: plan.interaction_mode.clone().unwrap_or_else(|| {
            thread["interactionMode"]
                .as_str()
                .unwrap_or("default")
                .to_string()
        }),
        prompt_effort: plan.prompt_effort.clone(),
        plan,
    }
}

/// The provider and model a selection names.
pub fn selected_model<'a>(config: &'a Value, selection: &Value) -> Option<(&'a Value, &'a Value)> {
    let query = format!(
        "{}/{}",
        selection["instanceId"].as_str()?,
        selection["model"].as_str()?
    );
    models::find_model(config, &query).ok()
}

pub fn items(kind: Kind, config: &Value, view: &View, filter: &str) -> Vec<Item> {
    match kind {
        Kind::Model => model_items(config, view, filter),
        Kind::Traits => trait_items(config, view),
        Kind::Mode => mode_items(config, view),
    }
}

fn heading(text: &str) -> Item {
    Item::Heading {
        text: text.to_string(),
        provider: None,
    }
}

fn model_items(config: &Value, view: &View, filter: &str) -> Vec<Item> {
    let filter = filter.trim().to_lowercase();
    let instance = view.selection["instanceId"].as_str().unwrap_or_default();
    let slug = view.selection["model"].as_str().unwrap_or_default();
    let mut items = Vec::new();
    for provider in models::providers(config)
        .iter()
        .filter(|p| models::enabled(p))
    {
        let provider_id = provider["instanceId"].as_str().unwrap_or_default();
        let provider_name = provider["displayName"].as_str().unwrap_or(provider_id);
        let entries: Vec<Item> = models::models(provider)
            .iter()
            .filter(|model| {
                filter.is_empty()
                    || [provider_name, str_of(model, "name"), str_of(model, "slug")]
                        .iter()
                        .any(|text| text.to_lowercase().contains(&filter))
            })
            .map(|model| {
                let model_slug = str_of(model, "slug");
                Item::Entry {
                    label: model["name"].as_str().unwrap_or(model_slug).to_string(),
                    current: provider_id == instance && model_slug == slug,
                    pick: Pick::Model(models::qualified(provider, model)),
                }
            })
            .collect();
        if entries.is_empty() {
            continue;
        }
        let status = match provider["status"].as_str() {
            Some(status) if status != "ready" => format!(" ({status})"),
            _ => String::new(),
        };
        items.push(Item::Heading {
            text: format!("{provider_name}{status}"),
            provider: Some(provider_id.to_string()),
        });
        items.extend(entries);
    }
    items
}

fn trait_items(config: &Value, view: &View) -> Vec<Item> {
    let Some((_, model)) = selected_model(config, &view.selection) else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for descriptor in models::descriptors(model) {
        let id = str_of(descriptor, "id");
        let label = descriptor["label"].as_str().unwrap_or(id);
        let current = models::effective_value(descriptor, &view.selection);
        match descriptor["type"].as_str() {
            Some("select") => {
                let prompt = view
                    .prompt_effort
                    .as_deref()
                    .filter(|effort| prompt_injected(descriptor, effort));
                items.push(heading(label));
                for choice in descriptor["options"].as_array().into_iter().flatten() {
                    let choice_id = str_of(choice, "id");
                    let injected = prompt_injected(descriptor, choice_id);
                    // `models::plan` refuses message-text values other than ultrathink.
                    if injected && choice_id != models::ULTRATHINK {
                        continue;
                    }
                    items.push(Item::Entry {
                        label: choice["label"].as_str().unwrap_or(choice_id).to_string(),
                        current: match prompt {
                            Some(effort) => effort == choice_id,
                            None => !injected && current.as_ref() == Some(&json!(choice_id)),
                        },
                        pick: if injected {
                            Pick::PromptEffort(id.to_string(), choice_id.to_string())
                        } else {
                            Pick::Option(id.to_string(), choice_id.to_string())
                        },
                    });
                }
            }
            Some("boolean") => {
                items.push(heading(label));
                for (text, value) in [("On", true), ("Off", false)] {
                    items.push(Item::Entry {
                        label: text.to_string(),
                        current: current.as_ref().and_then(Value::as_bool).unwrap_or(false)
                            == value,
                        pick: Pick::Option(id.to_string(), text.to_lowercase()),
                    });
                }
            }
            _ => {}
        }
    }
    items
}

fn mode_items(config: &Value, view: &View) -> Vec<Item> {
    let provider = selected_model(config, &view.selection).map(|(provider, _)| provider);
    let supported = provider.and_then(|p| p["supportedRuntimeModes"].as_array());
    let mut items = vec![heading("Access")];
    for (id, label) in models::RUNTIME_MODES {
        if supported.is_some_and(|modes| !modes.iter().any(|m| m == id)) {
            continue;
        }
        items.push(Item::Entry {
            label: label.to_string(),
            current: view.runtime_mode == *id,
            pick: Pick::Mode(id.to_string()),
        });
    }
    if provider.is_none_or(|p| p["showInteractionModeToggle"] != false) {
        items.push(heading("Plan mode"));
        for (text, on) in [("On", true), ("Off", false)] {
            items.push(Item::Entry {
                label: text.to_string(),
                current: (view.interaction_mode == "plan") == on,
                pick: Pick::Plan(on),
            });
        }
    }
    items
}

/// Records a pick in the thread's draft. The last pick for each setting wins, as in the desktop
/// app: a regular effort clears ultrathink, and ultrathink keeps the regular effort.
pub fn apply(draft: &mut Choice, thread: &Value, pick: &Pick) {
    match pick {
        Pick::Model(qualified) => {
            let selection = &thread["modelSelection"];
            let current = format!(
                "{}/{}",
                str_of(selection, "instanceId"),
                str_of(selection, "model")
            );
            draft.model = (*qualified != current).then(|| qualified.clone());
            draft.effort = None;
            draft.options.clear();
        }
        Pick::Option(id, value) => {
            draft.options.retain(|(option, _)| option != id);
            draft.options.push((id.clone(), value.clone()));
        }
        Pick::PromptEffort(id, value) => {
            draft
                .options
                .retain(|(option, chosen)| !(option == id && chosen == value));
            draft.options.push((id.clone(), value.clone()));
        }
        Pick::Mode(mode) => draft.runtime_mode = Some(mode.clone()),
        Pick::Plan(on) => {
            draft.interaction_mode = Some(if *on { "plan" } else { "default" }.to_string())
        }
    }
}

/// The traits chip, built the way the desktop app's `buildTraitsTriggerDisplay` does: each
/// option's value joined with ` · `, ultrathink in place of the effort, and Fast after the effort.
pub fn traits_label(config: &Value, view: &View) -> Option<String> {
    let (_, model) = selected_model(config, &view.selection)?;
    let effort_id = models::effort_descriptor(model).map(|d| str_of(d, "id"));
    let mut labels: Vec<String> = Vec::new();
    let mut effort_index = None;
    let mut fast = None;
    for descriptor in models::descriptors(model) {
        let id = str_of(descriptor, "id");
        let value = models::effective_value(descriptor, &view.selection);
        let label = match descriptor["type"].as_str() {
            Some("boolean") if id == "fastMode" => {
                fast = Some(value == Some(json!(true)));
                continue;
            }
            Some("boolean") => format!(
                "{} {}",
                descriptor["label"].as_str().unwrap_or(id),
                if value == Some(json!(true)) {
                    "On"
                } else {
                    "Off"
                }
            ),
            Some("select") if Some(id) == effort_id && view.prompt_effort.is_some() => {
                "Ultrathink".to_string()
            }
            Some("select") => {
                let Some(value) = value else { continue };
                descriptor["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|c| c["id"] == value)
                    .and_then(|c| c["label"].as_str())
                    .unwrap_or_else(|| value.as_str().unwrap_or_default())
                    .to_string()
            }
            _ => continue,
        };
        if Some(id) == effort_id {
            effort_index = Some(labels.len());
        }
        labels.push(label);
    }
    match (fast, effort_index) {
        (Some(false), _) if labels.is_empty() => labels.push("Normal".to_string()),
        (Some(true), Some(index)) => labels[index] += " Fast",
        (Some(true), None) => labels.push("Fast".to_string()),
        _ => {}
    }
    (!labels.is_empty()).then(|| labels.join(" · "))
}

fn prompt_injected(descriptor: &Value, value: &str) -> bool {
    descriptor["promptInjectedValues"]
        .as_array()
        .is_some_and(|values| values.iter().any(|v| v == value))
}

fn str_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Value {
        json!({"providers": [
            {"instanceId": "claudeAgent", "displayName": "Claude", "enabled": true, "status": "ready",
             "models": [
                {"slug": "claude-opus-5-5", "name": "Opus 5.5", "capabilities": {"optionDescriptors": [
                    {"id": "effort", "label": "Effort", "type": "select",
                     "options": [{"id": "low", "label": "Low"}, {"id": "high", "label": "High", "isDefault": true},
                                 {"id": "ultrathink", "label": "Ultrathink"},
                                 {"id": "megathink", "label": "Megathink"}],
                     "promptInjectedValues": ["ultrathink", "megathink"]},
                    {"id": "fastMode", "label": "Fast Mode", "type": "boolean"}
                ]}},
                {"slug": "claude-haiku-4-5", "name": "Claude Haiku 4.5", "capabilities": {"optionDescriptors": [
                    {"id": "thinking", "label": "Thinking", "type": "boolean"}
                ]}}
             ]},
            {"instanceId": "codex", "displayName": "Codex", "enabled": true, "status": "ready",
             "supportedRuntimeModes": ["approval-required", "full-access"],
             "showInteractionModeToggle": false,
             "models": [{"slug": "gpt-6", "name": "GPT-6", "capabilities": {"optionDescriptors": []}}]},
            {"instanceId": "pi", "displayName": "Pi", "enabled": false,
             "models": [{"slug": "pi-1", "name": "Pi One"}]}
        ]})
    }

    fn thread() -> Value {
        json!({
            "modelSelection": {"instanceId": "claudeAgent", "model": "claude-opus-5-5",
                               "options": [{"id": "effort", "value": "low"}]},
            "runtimeMode": "approval-required",
            "interactionMode": "default"
        })
    }

    fn entries(items: &[Item]) -> Vec<(String, bool)> {
        items
            .iter()
            .filter_map(|item| match item {
                Item::Entry { label, current, .. } => Some((label.clone(), *current)),
                Item::Heading { .. } => None,
            })
            .collect()
    }

    fn pick(items: &[Item], label: &str) -> Pick {
        items
            .iter()
            .find_map(|item| match item {
                Item::Entry { label: l, pick, .. } if l == label => Some(pick.clone()),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn models_list_enabled_providers_and_filter() {
        let (config, thread) = (config(), thread());
        let view = view(Some(&config), &thread, &Choice::default());
        let items = items(Kind::Model, &config, &view, "");
        assert_eq!(
            entries(&items),
            [
                ("Opus 5.5".to_string(), true),
                ("Claude Haiku 4.5".to_string(), false),
                ("GPT-6".to_string(), false)
            ]
        );
        assert!(
            !items
                .iter()
                .any(|i| matches!(i, Item::Heading { text, .. } if text == "Pi"))
        );
        let filtered = super::items(Kind::Model, &config, &view, "haiku");
        assert_eq!(
            entries(&filtered),
            [("Claude Haiku 4.5".to_string(), false)]
        );
    }

    #[test]
    fn picks_build_a_draft_that_the_chips_reflect() {
        let (config, thread) = (config(), thread());
        let mut draft = Choice::default();
        let base = view(Some(&config), &thread, &draft);
        assert_eq!(traits_label(&config, &base).as_deref(), Some("Low"));

        let traits = items(Kind::Traits, &config, &base, "");
        apply(&mut draft, &thread, &pick(&traits, "Ultrathink"));
        let ultra = view(Some(&config), &thread, &draft);
        assert_eq!(ultra.prompt_effort.as_deref(), Some("ultrathink"));
        assert_eq!(traits_label(&config, &ultra).as_deref(), Some("Ultrathink"));
        let marked = entries(&items(Kind::Traits, &config, &ultra, ""));
        assert!(marked.contains(&("Ultrathink".to_string(), true)));
        assert!(marked.contains(&("Low".to_string(), false)));
        // t3term can't send message-text values other than ultrathink, so the menu hides them.
        assert!(!marked.iter().any(|(label, _)| label == "Megathink"));

        // A regular effort replaces ultrathink. Fast mode joins the effort label.
        apply(&mut draft, &thread, &pick(&traits, "High"));
        apply(
            &mut draft,
            &thread,
            &Pick::Option("fastMode".into(), "on".into()),
        );
        let high = view(Some(&config), &thread, &draft);
        assert_eq!(high.prompt_effort, None);
        assert_eq!(traits_label(&config, &high).as_deref(), Some("High Fast"));

        // Choosing the current model again leaves only the other choices in the draft.
        let models = items(Kind::Model, &config, &high, "");
        apply(&mut draft, &thread, &pick(&models, "Claude Haiku 4.5"));
        let haiku = view(Some(&config), &thread, &draft);
        assert_eq!(haiku.selection["model"], "claude-haiku-4-5");
        assert_eq!(
            traits_label(&config, &haiku).as_deref(),
            Some("Thinking Off")
        );
        apply(&mut draft, &thread, &pick(&models, "Opus 5.5"));
        assert!(draft.is_empty());
    }

    #[test]
    fn modes_follow_the_providers_support() {
        let config = config();
        let mut draft = Choice::default();
        let claude = view(Some(&config), &thread(), &draft);
        let items = items(Kind::Mode, &config, &claude, "");
        assert_eq!(entries(&items).len(), 4 + 2);
        apply(&mut draft, &thread(), &pick(&items, "Full access"));
        apply(&mut draft, &thread(), &Pick::Plan(true));
        let changed = view(Some(&config), &thread(), &draft);
        assert_eq!(changed.plan.runtime_mode.as_deref(), Some("full-access"));
        assert_eq!(changed.interaction_mode, "plan");

        let codex_thread = json!({"modelSelection": {"instanceId": "codex", "model": "gpt-6"}});
        let codex = view(Some(&config), &codex_thread, &Choice::default());
        assert_eq!(
            entries(&super::items(Kind::Mode, &config, &codex, "")),
            [
                ("Supervised".to_string(), false),
                ("Full access".to_string(), true)
            ]
        );
        assert_eq!(traits_label(&config, &codex), None);
    }
}
