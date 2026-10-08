//! The composer's model, effort and mode pickers. As in the desktop app, a choice stays in a
//! draft for the thread and reaches T3 with the next message.

use anyhow::Result;
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
    /// `plan` only while Build and Plan are offered.
    pub interaction_mode: String,
    /// Whether the mode menu offers Build and Plan.
    pub offers_plan: bool,
    pub prompt_effort: Option<String>,
    /// What sending now would change.
    pub plan: Plan,
}

/// Whether the composer offers Build and Plan for a thread on `selection`, decided as the
/// desktop's `resolveComposerInteractionMode` does in apps/web/src/components/ChatView.logic.ts:
/// only with the legacy plan setting on, and only for a provider that shows the toggle. Before
/// T3's model list arrives the provider is unknown, and t3term keeps offering the thread's mode
/// rather than switching it. The desktop can't send at all then, so it never has to choose.
pub fn offers_plan(config: Option<&Value>, selection: &Value, plan_mode_enabled: bool) -> bool {
    plan_mode_enabled
        && config
            .and_then(|config| {
                models::providers(config)
                    .iter()
                    .find(|provider| provider["instanceId"] == selection["instanceId"])
            })
            .is_none_or(|provider| provider["showInteractionModeToggle"] != false)
}

/// What sending a message now changes: the draft, checked against `config`, and the mode the
/// desktop would send. Where Build and Plan aren't offered, that mode is Build, as in the
/// desktop's `persistThreadSettingsForNextTurn` in ChatView.tsx. A Plan pick in the draft then
/// waits unused, and a thread left in Plan goes back to Build with this message, so a hidden
/// control never leaves a turn planning.
pub fn send_plan(
    config: Option<&Value>,
    thread: &Value,
    draft: &Choice,
    plan_mode_enabled: bool,
) -> Result<Plan> {
    // `models::plan` checks a mode pick as Build, since whether Plan is allowed depends on the
    // provider the draft ends up on. It still refuses a provider T3 has turned off.
    let mut choice = draft.clone();
    let picked = choice.interaction_mode.take();
    choice.interaction_mode = picked.as_ref().map(|_| "default".to_string());
    let mut plan = match config {
        Some(config) if !choice.is_empty() => models::plan(config, thread, &choice)?,
        _ => Plan::default(),
    };
    let selection = plan
        .model_selection
        .as_ref()
        .unwrap_or(&thread["modelSelection"]);
    let current = thread["interactionMode"].as_str().unwrap_or("default");
    let mode = if offers_plan(config, selection, plan_mode_enabled) {
        picked.as_deref().unwrap_or(current)
    } else {
        "default"
    };
    // Only a change is sent, so a thread already in Build gets no command.
    plan.interaction_mode = (mode != current).then(|| mode.to_string());
    Ok(plan)
}

/// Whether a message would change the same with or without `draft`, so the draft can go. The
/// switch to Build that every message sends a thread left in Plan doesn't keep a draft alive.
pub fn spent(
    config: &Value,
    thread: &Value,
    draft: &Choice,
    plan_mode_enabled: bool,
) -> Result<bool> {
    let with = send_plan(Some(config), thread, draft, plan_mode_enabled)?;
    let without = send_plan(Some(config), thread, &Choice::default(), plan_mode_enabled)?;
    Ok(with == without)
}

