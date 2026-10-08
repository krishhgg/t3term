//! Messages that failed to send, kept so that none is lost and none goes out twice.
//!
//! A send that times out can still reach T3 once the server recovers. Each failed message keeps
//! the id it went out under, even after Ctrl+R brings it back, so when the thread later shows
//! that id, the copy kept for a retry comes out.

use std::collections::HashMap;

use super::composer::Composer;

/// A message that failed to send. `message_id` is the id it went out under, or `None` for
/// composer text that never went out.
#[derive(Clone, Debug, PartialEq)]
pub struct Failed {
    pub message_id: Option<String>,
    pub text: String,
}

/// Failed messages now in the composer, and the text they made there.
struct Restored {
    thread_id: String,
    messages: Vec<Failed>,
    text: String,
}

#[derive(Default)]
pub struct Unsent {
    /// Failed messages waiting for Ctrl+R or for their thread to open, by thread.
    saved: HashMap<String, Vec<Failed>>,
    restored: Option<Restored>,
}

fn join(messages: &[Failed]) -> String {
    messages
        .iter()
        .map(|m| m.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

impl Unsent {
    pub fn has_saved(&self, thread_id: &str) -> bool {
        self.saved.contains_key(thread_id)
    }

    /// Keeps a failed message until Ctrl+R or its thread brings it back.
    pub fn save(&mut self, thread_id: &str, message: Failed) {
        self.saved
            .entry(thread_id.to_string())
            .or_default()
            .push(message);
    }

    /// Replaces the composer's text with failed messages and remembers which they were.
    pub fn restore(&mut self, composer: &mut Composer, thread_id: &str, messages: Vec<Failed>) {
        composer.clear();
        composer.insert_str(&join(&messages));
        self.restored = Some(Restored {
            thread_id: thread_id.to_string(),
            messages,
            // What the composer holds, which drops carriage returns.
            text: composer.text(),
        });
    }

    /// Brings a thread's saved messages into the composer if it is empty, as when the thread
    /// opens.
    pub fn restore_saved(&mut self, composer: &mut Composer, thread_id: &str) {
        if composer.is_empty()
            && let Some(messages) = self.saved.remove(thread_id)
        {
            self.restore(composer, thread_id, messages);
        }
    }

    /// Swaps the composer's text with the thread's saved messages, so neither is lost. Returns
    /// false when the thread has none.
    pub fn swap(&mut self, composer: &mut Composer, thread_id: &str) -> bool {
        let Some(messages) = self.saved.remove(thread_id) else {
            return false;
        };
        let current = composer.text();
        let outgoing = match self.restored.take() {
            // Failed messages that are still as they came back keep their ids.
            Some(r) if r.thread_id == thread_id && r.text == current => r.messages,
            _ if current.trim().is_empty() => Vec::new(),
            _ => vec![Failed {
                message_id: None,
                text: current,
            }],
        };
        if !outgoing.is_empty() {
            self.saved.insert(thread_id.to_string(), outgoing);
        }
        self.restore(composer, thread_id, messages);
        true
    }

    /// The composer's text went out or was cleared, so the failed messages it came from are no
    /// longer tracked there.
    pub fn forget_composer(&mut self) {
        self.restored = None;
    }

    /// Removes every kept copy of a message the thread now shows. `in_thread` says whether the
    /// thread has a message id. Returns a status line note and whether it is a warning.
    pub fn drop_landed(
        &mut self,
        composer: &mut Composer,
        thread_id: &str,
        in_thread: impl Fn(&str) -> bool,
    ) -> Option<(String, bool)> {
        let landed = |m: &Failed| m.message_id.as_deref().is_some_and(&in_thread);
        let mut removed = None;
        if let Some(saved) = self.saved.get_mut(thread_id) {
            let before = saved.len();
            saved.retain(|m| !landed(m));
            if saved.len() < before {
                removed = Some("dropped its saved copy");
            }
            if saved.is_empty() {
                self.saved.remove(thread_id);
            }
        }
        let mut warning = None;
        if let Some(restored) = self.restored.as_mut().filter(|r| r.thread_id == thread_id)
            && restored.messages.iter().any(landed)
        {
            restored.messages.retain(|m| !landed(m));
            let current = composer.text();
            if current == restored.text {
                composer.clear();
                composer.insert_str(&join(&restored.messages));
                restored.text = composer.text();
                removed = Some("took it out of the composer");
            } else if !current.trim().is_empty() {
                // The text has changed since it came back, so only the user can take it out.
                warning = Some(
                    "A message you brought back reached T3 after all. Check the composer before you send."
                        .to_string(),
                );
            }
            if restored.messages.is_empty() {
                self.restored = None;
            }
        }
        warning.map(|w| (w, true)).or_else(|| {
            removed.map(|r| {
                (
                    format!("A message that failed reached T3 after all, so t3term {r}."),
                    false,
                )
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed(id: &str, text: &str) -> Failed {
        Failed {
            message_id: Some(id.to_string()),
            text: text.to_string(),
        }
    }

    fn composer(text: &str) -> Composer {
        let mut composer = Composer::default();
        composer.insert_str(text);
        composer
    }

    #[test]
    fn messages_keep_their_ids_through_ctrl_r() {
        let mut unsent = Unsent::default();
        let mut composer = composer("my draft");
        unsent.save("t", failed("m1", "one"));
        unsent.save("t", failed("m2", "two"));

        assert!(unsent.swap(&mut composer, "t"));
        assert_eq!(composer.text(), "one\n\ntwo");

        // The first one reaches T3 late: it leaves the composer, the second stays.
        let note = unsent.drop_landed(&mut composer, "t", |id| id == "m1");
        assert_eq!(composer.text(), "two");
        assert!(note.is_some_and(|(text, warning)| text.contains("composer") && !warning));

        // Swapping back keeps the second one's id, so it can still come out on its own.
        assert!(unsent.swap(&mut composer, "t"));
        assert_eq!(composer.text(), "my draft");
        let note = unsent.drop_landed(&mut composer, "t", |id| id == "m2");
        assert!(note.is_some_and(|(text, _)| text.contains("saved copy")));
        assert!(!unsent.has_saved("t"));
        assert_eq!(composer.text(), "my draft");
    }

    #[test]
    fn a_message_already_in_the_thread_goes_as_soon_as_it_is_saved() {
        // The thread update can arrive before the failed send's result.
        let mut unsent = Unsent::default();
        let mut composer = composer("next message");
        unsent.save("t", failed("m1", "one"));
        assert!(unsent.drop_landed(&mut composer, "t", |_| true).is_some());
        assert!(!unsent.has_saved("t"));
    }

    #[test]
    fn edited_text_is_left_alone_with_a_warning() {
        let mut unsent = Unsent::default();
        let mut composer = Composer::default();
        unsent.restore(&mut composer, "t", vec![failed("m1", "one")]);
        composer.insert_str(" more");

        let note = unsent.drop_landed(&mut composer, "t", |_| true);
        assert!(note.is_some_and(|(_, warning)| warning));
        assert_eq!(composer.text(), "one more");
        // Another thread's messages are not checked against this one.
        unsent.save("other", failed("m2", "two"));
        assert!(unsent.drop_landed(&mut composer, "t", |_| true).is_none());
        assert!(unsent.has_saved("other"));
    }

    #[test]
    fn text_that_went_out_stops_being_tracked() {
        let mut unsent = Unsent::default();
        let mut composer = Composer::default();
        unsent.restore(&mut composer, "t", vec![failed("m1", "one")]);
        unsent.forget_composer();
        composer.clear();
        assert!(unsent.drop_landed(&mut composer, "t", |_| true).is_none());
    }
}
