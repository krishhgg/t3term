//! The provider status banner over the top of the conversation, as the nightly desktop draws
//! `ProviderStatusBanner` (`apps/web/src/components/chat/ProviderStatusBanner.tsx`). It shows
//! when the provider instance the composer would send the next message with isn't ready, or
//! runs a version T3 knows is broken or doesn't support. When it shows, what it says and how
//! severe it is follow that file. Dismissing it hides that notice until the selected provider
//! has none, as `ChatView.tsx` keeps one dismissed key for the window. It changes nothing on
//! T3. t3term has no provider setup, so the banner's advice names T3 Code's.
//!
//! It lies over the transcript's top rows with the thread error banner under it, as the
//! desktop stacks the two, and `banner::Card` draws both.

use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use serde_json::Value;

use super::banner::{
    Answer, Card, Drawn, Keys, Look, MAX_BYTES, head, key_answer, mouse_answer, printable, prints,
};
use super::theme::Theme;
use crate::models::Choice;

/// Alt+O opens or closes the whole message and Alt+N dismisses the banner. The thread error
/// banner under it has Alt+I and Alt+W, and the rest of the TUI uses Alt with A, S, D, T, M, E
/// and P.
const KEYS: Keys = Keys {
    more: 'O',
    dismiss: 'N',
};

/// A provider's states, as `ServerProviderState` lists them.
const STATES: [&str; 4] = ["ready", "warning", "error", "disabled"];
/// Whether a provider is signed in, as `ServerProviderAuthStatus` lists it.
const AUTH_STATUSES: [&str; 3] = ["authenticated", "unauthenticated", "unknown"];
/// T3's verdicts on a provider's version, as `ServerProviderCompatibilityStatus` lists them.
const VERDICTS: [&str; 5] = ["unknown", "supported", "graceful", "unsupported", "broken"];

/// What the banner reads of a provider in T3's config, borrowed from it.
#[derive(Debug, Clone, Copy)]
struct Status<'a> {
    instance_id: &'a str,
    driver: &'a str,
    /// The name T3 gives the instance, trimmed. None when it gives none.
    display_name: Option<&'a str>,
    installed: bool,
    version: Option<&'a str>,
    /// One of `STATES`.
    state: &'a str,
    /// One of `AUTH_STATUSES`.
    auth: &'a str,
    message: Option<&'a str>,
    /// Whether T3 Code's provider setup can sign in to the provider or install it.
    setup: bool,
    compatibility: Option<Advisory<'a>>,
}

/// T3's verdict on the provider's version, its `compatibilityAdvisory`.
#[derive(Debug, Clone, Copy)]
struct Advisory<'a> {
    /// One of `VERDICTS`.
    status: &'a str,
    message: Option<&'a str>,
}

/// `value` as text, unless it isn't text or is blank.
fn text(value: &Value) -> Option<&str> {
    value.as_str().filter(|text| !text.trim().is_empty())
}

/// `value` as text, when it is one of `known`.
fn one_of<'a>(value: &'a Value, known: &[&str]) -> Option<&'a str> {
    value.as_str().filter(|text| known.contains(text))
}

/// Reads `provider` for the fields the banner uses, as the desktop decodes `ServerProvider`.
/// The desktop drops a provider it can't decode from the list. So a provider without an
/// instance id, a driver or `installed`, or with a state, an auth status or a version verdict
/// this build doesn't know, reads as none here. So does one T3 has turned off, or whose driver
/// this T3 build doesn't ship, since the composer can't send with either. A text field that
/// isn't text, or is blank, counts as absent.
fn read(provider: &Value) -> Option<Status<'_>> {
    if provider["enabled"] == false || provider["availability"] == "unavailable" {
        return None;
    }
    let compatibility = match &provider["compatibilityAdvisory"] {
        Value::Null => None,
        advisory => Some(Advisory {
            status: one_of(&advisory["status"], &VERDICTS)?,
            message: text(&advisory["message"]),
        }),
    };
    let setup = &provider["setup"];
    Some(Status {
        instance_id: text(&provider["instanceId"])?,
        driver: text(&provider["driver"])?,
        display_name: text(&provider["displayName"]).map(str::trim),
        installed: provider["installed"].as_bool()?,
        version: text(&provider["version"]),
        state: one_of(&provider["status"], &STATES)?,
        auth: one_of(&provider["auth"]["status"], &AUTH_STATUSES)?,
        message: text(&provider["message"]),
        setup: setup["canAuthenticate"] == true || setup["canInstall"] == true,
        compatibility,
    })
}

/// A driver's id in words, as `formatProviderDriverKindLabel` (`apps/web/src/providerModels.ts`)
/// writes it: `claudeAgent` is Claude Agent and `open_code` is Open Code.
fn driver_label(driver: &str) -> String {
    let mut spaced = String::with_capacity(driver.len() + 4);
    let mut previous: Option<char> = None;
    for c in driver.chars() {
        if c == '_' || c == '-' {
            if !matches!(previous, Some('_' | '-')) {
                spaced.push(' ');
            }
        } else {
            if previous.is_some_and(|p| p.is_ascii_lowercase()) && c.is_ascii_uppercase() {
                spaced.push(' ');
            }
            spaced.push(c);
        }
        previous = Some(c);
    }
    // JavaScript's `\b\w` starts a word at a letter, digit or underscore after anything else.
    let mut label = String::with_capacity(spaced.len());
    let mut in_word = false;
    for c in spaced.trim().chars() {
        let word = c.is_ascii_alphanumeric() || c == '_';
        label.push(if word && !in_word {
            c.to_ascii_uppercase()
        } else {
            c
        });
        in_word = word;
    }
    label
}

