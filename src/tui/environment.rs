//! The T3 environment the TUI is connected to, and the state of the sidebar's thread feed,
//! which colors the dot beside it.
//!
//! The desktop's connection settings name each environment by its descriptor's `label`
//! (`ExecutionEnvironmentDescriptor` in the nightly's packages/contracts/src/environment.ts,
//! shown by `EnvironmentRow` in apps/web/src/components/settings/EnvironmentRow.tsx), with its
//! address beside it, and color a dot by the connection's phase (`connectionPhaseDotClassName`
//! in apps/web/src/components/ConnectionStatusDot.tsx). t3term has one connection and no
//! connection supervisor, so its dot follows the sidebar's feed, as `Feed` says.
//!
//! The label and the origin come from discovery, which read them before the TUI started.
//! `Identity::new` cleans and bounds them once, and a frame only fits the result to its width.

use ratatui::style::{Color, Style};
use ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

use super::clip;
use super::setup::{bounded, middle};
use super::theme::Theme;

/// The most characters of the label kept. T3 sets the label to the machine's name unless the
/// user names the environment.
const LABEL: usize = 64;
/// The most characters of a host kept, the longest a DNS name can be.
const HOST: usize = 253;
/// An origin longer than this, in bytes, isn't parsed and shows no address.
const ORIGIN: usize = 2048;
/// Between the label and the address, and between the feed's word and the label.
const SEPARATOR: &str = " · ";
/// The fewest columns the address is cut to before it is left out, enough for `127.0…3773`.
const SHORTEST_ADDRESS: usize = 10;
/// The fewest columns the header gives the identity after the dot or the feed's word.
const SHORTEST_CHIP: usize = 4;

/// The environment's name and address, cleaned and bounded once at start.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Identity {
    /// The descriptor's label without its control characters, at most `LABEL` characters.
    /// None when the server sent none or only blanks and controls, as a host from before
    /// labels does.
    label: Option<String>,
    /// The origin's host and port, such as `127.0.0.1:3773` or `[::1]:3773`, without a port
    /// its scheme implies. Never the origin's user name, password, path, query or fragment.
    /// None when the origin doesn't parse or has no host.
    address: Option<String>,
    /// The two widths in columns, so a frame where both fit measures neither again.
    label_width: usize,
    address_width: usize,
}

impl Identity {
    /// The identity from the descriptor's `label` and the `origin` t3term connected to.
    pub fn new(label: Option<&str>, origin: &str) -> Identity {
        let label = label
            .map(|label| bounded(label, LABEL))
            .filter(|label| !label.is_empty());
        let address = address(origin);
        Identity {
            label_width: label.as_deref().map_or(0, str::width),
            address_width: address.as_deref().map_or(0, str::width),
            label,
            address,
        }
    }

    /// The label and the address in at most `width` columns. Both show when they fit. When
    /// they don't, the address is cut in the middle, keeping its port, down to
    /// `SHORTEST_ADDRESS` columns, then left out, and then the label is cut with `…`. Without
    /// a label the address takes its place, and without either there is nothing.
    pub fn spans(&self, width: usize, label: Style, address: Style) -> Vec<Span<'static>> {
        if width == 0 {
            return Vec::new();
        }
        let separated = self.label_width + SEPARATOR.width();
        match (&self.label, &self.address) {
            (Some(name), Some(at)) if separated + self.address_width <= width => vec![
                Span::styled(name.clone(), label),
                Span::styled(SEPARATOR, address),
                Span::styled(at.clone(), address),
            ],
            (Some(name), Some(at)) if separated + SHORTEST_ADDRESS <= width => vec![
                Span::styled(name.clone(), label),
                Span::styled(SEPARATOR, address),
                Span::styled(middle(at, width - separated), address),
            ],
            (Some(name), _) if self.label_width <= width => {
                vec![Span::styled(name.clone(), label)]
            }
            (Some(name), _) => vec![Span::styled(clip(name, width), label)],
            (None, Some(at)) if self.address_width <= width => {
                vec![Span::styled(at.clone(), address)]
            }
            (None, Some(at)) => vec![Span::styled(middle(at, width), address)],
            (None, None) => Vec::new(),
        }
    }
}