pub fn view(
    config: Option<&Value>,
    thread: &Value,
    draft: &Choice,
    plan_mode_enabled: bool,
) -> View {
    // A draft T3 would now refuse, say for a provider turned off since, shows as no draft.
    let plan = send_plan(config, thread, draft, plan_mode_enabled)
        .or_else(|_| send_plan(config, thread, &Choice::default(), plan_mode_enabled))
        .unwrap_or_default();
    let selection = plan
        .model_selection
        .clone()
        .unwrap_or_else(|| thread["modelSelection"].clone());
    View {
        offers_plan: offers_plan(config, &selection, plan_mode_enabled),
        selection,
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
    if view.offers_plan {
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

    /// The legacy plan setting, off by default as in the nightly.
    const PLAN_OFF: bool = false;
    const PLAN_ON: bool = true;

    fn thread() -> Value {
        json!({
            "modelSelection": {"instanceId": "claudeAgent", "model": "claude-opus-5-5",
                               "options": [{"id": "effort", "value": "low"}]},
            "runtimeMode": "approval-required",
            "interactionMode": "default"
        })
    }

    /// A thread put in Plan by the CLI, the desktop or an earlier t3term.
    fn planning() -> Value {
        let mut thread = thread();
        thread["interactionMode"] = json!("plan");
        thread
    }

    fn headings(items: &[Item]) -> Vec<&str> {
        items
            .iter()
            .filter_map(|item| match item {
                Item::Heading { text, .. } => Some(text.as_str()),
                Item::Entry { .. } => None,
            })
            .collect()
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
        let view = view(Some(&config), &thread, &Choice::default(), PLAN_OFF);
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
        let base = view(Some(&config), &thread, &draft, PLAN_OFF);
        assert_eq!(traits_label(&config, &base).as_deref(), Some("Low"));

        let traits = items(Kind::Traits, &config, &base, "");
        apply(&mut draft, &thread, &pick(&traits, "Ultrathink"));
        let ultra = view(Some(&config), &thread, &draft, PLAN_OFF);
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
        let high = view(Some(&config), &thread, &draft, PLAN_OFF);
        assert_eq!(high.prompt_effort, None);
        assert_eq!(traits_label(&config, &high).as_deref(), Some("High Fast"));

        // Choosing the current model again leaves only the other choices in the draft.
        let models = items(Kind::Model, &config, &high, "");
        apply(&mut draft, &thread, &pick(&models, "Claude Haiku 4.5"));
        let haiku = view(Some(&config), &thread, &draft, PLAN_OFF);
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
        let claude = view(Some(&config), &thread(), &draft, PLAN_ON);
        let items = items(Kind::Mode, &config, &claude, "");
        assert_eq!(entries(&items).len(), 4 + 2);
        apply(&mut draft, &thread(), &pick(&items, "Full access"));
        apply(&mut draft, &thread(), &pick(&items, "On"));
        let changed = view(Some(&config), &thread(), &draft, PLAN_ON);
        assert_eq!(changed.plan.runtime_mode.as_deref(), Some("full-access"));
        assert_eq!(changed.plan.interaction_mode.as_deref(), Some("plan"));
        assert_eq!(changed.interaction_mode, "plan");

        let codex_thread = json!({"modelSelection": {"instanceId": "codex", "model": "gpt-6"}});
        let codex = view(Some(&config), &codex_thread, &Choice::default(), PLAN_ON);
        assert_eq!(
            entries(&super::items(Kind::Mode, &config, &codex, "")),
            [
                ("Supervised".to_string(), false),
                ("Full access".to_string(), true)
            ]
        );
        assert_eq!(traits_label(&config, &codex), None);
    }

    #[test]
    fn plan_mode_shows_only_with_the_setting_and_a_provider_that_has_it() {
        let config = config();
        let none = Choice::default();
        // By default the menu has the access modes alone, which still work as before.
        let off = view(Some(&config), &thread(), &none, PLAN_OFF);
        assert!(!off.offers_plan);
        let menu = items(Kind::Mode, &config, &off, "");
        assert_eq!(headings(&menu), ["Access"]);
        assert_eq!(entries(&menu).len(), 4);
        let mut draft = Choice::default();
        apply(&mut draft, &thread(), &pick(&menu, "Full access"));
        assert_eq!(
            send_plan(Some(&config), &thread(), &draft, PLAN_OFF).unwrap(),
            Plan {
                runtime_mode: Some("full-access".into()),
                ..Plan::default()
            }
        );

        let on = view(Some(&config), &thread(), &none, PLAN_ON);
        assert!(on.offers_plan);
        assert_eq!(
            headings(&items(Kind::Mode, &config, &on, "")),
            ["Access", "Plan mode"]
        );

        // Codex hides the toggle, and the setting doesn't bring it back.
        let codex_thread = json!({"modelSelection": {"instanceId": "codex", "model": "gpt-6"}});
        let codex = view(Some(&config), &codex_thread, &none, PLAN_ON);
        assert!(!codex.offers_plan);
        assert_eq!(
            headings(&items(Kind::Mode, &config, &codex, "")),
            ["Access"]
        );
    }

    #[test]
    fn a_thread_left_in_plan_goes_back_to_build_unless_plan_is_offered() {
        let config = config();
        let none = Choice::default();
        let build = Plan {
            interaction_mode: Some("default".into()),
            ..Plan::default()
        };
        // Off, the next message switches it, even before T3's model list arrives, and the
        // view's mode, which the Plan chip reads, is Build.
        for list in [Some(&config), None] {
            assert_eq!(
                send_plan(list, &planning(), &none, PLAN_OFF).unwrap(),
                build
            );
            let view = view(list, &planning(), &none, PLAN_OFF);
            assert_eq!(view.interaction_mode, "default");
        }
        // On, it keeps planning and nothing more is sent.
        assert_eq!(
            send_plan(Some(&config), &planning(), &none, PLAN_ON).unwrap(),
            Plan::default()
        );
        assert_eq!(
            view(Some(&config), &planning(), &none, PLAN_ON).interaction_mode,
            "plan"
        );
        // A thread already in Build gets no command either way.
        for setting in [PLAN_OFF, PLAN_ON] {
            assert_eq!(
                send_plan(Some(&config), &thread(), &none, setting).unwrap(),
                Plan::default()
            );
        }
    }

    #[test]
    fn a_provider_without_plan_mode_sends_build_with_the_setting_on() {
        let config = config();
        // Moving a planning thread to Codex takes it to Build. The Plan pick doesn't fail the
        // draft, as it did when t3term checked it against Codex.
        let draft = Choice {
            model: Some("codex/gpt-6".into()),
            interaction_mode: Some("plan".into()),
            ..Choice::default()
        };
        let plan = send_plan(Some(&config), &planning(), &draft, PLAN_ON).unwrap();
        assert_eq!(plan.model_selection.unwrap()["instanceId"], "codex");
        assert_eq!(plan.interaction_mode.as_deref(), Some("default"));

        // A Codex thread another client left in Plan goes to Build too, but not before the
        // model list says Codex has no plan mode.
        let codex_planning = json!({"modelSelection": {"instanceId": "codex", "model": "gpt-6"},
                                    "interactionMode": "plan"});
        let none = Choice::default();
        let plan = send_plan(Some(&config), &codex_planning, &none, PLAN_ON).unwrap();
        assert_eq!(plan.interaction_mode.as_deref(), Some("default"));
        assert_eq!(
            send_plan(None, &codex_planning, &none, PLAN_ON).unwrap(),
            Plan::default()
        );

        // A provider T3 has turned off still refuses a mode pick.
        let pi_thread = json!({"modelSelection": {"instanceId": "pi", "model": "pi-1"}});
        let plan_on = Choice {
            interaction_mode: Some("plan".into()),
            ..Choice::default()
        };
        assert!(send_plan(Some(&config), &pi_thread, &plan_on, PLAN_ON).is_err());
    }

    #[test]
    fn a_draft_is_spent_once_the_thread_has_it_though_plan_still_switches() {
        let config = config();
        let mut draft = Choice::default();
        apply(&mut draft, &planning(), &Pick::Mode("full-access".into()));
        assert!(!spent(&config, &planning(), &draft, PLAN_OFF).unwrap());
        // The thread now runs with full access but is still in Plan. The switch to Build is
        // every message's, so it doesn't keep the draft around to undo a later change.
        let mut applied = planning();
        applied["runtimeMode"] = json!("full-access");
        assert!(spent(&config, &applied, &draft, PLAN_OFF).unwrap());
        assert_eq!(
            send_plan(Some(&config), &applied, &draft, PLAN_OFF)
                .unwrap()
                .interaction_mode
                .as_deref(),
            Some("default")
        );
    }
}