/// The provider's name: the one T3 gives it, else its driver's in words.
fn name(status: &Status) -> String {
    match status.display_name {
        Some(name) => head(name, MAX_BYTES).to_string(),
        None => driver_label(head(status.driver, MAX_BYTES)),
    }
}

/// The verdict that makes the provider's version worth a notice, as `getIncompatibleVersion`
/// finds it: a version known to be broken, or one T3 doesn't support on a provider that is
/// otherwise ready. Neither counts for a provider that has to sign in again.
fn incompatible<'a>(status: &Status<'a>) -> Option<Advisory<'a>> {
    if status.state == "error" && status.auth == "unauthenticated" {
        return None;
    }
    status.compatibility.filter(|advisory| {
        advisory.status == "broken" || (status.state == "ready" && advisory.status == "unsupported")
    })
}

/// What the banner says of a provider, as `getProviderStatusMessage` words it. The desktop's
/// advice to open provider setup names T3 Code's here, since t3term has none.
fn status_message(status: &Status) -> String {
    // Advice for a broken version comes before the startup failure it can cause.
    if status.auth != "unauthenticated"
        && let Some(Advisory {
            status: "broken",
            message: Some(message),
        }) = status.compatibility
    {
        return message.to_string();
    }
    if let Some(message) = status.message {
        return message.to_string();
    }
    let setup = status.driver == "antigravity" || status.setup;
    if !status.installed && setup {
        let driver = driver_label(head(status.driver, MAX_BYTES));
        return format!("Open provider setup in T3 Code to install {driver} on this environment.");
    }
    if status.auth == "unauthenticated" {
        let advice = match (setup, status.driver) {
            (false, _) => "Sign in via the CLI to authenticate again.",
            (true, "antigravity") => "Open provider setup in T3 Code to sign in with Google.",
            (true, _) => "Open provider setup in T3 Code to sign in.",
        };
        return advice.to_string();
    }
    let name = name(status);
    match status.state {
        "ready" => "No models are available for this provider.".to_string(),
        "error" => format!("{name} provider is unavailable."),
        _ => format!("{name} provider has limited availability."),
    }
}

/// A notice's identity, which dismissing it goes by: the provider's raw values, as
/// `getProviderStatusBannerKey` takes them. The desktop joins them into one string with NUL
/// characters between.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Key {
    /// A version known to be broken, or unsupported on a ready provider: the instance, the
    /// verdict, the version and the verdict's message.
    Compatibility([String; 4]),
    /// A provider that isn't ready: the instance, its state, its auth status and its message.
    Status([String; 4]),
}

/// What the banner says of a provider, before it is cut and cleaned.
#[derive(Debug, Clone, PartialEq)]
struct Notice {
    key: Key,
    title: String,
    message: String,
    /// Drawn in the warning color rather than the error color.
    warning: bool,
}

/// The notice for `status`, as `getProviderStatusBannerKey` and the component work it out, or
/// None when the provider needs none.
fn notice(status: &Status) -> Option<Notice> {
    if status.state == "disabled" {
        return None;
    }
    let incompatible = incompatible(status);
    let key = match incompatible {
        Some(advisory) => Key::Compatibility(
            [
                status.instance_id,
                advisory.status,
                status.version.unwrap_or_default(),
                advisory.message.unwrap_or_default(),
            ]
            .map(String::from),
        ),
        None if status.state == "ready" => return None,
        // Antigravity checks its saved sign-in only when a session starts, so after a restart
        // its health check leaves the auth status unknown, which is no failure.
        None if status.driver == "antigravity"
            && status.installed
            && status.state == "warning"
            && status.auth == "unknown" =>
        {
            return None;
        }
        None => Key::Status(
            [
                status.instance_id,
                status.state,
                status.auth,
                status.message.unwrap_or_default(),
            ]
            .map(String::from),
        ),
    };
    let name = name(status);
    let title = if status.state == "error" && status.auth == "unauthenticated" {
        format!("{name} is unauthenticated")
    } else if let Some(advisory) = incompatible {
        // The desktop's markup collapses the double space a missing version leaves.
        let version = status
            .version
            .map(|version| format!(" {}", head(version, MAX_BYTES)))
            .unwrap_or_default();
        let verdict = if advisory.status == "broken" {
            "known to be broken"
        } else {
            "unsupported"
        };
        format!("{name}{version} is {verdict}")
    } else {
        format!("{name} provider status")
    };
    let message = match incompatible.and_then(|advisory| advisory.message) {
        Some(message) => message.to_string(),
        None => status_message(status),
    };
    let warning = incompatible.is_none_or(|advisory| advisory.status != "broken")
        && (status.state == "warning" || incompatible.is_some());
    Some(Notice {
        key,
        title,
        message,
        warning,
    })
}

/// What the composer's selection was last worked out from: the open thread, its saved model
/// selection and provider instance, and its unsent choice. A change to T3's config works it
/// out again too.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Inputs {
    thread_id: String,
    model_selection: Value,
    provider_instance: Value,
    draft: Option<Choice>,
}

impl Inputs {
    pub fn new(thread_id: &str, thread: &Value, draft: Option<&Choice>) -> Inputs {
        Inputs {
            thread_id: thread_id.to_string(),
            model_selection: thread["modelSelection"].clone(),
            provider_instance: thread["providerInstanceId"].clone(),
            draft: draft.cloned(),
        }
    }