/// The host and port of `origin`, or None when it doesn't parse or names no host. Only the
/// host and the port are read, so nothing else in the origin, such as credentials in
/// `T3TERM_ORIGIN`, can reach the screen. A non-ASCII domain comes back in its ASCII form, as
/// the URL parser writes it.
fn address(origin: &str) -> Option<String> {
    if origin.len() > ORIGIN {
        return None;
    }
    let url = reqwest::Url::parse(origin).ok()?;
    let host = bounded(url.host_str()?, HOST);
    if host.is_empty() {
        return None;
    }
    Some(match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    })
}

/// Where the sidebar's thread feed stands: T3's `orchestration.subscribeShell`, which
/// `Client::watch_shell` keeps open. The feed shares the TUI's one WebSocket with the open
/// thread's watch, so a dropped connection shows here, but nothing else watches the socket.
/// The open thread's watch keeps its own state, in the status line and the breadcrumb.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feed {
    /// Subscribing for the first time, until T3 says the feed caught up.
    Connecting,
    /// Caught up, as T3's `synchronized` marker says, and live since.
    Live,
    /// The connection dropped or T3 ended the stream. The watcher subscribes again after
    /// the last sequence it saw, and the feed is live again once T3 says it caught up.
    Reconnecting,
    /// T3 refused the subscription, and the watcher stopped for good.
    Failed,
}

impl Feed {
    /// The dot's color, as the desktop's `connectionPhaseDotClassName` picks it:
    /// `bg-success` once connected, `bg-warning` while connecting or reconnecting and
    /// `bg-destructive` for an error. The desktop's muted phases, such as an environment
    /// switched off, have no counterpart here.
    pub fn dot(self, t: &Theme) -> Color {
        match self {
            Feed::Live => t.emerald,
            Feed::Connecting | Feed::Reconnecting => t.warning,
            Feed::Failed => t.error,
        }
    }

    /// The word for the state, as the status line words a thread's watch.
    pub fn word(self) -> &'static str {
        match self {
            Feed::Connecting => "connecting",
            Feed::Live => "live",
            Feed::Reconnecting => "reconnecting",
            Feed::Failed => "closed",
        }
    }
}

