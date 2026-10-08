//! Messages that failed to send, kept so that none is lost and none goes out twice.
//!
//! A send that times out can still reach T3 once the server recovers. Each failed message keeps
//! its thread and the id it went out under wherever Ctrl+R moves it, so when that thread later
//! shows the id, the copy kept for a retry comes out.

use std::collections::HashMap;

use super::composer::Composer;

/// A message that failed to send. `message_id` is the id it went out under, or `None` for
/// composer text that never went out.
#[derive(Clone, Debug, PartialEq)]
pub struct Failed {
    /// The thread it was sent to.
    pub thread_id: String,
    pub message_id: Option<String>,
    pub text: String,
}

impl Failed {
    /// Whether this is a message that `thread_id` now shows.
    fn landed(&self, thread_id: &str, in_thread: impl Fn(&str) -> bool) -> bool {
        self.thread_id == thread_id && self.message_id.as_deref().is_some_and(in_thread)
    }
}

/// Failed messages now in the composer, and the text they made there.
struct Restored {
    messages: Vec<Failed>,
    text: String,
}

#[derive(Default)]
pub struct Unsent {
    /// Failed messages waiting for Ctrl+R or for a thread to open, by that thread. Ctrl+R can
    /// put one thread's message in another's list.
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
    pub fn restore(&mut self, composer: &mut Composer, messages: Vec<Failed>) {
        composer.clear();
        composer.insert_str(&join(&messages));
        self.restored = Some(Restored {
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
            self.restore(composer, messages);
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
            // Failed messages that are still as they came back keep their thread and id, even
            // when they came from another thread.
            Some(r) if r.text == current => r.messages,
            _ if current.trim().is_empty() => Vec::new(),
            _ => vec![Failed {
                thread_id: thread_id.to_string(),
                message_id: None,
                text: current,
            }],
        };
        if !outgoing.is_empty() {
            self.saved.insert(thread_id.to_string(), outgoing);
        }
        self.restore(composer, messages);
        true
    }

    /// The composer's text went out or was cleared, so the failed messages it came from are no
    /// longer tracked there.
    pub fn forget_composer(&mut self) {
        self.restored = None;
    }

    /// Removes every kept copy of a message the thread now shows, wherever Ctrl+R has put it.
    /// `in_thread` says whether the thread has a message id. Returns a status line note and
    /// whether it is a warning.
    pub fn drop_landed(
        &mut self,
        composer: &mut Composer,
        thread_id: &str,
        in_thread: impl Fn(&str) -> bool,
    ) -> Option<(String, bool)> {
        let landed = |m: &Failed| m.landed(thread_id, &in_thread);
        let mut removed = None;
        for saved in self.saved.values_mut() {
            let before = saved.len();
            saved.retain(|m| !landed(m));
            if saved.len() < before {
                removed = Some("dropped its saved copy");
            }
        }
        self.saved.retain(|_, saved| !saved.is_empty());
        let mut warning = None;
        if let Some(restored) = self.restored.as_mut()
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
        sent_to("t", id, text)
    }

    fn sent_to(thread_id: &str, id: &str, text: &str) -> Failed {
        Failed {
            thread_id: thread_id.to_string(),
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
        unsent.restore(&mut composer, vec![failed("m1", "one")]);
        composer.insert_str(" more");

        let note = unsent.drop_landed(&mut composer, "t", |_| true);
        assert!(note.is_some_and(|(_, warning)| warning));
        assert_eq!(composer.text(), "one more");
        // Another thread's messages are not checked against this one.
        unsent.save("other", sent_to("other", "m2", "two"));
        assert!(unsent.drop_landed(&mut composer, "t", |_| true).is_none());
        assert!(unsent.has_saved("other"));
    }

    #[test]
    fn text_that_went_out_stops_being_tracked() {
        let mut unsent = Unsent::default();
        let mut composer = Composer::default();
        unsent.restore(&mut composer, vec![failed("m1", "one")]);
        unsent.forget_composer();
        composer.clear();
        assert!(unsent.drop_landed(&mut composer, "t", |_| true).is_none());
    }

    #[test]
    fn a_message_swapped_into_another_thread_keeps_its_thread_and_id() {
        // A failed message for thread a is in the composer, and thread b has its own.
        let mut unsent = Unsent::default();
        let mut composer = Composer::default();
        unsent.restore(&mut composer, vec![sent_to("a", "m1", "for a")]);
        unsent.save("b", sent_to("b", "m2", "for b"));

        // Ctrl+R in b swaps them, so a's message waits in b's list.
        assert!(unsent.swap(&mut composer, "b"));
        assert_eq!(composer.text(), "for b");
        assert!(unsent.has_saved("b"));

        // b showing the same id doesn't count. a showing it does.
        assert!(
            unsent
                .drop_landed(&mut composer, "b", |id| id == "m1")
                .is_none()
        );
        let note = unsent.drop_landed(&mut composer, "a", |id| id == "m1");
        assert!(note.is_some_and(|(text, _)| text.contains("saved copy")));
        assert!(!unsent.has_saved("b"));
        assert_eq!(composer.text(), "for b");
    }
}