    /// Whether these are the inputs for `thread_id`, its record `thread` and its `draft`,
    /// compared without copying them.
    fn matches(&self, thread_id: &str, thread: &Value, draft: Option<&Choice>) -> bool {
        self.thread_id == thread_id
            && self.model_selection == thread["modelSelection"]
            && self.provider_instance == thread["providerInstanceId"]
            && self.draft.as_ref() == draft
    }
}

/// The notice on show and what the banner made of it.
#[derive(Debug)]
struct Shown {
    notice: Notice,
    /// The title and the message as the banner prints them, cut to `MAX_BYTES` and cleaned.
    /// The message is empty when nothing of it is left to print.
    title: String,
    text: String,
    card: Card,
}

/// The provider banner's state for the session. Only this window keeps it: nothing goes to T3
/// or the settings file.
#[derive(Debug, Default)]
pub(super) struct Banner {
    /// What the selection was last worked out from. None with no thread open.
    inputs: Option<Inputs>,
    /// The notice the reader dismissed. It stays hidden while the provider gives it, and comes
    /// back after another notice, until a selection with no notice clears it, as the
    /// desktop's `dismissedProviderStatusBannerKey` does.
    dismissed: Option<Key>,
    shown: Option<Shown>,
    /// Where the last frame drew the banner and its close button, for the mouse. Empty when
    /// it drew none.
    pub area: Rect,
    pub close: Rect,
    /// Whether the last frame cut the closed message short.
    clipped: bool,
    /// Whether the last frame's open message was too long for its rows.
    overflows: bool,
    /// How many times the banner followed a selection worked out again, and cleaned a notice
    /// it didn't have, so the tests can tell what an event or a frame did.
    #[cfg(test)]
    pub selects: usize,
    #[cfg(test)]
    pub prepares: usize,
}

impl Banner {
    /// Whether the composer's selection has to be worked out again, though T3's config is as it
    /// was, for `open`: the open thread's id, its record and its unsent choice, or None with no
    /// thread open.
    pub fn stale(&self, open: Option<(&str, &Value, Option<&Choice>)>) -> bool {
        match (&self.inputs, open) {
            (None, None) => false,
            (Some(inputs), Some((id, thread, draft))) => !inputs.matches(id, thread, draft),
            _ => true,
        }
    }

    /// Brings the banner up to date with `provider`, the status in T3's config of the provider
    /// instance the composer would send with, which the caller worked out from `inputs`. The
    /// same notice keeps the banner as it was, open, scrolled or dismissed, after one
    /// comparison. Only a notice the banner doesn't have is cut and cleaned. No provider, or
    /// one with no notice, hides the banner and clears the dismissal.
    pub fn follow(&mut self, inputs: Option<Inputs>, provider: Option<&Value>) {
        self.inputs = inputs;
        #[cfg(test)]
        {
            self.selects += 1;
        }
        let Some(notice) = provider.and_then(read).as_ref().and_then(notice) else {
            self.dismissed = None;
            self.shown = None;
            return;
        };
        if self
            .shown
            .as_ref()
            .is_some_and(|shown| shown.notice == notice)
        {
            return;
        }
        if self.dismissed.as_ref() == Some(&notice.key) {
            self.shown = None;
            return;
        }
        #[cfg(test)]
        {
            self.prepares += 1;
        }
        let title = printable(&notice.title, "title");
        let text = Some(printable(&notice.message, "message"))
            .filter(|text| prints(text))
            .unwrap_or_default();
        self.shown = Some(Shown {
            notice,
            title,
            text,
            card: Card::default(),
        });
    }

    /// Whether the banner has a notice to print, drawn or not.
    pub fn prints(&self) -> bool {
        self.shown.is_some()
    }

    /// Whether the last frame drew the banner, which is when its keys and clicks apply.
    pub fn showing(&self) -> bool {
        self.prints() && self.area.height > 0
    }

    /// Whether the reader opened the whole message.
    pub fn expanded(&self) -> bool {
        self.shown.as_ref().is_some_and(|shown| shown.card.expanded)
    }

    /// Closes the message back to its first rows, as when the reader opens the thread's
    /// error. It no longer scrolls, so Alt+↑/↓ go on before the next frame.
    pub fn collapse(&mut self) {
        if let Some(shown) = self.shown.as_mut().filter(|shown| shown.card.expanded) {
            shown.card.toggle();
        }
        self.overflows = false;
    }

    /// Hides the notice on show, as the desktop's close button does. The banner keeps the
    /// notice's key, so following the same notice again only compares it.
    pub fn dismiss(&mut self) {
        if let Some(shown) = self.shown.take() {
            self.dismissed = Some(shown.notice.key);
        }
        self.drawn(Drawn::default());
    }

    /// Does what the reader asked with a key or the mouse, and says whether it changed the
    /// banner.
    fn answer(&mut self, answer: Answer) -> bool {
        if answer == Answer::Dismiss {
            self.dismiss();
            return true;
        }
        let Some(shown) = self.shown.as_mut() else {
            return false;
        };
        match answer {
            Answer::Toggle => shown.card.toggle(),
            Answer::Scroll { down } => shown.card.scroll(down),
            Answer::Dismiss | Answer::Nothing => return false,
        }
        true
    }

    /// Alt+N dismisses the banner and Alt+O opens or closes the whole message, while the
    /// banner shows. Alt+↑/↓ scroll an open message too long for its rows. Returns whether
    /// the key was the banner's. With no banner on screen the keys go on.
    pub fn on_key(&mut self, key: &KeyEvent) -> bool {
        if !self.showing() {
            return false;
        }
        let Some(answer) = key_answer(key, KEYS, self.overflows) else {
            return false;
        };
        self.answer(answer);
        true
    }