/// The end of the breadcrumb row while the sidebar doesn't show the environment, in at most
/// `room` columns: two blank columns, the feed's dot, the feed's word while it isn't live,
/// and as much of the identity as fits after them. The dot comes first, then the word, then
/// the identity, which needs `SHORTEST_CHIP` columns. Under three columns there is nothing.
pub fn chip(identity: &Identity, feed: Feed, room: usize, t: &Theme) -> Vec<Span<'static>> {
    let dot = Style::new().fg(feed.dot(t));
    let muted = Style::new().fg(t.muted);
    let Some(mut rest) = room.checked_sub(3) else {
        return Vec::new();
    };
    let mut spans = vec![Span::raw("  "), Span::styled("●", dot)];
    // Each part after the dot follows a blank column, or the separator after the word.
    let mut gap = " ";
    if feed != Feed::Live {
        let word = feed.word();
        if 1 + word.width() > rest {
            return spans;
        }
        spans.push(Span::raw(gap));
        spans.push(Span::styled(word, dot));
        rest -= 1 + word.width();
        gap = SEPARATOR;
    }
    if rest >= gap.width() + SHORTEST_CHIP {
        let named = identity.spans(rest - gap.width(), muted, muted);
        if !named.is_empty() {
            spans.push(Span::styled(gap, muted));
            spans.extend(named);
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::Depth;

    fn text(spans: &[Span]) -> String {
        spans.iter().map(|span| span.content.as_ref()).collect()
    }

    fn fitted(identity: &Identity, width: usize) -> String {
        text(&identity.spans(width, Style::new(), Style::new()))
    }

    #[test]
    fn an_origin_shows_only_its_host_and_port() {
        let shown = |origin: &str| Identity::new(None, origin).address;
        assert_eq!(
            shown("http://127.0.0.1:3773").as_deref(),
            Some("127.0.0.1:3773")
        );
        assert_eq!(shown("http://[::1]:3773").as_deref(), Some("[::1]:3773"));
        assert_eq!(
            shown("http://LocalHost:3773/").as_deref(),
            Some("localhost:3773")
        );
        // A port the scheme implies isn't written.
        assert_eq!(
            shown("https://t3.example.com").as_deref(),
            Some("t3.example.com")
        );
        assert_eq!(
            shown("http://t3.example.com:80").as_deref(),
            Some("t3.example.com")
        );

        // Credentials, a path, a query and a fragment never come through.
        let origin = "http://alice:hunter2@studio.example.com:8443/private-path?token=sekrit#frag";
        let address = shown(origin).expect("a host");
        assert_eq!(address, "studio.example.com:8443");
        for secret in [
            "alice",
            "hunter2",
            "private-path",
            "token",
            "sekrit",
            "frag",
        ] {
            assert!(!address.contains(secret), "{secret} in {address}");
        }
        let address = shown("http://user@[::1]:3773/?q=1").expect("a host");
        assert_eq!(address, "[::1]:3773");

        // A non-ASCII domain comes back in its ASCII form.
        let address = shown("http://例え.jp:3773").expect("a host");
        assert!(
            address.is_ascii() && address.starts_with("xn--"),
            "{address}"
        );
        assert!(address.ends_with(".jp:3773"), "{address}");

        // What doesn't parse, or has no host, shows no address.
        for origin in [
            "",
            "not a url",
            "127.0.0.1:3773",
            "file:///tmp/t3",
            "http://",
        ] {
            assert_eq!(shown(origin), None, "{origin:?}");
        }
        // Nor does an origin too long to be worth parsing.
        let long = format!("http://{}.example.com:3773", "a".repeat(ORIGIN));
        assert_eq!(shown(&long), None);
        // A control character in the host never reaches the screen, whether or not the
        // parser takes the origin.
        for origin in [
            "http://exa\u{1b}mple.com",
            "foo://ho\u{9b}st:1",
            "foo://h%1Bst",
        ] {
            if let Some(address) = shown(origin) {
                assert!(!address.contains(char::is_control), "{address:?}");
            }
        }
    }

    #[test]
    fn a_label_drops_its_controls_and_is_cut_whole() {
        let label = |raw: &str| Identity::new(Some(raw), "").label;
        assert_eq!(label("Studio Mac").as_deref(), Some("Studio Mac"));
        // A server from before labels sends none, and blanks and controls alone are none.
        assert_eq!(Identity::new(None, "").label, None);
        for blank in ["", "   ", "\u{1b}\u{7}", "\t\r\n", "\u{9b}"] {
            assert_eq!(label(blank), None, "{blank:?}");
        }
        // Escape sequences lose their controls and what's left is plain text. Line breaks
        // and tabs read as spaces, and the ends are trimmed.
        assert_eq!(
            label("\u{1b}]52;c;aGk=\u{7}Studio\u{1b}[2J\r\nMac\t").as_deref(),
            Some("]52;c;aGk=Studio[2J Mac")
        );
        assert_eq!(label("\u{9b}31mMac").as_deref(), Some("31mMac"));

        // A long label keeps its first 64 characters and ends with …, cut where a character
        // starts, so CJK, an accent on its letter and a joined emoji stay whole.
        let cjk = label(&"名".repeat(10_000)).unwrap();
        assert_eq!(cjk, format!("{}…", "名".repeat(LABEL)));
        let accents = label(&"e\u{301}".repeat(100)).unwrap();
        assert_eq!(accents, format!("{}…", "e\u{301}".repeat(LABEL / 2)));
        let coder = "👩\u{200d}💻";
        let emoji = label(&coder.repeat(100)).unwrap();
        let kept = emoji.strip_suffix('…').unwrap();
        assert_eq!(kept.replace(coder, ""), "", "{emoji}");
        assert!(kept.chars().count() <= LABEL);
    }

    #[test]
    fn the_identity_gives_up_the_address_before_the_label() {
        let both = Identity::new(Some("Studio Mac"), "http://127.0.0.1:3773");
        assert_eq!(fitted(&both, 80), "Studio Mac · 127.0.0.1:3773");
        assert_eq!(fitted(&both, 27), "Studio Mac · 127.0.0.1:3773");
        // The address is cut in the middle, so its port stays, down to ten columns.
        assert_eq!(fitted(&both, 26), "Studio Mac · 127.0.…1:3773");
        assert_eq!(fitted(&both, 23), "Studio Mac · 127.0…3773");
        // Then it goes, and then the label is cut.
        assert_eq!(fitted(&both, 22), "Studio Mac");
        assert_eq!(fitted(&both, 10), "Studio Mac");
        assert_eq!(fitted(&both, 9), "Studio M…");
        assert_eq!(fitted(&both, 1), "…");
        assert_eq!(fitted(&both, 0), "");

        // Without a label the address takes its place, and without an address the label
        // shows alone.
        let address = Identity::new(None, "http://[::1]:3773");
        assert_eq!(fitted(&address, 40), "[::1]:3773");
        assert_eq!(fitted(&address, 7), "[::…773");
        let label = Identity::new(Some("Studio Mac"), "not a url");
        assert_eq!(fitted(&label, 40), "Studio Mac");
        assert_eq!(fitted(&Identity::new(None, "not a url"), 40), "");

        // Wide characters never take a row past its width.
        let wide = Identity::new(Some(&"名👩\u{200d}💻é".repeat(30)), "http://例え.jp:3773");
        for width in 0..=120 {
            for identity in [&both, &address, &label, &wide] {
                let shown = fitted(identity, width);
                assert!(shown.width() <= width, "{width}: {shown:?}");
            }
        }
    }

    #[test]
    fn the_header_chip_keeps_the_dot_then_the_word_then_the_identity() {
        let t = Theme::new(Depth::TrueColor);
        let both = Identity::new(Some("Studio Mac"), "http://127.0.0.1:3773");
        let chip = |feed, room| text(&chip(&both, feed, room, &t));
        assert_eq!(chip(Feed::Live, 40), "  ● Studio Mac · 127.0.0.1:3773");
        assert_eq!(chip(Feed::Live, 20), "  ● Studio Mac");
        assert_eq!(chip(Feed::Live, 8), "  ● Stu…");
        assert_eq!(chip(Feed::Live, 7), "  ●");
        assert_eq!(chip(Feed::Live, 3), "  ●");
        assert_eq!(chip(Feed::Live, 2), "");
        assert_eq!(
            chip(Feed::Reconnecting, 60),
            "  ● reconnecting · Studio Mac · 127.0.0.1:3773"
        );
        assert_eq!(chip(Feed::Failed, 20), "  ● closed · Studio…");
        assert_eq!(chip(Feed::Failed, 16), "  ● closed");
        assert_eq!(chip(Feed::Reconnecting, 12), "  ●");
        for room in 0..=60 {
            for feed in [
                Feed::Connecting,
                Feed::Live,
                Feed::Reconnecting,
                Feed::Failed,
            ] {
                assert!(chip(feed, room).width() <= room, "{feed:?} in {room}");
            }
        }

        // The dot takes the desktop's colors.
        let dot = |feed| super::chip(&both, feed, 40, &t)[1].style.fg;
        assert_eq!(dot(Feed::Live), Some(t.emerald));
        assert_eq!(dot(Feed::Connecting), Some(t.warning));
        assert_eq!(dot(Feed::Reconnecting), Some(t.warning));
        assert_eq!(dot(Feed::Failed), Some(t.error));
    }
}