    /// The mouse on the banner, as `banner::mouse_answer` has it. Returns None for events
    /// outside the banner and wheel events it leaves to the transcript, else whether the
    /// banner changed.
    pub fn on_mouse(&mut self, mouse: &MouseEvent) -> Option<bool> {
        if !self.showing() {
            return None;
        }
        let drawn = Drawn {
            area: self.area,
            close: self.close,
            clipped: self.clipped,
            overflows: self.overflows,
        };
        let answer = mouse_answer(mouse, drawn, self.expanded())?;
        Some(self.answer(answer))
    }

    /// Draws the banner over the top rows of `area`, the transcript's, in at most `limit` of
    /// them, as `Card::draw` does. Its title is bold and its icon is `i`, the desktop's info
    /// icon.
    pub fn draw(&mut self, frame: &mut Frame, area: Rect, limit: usize, theme: &Theme) {
        let Some(shown) = self.shown.as_mut() else {
            self.drawn(Drawn::default());
            return;
        };
        let look = Look {
            title: &shown.title,
            text: &shown.text,
            warning: shown.notice.warning,
            icon: 'i',
            keys: KEYS,
        };
        let drawn = shown.card.draw(frame, area, limit, theme, &look);
        self.drawn(drawn);
    }

    fn drawn(&mut self, drawn: Drawn) {
        self.area = drawn.area;
        self.close = drawn.close;
        self.clipped = drawn.clipped;
        self.overflows = drawn.overflows;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::Depth;
    use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Position;
    use ratatui::style::Modifier;
    use serde_json::json;

    // ---- which notice a provider gives ----

    /// Codex as T3 reports it, ready and signed in on version 1.0.0.
    fn codex(state: &str) -> Value {
        json!({
            "instanceId": "codex",
            "driver": "codex",
            "displayName": "Codex",
            "enabled": true,
            "installed": true,
            "version": "1.0.0",
            "status": state,
            "auth": {"status": "authenticated"},
            "checkedAt": "2026-10-10T12:00:00.000Z",
            "models": [],
        })
    }

    /// Codex on a version T3 doesn't support, as the desktop's own test has it.
    fn unsupported() -> Value {
        let mut provider = codex("ready");
        provider["instanceId"] = json!("codex-work");
        provider["compatibilityAdvisory"] = json!({
            "status": "unsupported",
            "message": "Unsupported version. Use 2.0.0.",
            "recommendedVersion": "2.0.0",
            "recommendedRange": null,
        });
        provider
    }

    fn notice_of(provider: &Value) -> Option<Notice> {
        read(provider).as_ref().and_then(notice)
    }

    fn key_of(provider: &Value) -> Option<Key> {
        notice_of(provider).map(|notice| notice.key)
    }

    fn message_of(provider: &Value) -> String {
        status_message(&read(provider).expect("a provider the banner reads"))
    }

    /// Whether a banner that dismissed `dismissed` shows `provider`, as the desktop's
    /// `shouldShowProviderStatusBanner` has it.
    fn shows(provider: &Value, dismissed: Option<&Value>) -> bool {
        let mut banner = Banner::default();
        if let Some(dismissed) = dismissed {
            banner.follow(None, Some(dismissed));
            banner.dismiss();
        }
        banner.follow(None, Some(provider));
        banner.prints()
    }

    #[test]
    fn an_unsupported_version_warns_on_a_ready_provider_until_t3_relaxes_its_policy() {
        let provider = unsupported();
        let notice = notice_of(&provider).unwrap();
        assert_eq!(notice.title, "Codex 1.0.0 is unsupported");
        assert_eq!(notice.message, "Unsupported version. Use 2.0.0.");
        assert!(notice.warning);
        assert!(shows(&provider, None));
        assert!(!shows(&provider, Some(&provider)));
        let mut newer = provider.clone();
        newer["version"] = json!("1.0.1");
        assert!(shows(&newer, Some(&provider)));

        let mut relaxed = provider.clone();
        relaxed["compatibilityAdvisory"]["status"] = json!("supported");
        relaxed["compatibilityAdvisory"]["message"] = Value::Null;
        assert_eq!(key_of(&relaxed), None);
        let mut disabled = provider.clone();
        disabled["status"] = json!("disabled");
        assert_eq!(key_of(&disabled), None);
        let mut graceful = provider.clone();
        graceful["compatibilityAdvisory"]["status"] = json!("graceful");
        assert_eq!(key_of(&graceful), None);
        // Without a version, the title has no gap where it would be.
        let mut unknown = provider;
        unknown["version"] = Value::Null;
        assert_eq!(notice_of(&unknown).unwrap().title, "Codex is unsupported");
    }

    #[test]
    fn a_broken_version_shows_its_advice_in_place_of_the_failure_it_causes() {
        let advice = "This provider version is known to be incompatible. Use 1.14.19.";
        let mut broken = codex("error");
        broken["driver"] = json!("opencode");
        broken["displayName"] = json!("OpenCode");
        broken["version"] = json!("2.0.3");
        broken["auth"] = json!({"status": "unknown"});
        broken["message"] = json!(
            "Failed to load OpenCode provider inventory: Timed out waiting for server start."
        );
        broken["compatibilityAdvisory"] = json!({
            "status": "broken",
            "message": advice,
            "recommendedVersion": "1.14.19",
            "recommendedRange": ">=1.14.19 <2.0.0",
        });
        let notice = notice_of(&broken).unwrap();
        assert_eq!(notice.title, "OpenCode 2.0.3 is known to be broken");
        assert_eq!(notice.message, advice);
        assert!(!notice.warning, "a broken version is an error");
        assert!(shows(&broken, None));

        let mut timeout_only = broken.clone();
        timeout_only["compatibilityAdvisory"]["status"] = json!("supported");
        timeout_only["compatibilityAdvisory"]["message"] = Value::Null;
        assert!(shows(&broken, Some(&timeout_only)));
        assert!(!shows(&broken, Some(&broken)));
        let mut new_advice = broken.clone();
        new_advice["compatibilityAdvisory"]["message"] = json!("Use 1.14.20.");
        assert!(shows(&new_advice, Some(&broken)));

        assert_eq!(message_of(&broken), advice);
        assert_eq!(message_of(&timeout_only), broken["message"]);
        let mut signed_out = broken.clone();
        signed_out["auth"]["status"] = json!("unauthenticated");
        assert_eq!(message_of(&signed_out), broken["message"]);
    }

    #[test]
    fn signing_in_again_comes_before_a_version_warning() {
        let mut signed_out = unsupported();
        signed_out["status"] = json!("error");
        signed_out["auth"]["status"] = json!("unauthenticated");
        let notice = notice_of(&signed_out).unwrap();
        assert_eq!(notice.title, "Codex is unauthenticated");
        assert_eq!(notice.message, "Sign in via the CLI to authenticate again.");
        assert!(!notice.warning);
        signed_out["message"] = json!("Credentials expired");
        assert_eq!(message_of(&signed_out), "Credentials expired");
    }

    #[test]
    fn antigravity_waits_for_its_auth_result_before_warning() {
        let mut antigravity = codex("warning");
        antigravity["instanceId"] = json!("google_work");
        antigravity["driver"] = json!("antigravity");
        antigravity["auth"] = json!({"status": "unknown"});
        antigravity["message"] =
            json!("Antigravity is installed. Google account access is not checked yet.");
        assert_eq!(key_of(&antigravity), None);
        let mut signed_out = antigravity.clone();
        signed_out["auth"]["status"] = json!("unauthenticated");
        signed_out["message"] = json!("Sign in with Google to use Antigravity.");
        assert!(shows(&signed_out, None));

        // Not installed, failing to start, or another driver, it shows before auth is checked.
        let mut missing = antigravity.clone();
        missing["installed"] = json!(false);
        assert!(shows(&missing, None));
        let mut failing = antigravity.clone();
        failing["status"] = json!("error");
        assert!(shows(&failing, None));
        let mut other = antigravity;
        other["driver"] = json!("codex");
        assert!(shows(&other, None));
    }

    #[test]
    fn a_warning_reads_as_one_and_an_error_as_an_error() {
        let mut warning = codex("warning");
        warning["message"] = json!("Provider is temporarily degraded.");
        let notice = notice_of(&warning).unwrap();
        assert_eq!(notice.title, "Codex provider status");
        assert_eq!(notice.message, "Provider is temporarily degraded.");
        assert!(notice.warning);
        warning["status"] = json!("error");
        assert!(!notice_of(&warning).unwrap().warning);
        // A ready provider with nothing wrong with its version has no notice.
        assert_eq!(key_of(&codex("ready")), None);
    }

    #[test]
    fn the_messages_point_to_t3_codes_provider_setup_or_the_cli() {
        let antigravity = |message: &str| {
            let mut provider = codex("error");
            provider["driver"] = json!("antigravity");
            provider["auth"]["status"] = json!("unauthenticated");
            provider["message"] = json!(message);
            provider
        };
        let refused = "SUBSCRIPTION_REQUIRED: This Google account cannot use Antigravity.";
        assert_eq!(message_of(&antigravity(refused)), refused);
        assert_eq!(
            message_of(&antigravity("")),
            "Open provider setup in T3 Code to sign in with Google."
        );
        // Installing names the driver, not the instance's own name.
        let mut missing = antigravity("");
        missing["displayName"] = json!("Google work account");
        missing["installed"] = json!(false);
        assert_eq!(
            message_of(&missing),
            "Open provider setup in T3 Code to install Antigravity on this environment."
        );

        let mut signed_out = codex("error");
        signed_out["auth"]["status"] = json!("unauthenticated");
        assert_eq!(
            message_of(&signed_out),
            "Sign in via the CLI to authenticate again."
        );
        signed_out["setup"] = json!({"canAuthenticate": true, "canInstall": false});
        assert_eq!(
            message_of(&signed_out),
            "Open provider setup in T3 Code to sign in."
        );
        // A provider without setup that isn't installed gets no install advice.
        let mut uninstalled = codex("error");
        uninstalled["installed"] = json!(false);
        assert_eq!(message_of(&uninstalled), "Codex provider is unavailable.");
        uninstalled["setup"] = json!({"canAuthenticate": false, "canInstall": true});
        assert_eq!(
            message_of(&uninstalled),
            "Open provider setup in T3 Code to install Codex on this environment."
        );

        // With no message of its own, the provider's name says what is wrong. A provider with
        // no name of its own goes by its driver's.
        let mut nameless = codex("warning");
        nameless.as_object_mut().unwrap().remove("displayName");
        nameless["driver"] = json!("claudeAgent");
        assert_eq!(
            message_of(&nameless),
            "Claude Agent provider has limited availability."
        );
        assert_eq!(
            notice_of(&nameless).unwrap().title,
            "Claude Agent provider status"
        );
        nameless["displayName"] = json!("   ");
        assert_eq!(
            notice_of(&nameless).unwrap().title,
            "Claude Agent provider status"
        );
    }

    #[test]
    fn a_provider_the_desktop_couldnt_read_or_cant_send_with_has_no_notice() {
        fn changed(change: impl FnOnce(&mut Value)) -> Value {
            let mut provider = codex("error");
            change(&mut provider);
            provider
        }
        assert!(shows(&codex("error"), None));
        let hidden = [
            changed(|p| p["status"] = json!("degraded")),
            changed(|p| p["auth"]["status"] = json!("expired")),
            changed(|p| p["auth"] = json!("authenticated")),
            changed(|p| p["installed"] = json!("yes")),
            changed(|p| p["driver"] = json!("")),
            changed(|p| p["instanceId"] = json!(7)),
            changed(|p| p["enabled"] = json!(false)),
            changed(|p| p["availability"] = json!("unavailable")),
            changed(|p| p["compatibilityAdvisory"] = json!({"status": "retired"})),
            changed(|p| p["compatibilityAdvisory"] = json!("broken")),
        ];
        for provider in &hidden {
            assert!(!shows(provider, None), "{provider}");
        }
        // An optional field of the wrong kind counts as absent.
        let loose = changed(|p| {
            p["message"] = json!(42);
            p["displayName"] = json!(["Codex"]);
            p["version"] = json!(1);
            p["setup"] = json!("yes");
            p["enabled"] = Value::Null;
        });
        let notice = notice_of(&loose).unwrap();
        assert_eq!(notice.title, "Codex provider status");
        assert_eq!(notice.message, "Codex provider is unavailable.");
    }

    #[test]
    fn driver_ids_read_as_words() {
        for (driver, label) in [
            ("codex", "Codex"),
            ("claudeAgent", "Claude Agent"),
            ("open_code", "Open Code"),
            ("cursor--agent__x", "Cursor Agent X"),
            ("geminiCLI", "Gemini CLI"),
            ("_pi-", "Pi"),
            ("x1y", "X1y"),
            ("café-bar", "Café Bar"),
        ] {
            assert_eq!(driver_label(driver), label, "{driver}");
        }
    }

    // ---- following and dismissing ----

    fn title(banner: &Banner) -> Option<&str> {
        banner.shown.as_ref().map(|shown| shown.title.as_str())
    }

    #[test]
    fn one_dismissal_holds_until_the_selection_has_no_notice() {
        let mut warning = codex("warning");
        warning["message"] = json!("Degraded");
        let error = codex("error");
        let mut banner = Banner::default();
        banner.follow(None, Some(&warning));
        assert_eq!(title(&banner), Some("Codex provider status"));
        banner.dismiss();
        assert!(!banner.prints());
        // Another notice shows, and the dismissed one stays hidden when it comes back.
        banner.follow(None, Some(&error));
        assert!(banner.prints());
        banner.follow(None, Some(&warning));
        assert!(!banner.prints());
        // A ready provider has no notice, which clears the dismissal.
        banner.follow(None, Some(&codex("ready")));
        assert!(!banner.prints());
        banner.follow(None, Some(&warning));
        assert!(banner.prints());
        // So does no provider at all.
        banner.dismiss();
        banner.follow(None, None);
        banner.follow(None, Some(&warning));
        assert!(banner.prints());
        // The same status on another instance is another notice.
        banner.dismiss();
        let mut work = warning.clone();
        work["instanceId"] = json!("codex_work");
        banner.follow(None, Some(&work));
        assert!(banner.prints());
    }

    #[test]
    fn the_same_notice_keeps_the_banner_as_it_was_and_cleans_nothing() {
        let mut warning = codex("warning");
        warning["message"] = json!("Degraded");
        let mut banner = Banner::default();
        banner.follow(None, Some(&warning));
        assert_eq!((banner.selects, banner.prepares), (1, 1));
        banner.shown.as_mut().unwrap().card.toggle();
        // T3 checks the provider again and finds it as it was.
        for second in 1..=5 {
            warning["checkedAt"] = json!(format!("2026-10-10T12:00:0{second}.000Z"));
            banner.follow(None, Some(&warning));
        }
        assert_eq!((banner.selects, banner.prepares), (6, 1));
        assert!(banner.expanded());
        // A new name changes the title but not the notice's key, so it is cleaned again and
        // starts closed.
        warning["displayName"] = json!("Codex at work");
        banner.follow(None, Some(&warning));
        assert_eq!(banner.prepares, 2);
        assert_eq!(title(&banner), Some("Codex at work provider status"));
        assert!(!banner.expanded());
        // A dismissed notice is only compared.
        banner.dismiss();
        for _ in 0..3 {
            banner.follow(None, Some(&warning));
        }
        assert_eq!(banner.prepares, 2);
    }

    #[test]
    fn dismissal_goes_by_the_whole_message_past_what_the_banner_prints() {
        let long = format!("Codex quit: {}", "界".repeat(3000));
        let mut first = codex("error");
        first["message"] = json!(long);
        let mut banner = Banner::default();
        banner.follow(None, Some(&first));
        let text = &banner.shown.as_ref().unwrap().text;
        assert!(
            text.ends_with("… the message goes on past 4096 bytes"),
            "{text}"
        );
        assert!(text.len() < 4200);
        banner.dismiss();
        banner.follow(None, Some(&first));
        assert!(!banner.prints());
        let mut second = first.clone();
        second["message"] = json!(format!("{long}!"));
        banner.follow(None, Some(&second));
        assert!(banner.prints());
    }

    #[test]
    fn the_selection_is_stale_only_when_its_inputs_change() {
        let thread =
            json!({"id": "t", "modelSelection": {"instanceId": "codex", "model": "gpt-6"}});
        let mut banner = Banner::default();
        assert!(!banner.stale(None), "no thread, and none before");
        assert!(banner.stale(Some(("t", &thread, None))));
        banner.follow(Some(Inputs::new("t", &thread, None)), None);
        assert!(!banner.stale(Some(("t", &thread, None))));
        // Another thread with the same selection, a new selection, or a draft.
        assert!(banner.stale(Some(("u", &thread, None))));
        let mut moved = thread.clone();
        moved["modelSelection"]["instanceId"] = json!("codex_work");
        assert!(banner.stale(Some(("t", &moved, None))));
        let draft = Choice {
            model: Some("claudeAgent/claude-opus-5-5".into()),
            ..Choice::default()
        };
        assert!(banner.stale(Some(("t", &thread, Some(&draft)))));
        // A title or status change on the record leaves the selection as it was.
        let mut renamed = thread.clone();
        renamed["title"] = json!("Renamed");
        renamed["status"] = json!("running");
        assert!(!banner.stale(Some(("t", &renamed, None))));
        assert!(banner.stale(None));
    }

    // ---- drawing ----

    fn draw_cells(banner: &mut Banner, width: u16, height: u16, limit: usize) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let theme = Theme::new(Depth::TrueColor);
        let frame = terminal
            .draw(|frame| {
                let area = frame.area();
                banner.draw(frame, area, limit, &theme);
            })
            .unwrap();
        frame.buffer.clone()
    }

    fn draw(banner: &mut Banner, width: u16, height: u16, limit: usize) -> Vec<String> {
        let buffer = draw_cells(banner, width, height, limit);
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    }

    fn showing(message: &str) -> Banner {
        let mut provider = codex("warning");
        provider["message"] = json!(message);
        let mut banner = Banner::default();
        banner.follow(None, Some(&provider));
        banner
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    const CLICK: MouseEventKind = MouseEventKind::Down(MouseButton::Left);

    #[test]
    fn a_notice_draws_its_title_over_its_message_and_names_its_keys() {
        let theme = Theme::new(Depth::TrueColor);
        let mut banner = showing("Provider is temporarily degraded.");
        let buffer = draw_cells(&mut banner, 80, 20, 10);
        let rows: Vec<String> = (0..20)
            .map(|y| (0..80).map(|x| buffer[(x, y)].symbol()).collect())
            .collect();
        let area = banner.area;
        assert_eq!(
            area.height, 4,
            "the border, the title, the message and the border"
        );
        assert!(area.x > 0 && area.right() < 80);
        assert!(rows[1].contains(" i Codex provider status"), "{rows:#?}");
        assert!(rows[1].contains(" × "), "{rows:#?}");
        assert!(
            rows[2].contains("Provider is temporarily degraded."),
            "{rows:#?}"
        );
        assert!(rows[3].contains("Alt+N dismiss"), "{rows:#?}");
        assert!(!rows[3].contains("Alt+O"), "nothing is cut");
        assert!(rows[4..].iter().all(|row| row.trim().is_empty()));
        // The title is bold and the message isn't, and a warning has the warning's border.
        // Both start past the border and the icon's three columns.
        let text_x = area.x + 4;
        assert_eq!(buffer[(text_x, 1)].symbol(), "C");
        assert!(buffer[(text_x, 1)].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(text_x, 2)].symbol(), "P");
        assert!(!buffer[(text_x, 2)].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(area.x, 0)].fg, theme.warning);
        // × is on the title's row.
        let close = banner.close;
        assert_eq!(close.y, 1);
        assert_eq!(buffer[(close.x + 1, 1)].symbol(), "×");
    }

    #[test]
    fn a_long_message_shows_three_rows_and_opens_with_alt_o() {
        let message = (1..=12)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut banner = showing(&message);
        let rows = draw(&mut banner, 60, 30, 15);
        assert_eq!(
            banner.area.height, 6,
            "the title, three rows and the borders"
        );
        assert!(rows[2].contains("line 1"), "{rows:#?}");
        assert!(rows[4].contains("line 3…"), "{rows:#?}");
        assert!(rows[5].contains("Alt+O more · Alt+N dismiss"), "{rows:#?}");

        let alt = KeyModifiers::ALT;
        assert!(banner.on_key(&key(KeyCode::Char('o'), alt)));
        let rows = draw(&mut banner, 60, 30, 30);
        assert_eq!(
            banner.area.height, 15,
            "the title, twelve lines and the borders"
        );
        assert!(rows[14].contains("Alt+O less"), "{rows:#?}");

        // Fewer rows, and the open message scrolls with Alt+↓ and the wheel.
        let rows = draw(&mut banner, 60, 30, 8);
        assert_eq!(banner.area.height, 8);
        assert!(rows[7].contains("Lines 1-6 of 13"), "{rows:#?}");
        assert!(banner.on_key(&key(KeyCode::Down, alt)));
        let rows = draw(&mut banner, 60, 30, 8);
        assert!(rows[7].contains("Lines 2-7 of 13"), "{rows:#?}");
        let x = banner.area.x + 2;
        assert_eq!(
            banner.on_mouse(&mouse(MouseEventKind::ScrollUp, x, 3)),
            Some(true)
        );
        let rows = draw(&mut banner, 60, 30, 8);
        assert!(rows[7].contains("Lines 1-6 of 13"), "{rows:#?}");

        // A click on it closes it, and a closed banner leaves the wheel to the transcript.
        assert_eq!(banner.on_mouse(&mouse(CLICK, x, 3)), Some(true));
        draw(&mut banner, 60, 30, 15);
        assert!(!banner.expanded());
        assert_eq!(
            banner.on_mouse(&mouse(MouseEventKind::ScrollDown, x, 2)),
            None
        );
        assert!(!banner.on_key(&key(KeyCode::Down, alt)));
    }

    #[test]
    fn its_keys_are_its_own_and_only_while_it_shows() {
        let alt = KeyModifiers::ALT;
        let mut banner = showing("Degraded");
        assert!(
            !banner.on_key(&key(KeyCode::Char('n'), alt)),
            "not drawn yet"
        );
        draw(&mut banner, 60, 20, 10);
        // The thread error banner's keys, the approvals', the menus' and the drawer's go on.
        for c in ['w', 'i', 'a', 's', 'd', 'm', 'e', 'p', 't'] {
            assert!(!banner.on_key(&key(KeyCode::Char(c), alt)), "Alt+{c}");
        }
        assert!(!banner.on_key(&key(KeyCode::Char('n'), KeyModifiers::NONE)));
        let ctrl_alt = alt | KeyModifiers::CONTROL;
        assert!(!banner.on_key(&key(KeyCode::Char('n'), ctrl_alt)));
        assert!(!banner.on_key(&key(KeyCode::Up, alt)), "nothing to scroll");
        let shift_o = key(KeyCode::Char('O'), alt | KeyModifiers::SHIFT);
        assert!(banner.on_key(&shift_o));
        assert!(banner.expanded());
        assert!(banner.on_key(&key(KeyCode::Char('n'), alt)));
        assert!(!banner.prints());
        assert_eq!(banner.area, Rect::default());
        assert!(!banner.on_key(&key(KeyCode::Char('n'), alt)));
        assert!(!banner.on_key(&key(KeyCode::Char('o'), alt)));
    }

    #[test]
    fn it_takes_its_clicks_and_closes_with_its_x() {
        let mut banner = showing(&"word ".repeat(100));
        draw(&mut banner, 60, 20, 10);
        let area = banner.area;
        assert_eq!(banner.on_mouse(&mouse(CLICK, 0, area.bottom())), None);
        let right = MouseEventKind::Down(MouseButton::Right);
        assert_eq!(banner.on_mouse(&mouse(right, area.x + 3, 2)), Some(false));
        assert_eq!(banner.on_mouse(&mouse(CLICK, area.x + 3, 2)), Some(true));
        assert!(banner.expanded());
        draw(&mut banner, 60, 20, 10);
        let close = banner.close;
        assert_eq!(banner.on_mouse(&mouse(CLICK, close.x, close.y)), Some(true));
        assert!(!banner.prints());
        assert_eq!(banner.on_mouse(&mouse(CLICK, close.x, close.y)), None);
    }

    #[test]
    fn a_frame_rewraps_only_for_a_new_width() {
        let mut banner = showing(&"word ".repeat(400));
        let wraps = |banner: &Banner| banner.shown.as_ref().unwrap().card.wraps;
        for limit in [3, 6, 10] {
            draw(&mut banner, 80, 24, limit);
        }
        assert_eq!(wraps(&banner), 1);
        banner.shown.as_mut().unwrap().card.toggle();
        draw(&mut banner, 80, 24, 24);
        assert_eq!(
            wraps(&banner),
            1,
            "the limit and opening don't change the width"
        );
        draw(&mut banner, 70, 24, 10);
        assert_eq!(wraps(&banner), 2);
    }

    #[test]
    fn long_unicode_and_control_text_stays_inside_the_banner_at_any_size() {
        let mut provider = codex("error");
        provider["displayName"] = json!(format!(
            "\u{1b}[31m👩‍💻 Codex\u{7} {}\u{202e}",
            "界".repeat(3000)
        ));
        provider["message"] = json!(format!(
            "{}\n\u{1b}]52;c;aGk=\u{7}e\u{301}\u{301} ⚠\u{fe0f}\t{}\n{}",
            "界".repeat(500),
            "a".repeat(300),
            "\n".repeat(50)
        ));
        let sizes = [
            (0, 0),
            (1, 1),
            (4, 3),
            (5, 1),
            (6, 2),
            (11, 3),
            (12, 3),
            (14, 5),
            (40, 4),
            (80, 24),
            (200, 60),
        ];
        for expanded in [false, true] {
            for (width, height) in sizes {
                for limit in [0, 1, 2, 3, height as usize, 100] {
                    let size = format!("{width}x{height} in {limit}, open: {expanded}");
                    let mut banner = Banner::default();
                    banner.follow(None, Some(&provider));
                    if expanded {
                        banner.shown.as_mut().unwrap().card.toggle();
                    }
                    let shown = banner.shown.as_ref().unwrap();
                    assert!(!shown.title.chars().any(char::is_control), "{size}");
                    assert!(shown.title.len() < 4200, "{size}");
                    let cells = draw_cells(&mut banner, width, height, limit);
                    let (area, close) = (banner.area, banner.close);
                    let screen = Rect::new(0, 0, width, height);
                    assert_eq!(area.intersection(screen), area, "{size}");
                    assert!(area.height as usize <= limit, "{size}");
                    assert_eq!(close.intersection(area), close, "{size}");
                    for y in 0..height {
                        for x in 0..width {
                            if !area.contains(Position::new(x, y)) {
                                assert_eq!(cells[(x, y)].symbol(), " ", "{size} at {x},{y}");
                            }
                        }
                    }
                    if width < 5 || height == 0 || limit == 0 {
                        assert!(!banner.showing(), "{size}");
                        continue;
                    }
                    assert!(banner.showing(), "{size}");
                    let on_close =
                        (close.x..close.right()).any(|x| cells[(x, close.y)].symbol() == "×");
                    assert!(on_close, "{size}");
                }
            }
        }
    }
}
