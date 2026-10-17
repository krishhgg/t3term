//! T3 Code's stock theme as terminal colors, in its light and dark halves, plus the small
//! formatting helpers the GUI uses for times, durations, model names and project badges.
//!
//! True color is used when `COLORTERM` says the terminal supports it. Otherwise every value
//! degrades to the nearest xterm-256 color when the theme is built, so drawing never pays for
//! the mapping.

use ratatui::style::{Color, Style};

/// Whether colors go out as 24-bit values or as xterm-256 indexes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    TrueColor,
    Indexed,
}

impl Depth {
    /// `T3TERM_COLOR=256|truecolor` overrides the usual `COLORTERM` check.
    pub fn detect() -> Depth {
        if let Ok(forced) = std::env::var("T3TERM_COLOR") {
            return match forced.as_str() {
                "256" | "indexed" => Depth::Indexed,
                _ => Depth::TrueColor,
            };
        }
        match std::env::var("COLORTERM")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "truecolor" | "24bit" => Depth::TrueColor,
            _ => Depth::Indexed,
        }
    }
}

/// The nearest xterm-256 color: the 6x6x6 cube or the 24-step gray ramp, whichever is closer.
pub fn nearest_256(r: u8, g: u8, b: u8) -> u8 {
    let level = |v: u8| -> (u8, i32) {
        // Cube levels are 0, 95, 135, 175, 215, 255.
        let steps = [0i32, 95, 135, 175, 215, 255];
        let (i, value) = steps
            .iter()
            .enumerate()
            .min_by_key(|(_, s)| (**s - v as i32).abs())
            .expect("six steps");
        (i as u8, *value)
    };
    let (ri, rv) = level(r);
    let (gi, gv) = level(g);
    let (bi, bv) = level(b);
    let cube_index = 16 + 36 * ri + 6 * gi + bi;
    let cube_distance = (rv - r as i32).pow(2) + (gv - g as i32).pow(2) + (bv - b as i32).pow(2);

    let average = (r as i32 + g as i32 + b as i32) / 3;
    // Ramp steps are 8, 18, ..., 238. Adding half a step rounds to the nearest one.
    let gray_step = ((average - 3).max(0) / 10).min(23);
    let gray_value = 8 + gray_step * 10;
    let gray_distance = (gray_value - r as i32).pow(2)
        + (gray_value - g as i32).pow(2)
        + (gray_value - b as i32).pow(2);
    if gray_distance < cube_distance {
        232 + gray_step as u8
    } else {
        cube_index
    }
}

fn hex(depth: Depth, value: u32) -> Color {
    let (r, g, b) = ((value >> 16) as u8, (value >> 8) as u8, value as u8);
    match depth {
        Depth::TrueColor => Color::Rgb(r, g, b),
        Depth::Indexed => Color::Indexed(nearest_256(r, g, b)),
    }
}

/// Tailwind's `-400` shades, the dark-mode half of T3's project icon palette, in T3's order.
const PROJECT_COLORS: [u32; 18] = [
    0x99a1af, 0xff6467, 0xff8904, 0xffb900, 0xfdc700, 0x9ae600, 0x05df72, 0x00d492, 0x00d5be,
    0x00d3f2, 0x00bcff, 0x51a2ff, 0x7c86ff, 0xa684ff, 0xc27aff, 0xed6bff, 0xfb64b6, 0xff637e,
];

/// Tailwind's `-600` shades, the light half of the same palette (`text-{color}-600
/// dark:text-{color}-400` in apps/web/src/projectIdentity.ts), in the same order.
const PROJECT_COLORS_LIGHT: [u32; 18] = [
    0x4a5565, 0xe7000b, 0xf54900, 0xe17100, 0xd08700, 0x5ea500, 0x00a63e, 0x009966, 0x009689,
    0x0092b8, 0x0084d1, 0x155dfc, 0x4f39f6, 0x7f22fe, 0x9810fa, 0xc800de, 0xe60076, 0xec003f,
];

/// Which half of a theme draws: the desktop's resolved appearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    Light,
    Dark,
}

/// Semantic colors, named after the CSS variables in T3's `index.css` where one exists.
#[derive(Debug, Clone)]
pub struct Theme {
    /// The half these colors are.
    pub appearance: Appearance,
    /// The depth they were built for, which another half is built for too.
    pub depth: Depth,
    pub bg: Color,
    pub fg: Color,
    pub muted: Color,
    pub border: Color,
    pub border_strong: Color,
    pub primary: Color,
    /// `--primary-foreground`, text on `primary`, as on the send button.
    pub on_primary: Color,
    pub info: Color,
    pub info_fg: Color,
    pub success: Color,
    /// `--success` itself, emerald-500, which the sidebar's Done takes. `success` above is the
    /// lighter shade the dark theme gives `--success-foreground`.
    pub emerald: Color,
    /// `--warning`, amber-500, for the sidebar's Limited and Woke.
    pub warning: Color,
    pub warning_fg: Color,
    pub warning_border: Color,
    pub warning_surface: Color,
    pub error: Color,
    pub error_fg: Color,
    pub indigo: Color,
    pub violet: Color,
    pub bubble: Color,
    /// The composer box, one step above the canvas.
    pub raised: Color,
    pub code_bg: Color,
    pub inline_code_bg: Color,
    pub chip_bg: Color,
    pub sidebar_bg: Color,
    pub sidebar_fg: Color,
    pub sidebar_muted: Color,
    pub sidebar_border: Color,
    pub row_active: Color,
    pub row_selected: Color,
    /// Menus, and the highlighted row in one: the GUI's `popover` and `accent`.
    pub popover: Color,
    pub highlight: Color,
    pub indigo_border: Color,
    pub indigo_surface: Color,
    pub claude: Color,
    pub openai: Color,
    pub project_colors: [Color; 18],
}

impl Theme {
    /// The stock theme's `appearance` half.
    pub fn of(appearance: Appearance, depth: Depth) -> Theme {
        match appearance {
            Appearance::Light => Theme::light(depth),
            Appearance::Dark => Theme::new(depth),
        }
    }

    /// The stock theme's dark half.
    pub fn new(depth: Depth) -> Theme {
        let c = |v: u32| hex(depth, v);
        Theme {
            appearance: Appearance::Dark,
            depth,
            bg: c(0x0a0a0a),
            fg: c(0xf5f5f5),
            muted: c(0x818181),
            border: c(0x262626),
            border_strong: c(0x3a3a3a),
            primary: c(0x346bf1),
            // The desktop's is white. The dark half keeps the text color it drew before.
            on_primary: c(0xf5f5f5),
            info: c(0x2b7fff),
            info_fg: c(0x51a2ff),
            success: c(0x00d492),
            emerald: c(0x00bc7d),
            warning: c(0xfe9a00),
            warning_fg: c(0xffb900),
            warning_border: c(0x4e3207),
            warning_surface: c(0x1d150a),
            error: c(0xfb414a),
            error_fg: c(0xff6467),
            indigo: c(0xa3b3ff),
            violet: c(0xc4b4ff),
            bubble: c(0x171717),
            raised: c(0x101010),
            code_bg: c(0x131313),
            inline_code_bg: c(0x1f1f1f),
            chip_bg: c(0x1e1e1e),
            sidebar_bg: c(0x000000),
            sidebar_fg: c(0xf1f3f7),
            sidebar_muted: c(0xa3a3a3),
            sidebar_border: c(0x1f1f1f),
            row_active: c(0x1a1b1b),
            row_selected: c(0x111111),
            popover: c(0x171717),
            highlight: c(0x262626),
            indigo_border: c(0x2f3366),
            indigo_surface: c(0x14152a),
            claude: c(0xd97757),
            openai: c(0xd4d4d4),
            project_colors: PROJECT_COLORS.map(c),
        }
    }

    /// The stock theme's light half: the desktop's light tokens, flattened to sRGB as the
    /// desktop's own values are. Where a token would lose a difference the dark half draws,
    /// the field takes a darker zinc shade and says so.
    pub fn light(depth: Depth) -> Theme {
        let c = |v: u32| hex(depth, v);
        Theme {
            appearance: Appearance::Light,
            depth,
            // `--background`, zinc-25, and `--foreground`, zinc-800.
            bg: c(0xfcfcfc),
            fg: c(0x27272a),
            // `--muted-foreground`, zinc-500.
            muted: c(0x71717b),
            // `--border`, zinc-200, and `--input`, zinc-300.
            border: c(0xe4e4e7),
            border_strong: c(0xd4d4d8),
            // `--primary`, oklch(0.488 0.217 264).
            primary: c(0x1b4ed8),
            // `--primary-foreground`, white.
            on_primary: c(0xffffff),
            // `--info`, blue-500, and `--info-foreground`, blue-700.
            info: c(0x2b7fff),
            info_fg: c(0x1447e6),
            // `--success-foreground`, emerald-700, and `--success`, emerald-500.
            success: c(0x007a55),
            emerald: c(0x00bc7d),
            // `--warning`, amber-500, and `--warning-foreground`, amber-700.
            warning: c(0xfe9a00),
            warning_fg: c(0xbb4d00),
            // The approval panel's outline and fill: amber-500 at 28% and 8% over the canvas,
            // as the dark half's are.
            warning_border: c(0xfde1b5),
            warning_surface: c(0xfcf4e8),
            // `--error`, red-500, and `--error-foreground`, red-700.
            error: c(0xfb2c36),
            error_fg: c(0xc10007),
            // The light half of `text-indigo-600 dark:text-indigo-300/90` and of
            // `text-violet-600 dark:text-violet-300/90`.
            indigo: c(0x4f39f6),
            violet: c(0x7f22fe),
            // `--message-surface`, zinc-100.
            bubble: c(0xf4f4f5),
            // The composer is `--card`, white, in light.
            raised: c(0xffffff),
            // The desktop sets code on zinc-50 inside a border t3term doesn't draw. Halfway
            // between zinc-100 and zinc-200, a block shows on the canvas and in a message
            // bubble, as the dark half's does.
            code_bg: c(0xececee),
            // zinc-200, so inline code shows inside a message bubble, which is zinc-100.
            inline_code_bg: c(0xe4e4e7),
            // The desktop draws chips with no fill. t3term fills them, as in the dark half.
            chip_bg: c(0xe4e4e7),
            // `--sidebar`, zinc-50, with `--sidebar-foreground` and the muted text and border.
            sidebar_bg: c(0xfafafa),
            sidebar_fg: c(0x27272a),
            sidebar_muted: c(0x71717b),
            sidebar_border: c(0xe4e4e7),
            // The desktop's active and selected rows are white, which barely shows on zinc-50,
            // so zinc-200 and zinc-100.
            row_active: c(0xe4e4e7),
            row_selected: c(0xf4f4f5),
            // `--popover`, white. `--accent` is zinc-100, which barely shows on white, so the
            // highlighted row is zinc-200.
            popover: c(0xffffff),
            highlight: c(0xe4e4e7),
            // The question panel's outline and fill: indigo-600 at 28% and 8% over the canvas.
            indigo_border: c(0xccc5fa),
            indigo_surface: c(0xeeecfc),
            claude: c(0xd97757),
            // The OpenAI mark is black in light and white in dark, where the dark half takes
            // neutral-300. neutral-700 mirrors it.
            openai: c(0x404040),
            project_colors: PROJECT_COLORS_LIGHT.map(c),
        }
    }

    pub fn text(&self) -> Style {
        Style::new().fg(self.fg)
    }

    /// Every color the theme has. A field added to `Theme` fails to build here until it is
    /// listed.
    #[cfg(test)]
    pub fn colors(&self) -> Vec<Color> {
        let Theme {
            appearance: _,
            depth: _,
            bg,
            fg,
            muted,
            border,
            border_strong,
            primary,
            on_primary,
            info,
            info_fg,
            success,
            emerald,
            warning,
            warning_fg,
            warning_border,
            warning_surface,
            error,
            error_fg,
            indigo,
            violet,
            bubble,
            raised,
            code_bg,
            inline_code_bg,
            chip_bg,
            sidebar_bg,
            sidebar_fg,
            sidebar_muted,
            sidebar_border,
            row_active,
            row_selected,
            popover,
            highlight,
            indigo_border,
            indigo_surface,
            claude,
            openai,
            project_colors,
        } = self;
        let mut colors = vec![
            *bg,
            *fg,
            *muted,
            *border,
            *border_strong,
            *primary,
            *on_primary,
            *info,
            *info_fg,
            *success,
            *emerald,
            *warning,
            *warning_fg,
            *warning_border,
            *warning_surface,
            *error,
            *error_fg,
            *indigo,
            *violet,
            *bubble,
            *raised,
            *code_bg,
            *inline_code_bg,
            *chip_bg,
            *sidebar_bg,
            *sidebar_fg,
            *sidebar_muted,
            *sidebar_border,
            *row_active,
            *row_selected,
            *popover,
            *highlight,
            *indigo_border,
            *indigo_surface,
            *claude,
            *openai,
        ];
        colors.extend(project_colors);
        colors
    }

    pub fn project_color(&self, badge: &Badge) -> Color {
        self.project_colors[badge.color]
    }

    /// A glyph standing in for the provider's logo.
    pub fn provider_glyph(&self, instance_id: &str) -> (&'static str, Color) {
        let id = instance_id.to_ascii_lowercase();
        if id.contains("claude") || id.contains("anthropic") {
            ("✱", self.claude)
        } else if id.contains("codex") || id.contains("openai") {
            ("◎", self.openai)
        } else if id.contains("cursor") {
            ("◆", self.fg)
        } else {
            ("◇", self.muted)
        }
    }
}

// ---- project identity, as in apps/web/src/projectIdentity.ts ----

/// A project's monogram and color, from its whole title. Working them out reads all of the
/// title, so the sidebar's `Badges` keeps them from one change of the project to the next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Badge {
    pub letters: String,
    /// The color's place in `PROJECT_COLORS`.
    color: usize,
}

impl Badge {
    pub fn of(title: &str) -> Badge {
        Badge {
            letters: monogram(title),
            color: project_color_index(title),
        }
    }
}

/// Two letters for a project without an icon: the first glyph, then the first digit in the
/// first word, else the first glyph of the last word, else the last glyph of the word.
pub fn monogram(name: &str) -> String {
    let words: Vec<Vec<char>> = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.chars().collect())
        .collect();
    let Some(first_word) = words.first() else {
        return "PR".into();
    };
    let first = first_word[0];
    let second = first_word[1..]
        .iter()
        .copied()
        .find(|c| c.is_numeric())
        .or_else(|| {
            if words.len() > 1 {
                words.last().and_then(|w| w.first().copied())
            } else {
                first_word.last().copied()
            }
        })
        .unwrap_or(first);
    format!("{first}{second}").to_uppercase()
}

pub fn project_color_index(name: &str) -> usize {
    let seed = name.trim().to_lowercase();
    let seed = if seed.is_empty() {
        "project".to_string()
    } else {
        seed
    };
    seed.chars().fold(0usize, |index, c| {
        (index * 31 + c as usize) % PROJECT_COLORS.len()
    })
}

// ---- labels ----

pub fn runtime_mode_label(mode: &str) -> &str {
    match mode {
        "approval-required" => "Supervised",
        "auto-accept-edits" => "Auto-accept edits",
        "auto" => "Auto",
        "full-access" => "Full access",
        other => other,
    }
}

/// A readable form of a model slug (`claude-haiku-4-5` becomes `Claude Haiku 4.5`), for models
/// missing from T3's provider list.
pub fn model_display_name(slug: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    for part in slug.split('-').filter(|p| !p.is_empty()) {
        let numeric = part.chars().all(|c| c.is_ascii_digit());
        match (numeric, words.last_mut()) {
            (true, Some(last)) if last.chars().all(|c| c.is_ascii_digit() || c == '.') => {
                last.push('.');
                last.push_str(part);
            }
            _ => {
                let mut chars = part.chars();
                let word = match chars.next() {
                    Some(_) if part.len() <= 3 && !numeric => part.to_uppercase(),
                    Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                };
                words.push(word);
            }
        }
    }
    words.join(" ")
}

// ---- times ----

/// Milliseconds since the Unix epoch for an ISO-8601 UTC timestamp such as
/// `2026-10-08T00:06:52.769Z`. Offsets other than `Z` are not needed: the server sends UTC.
pub fn parse_iso_ms(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> { text.get(from..to)?.parse().ok() };
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let mut millis = 0;
    if bytes.get(19) == Some(&b'.') {
        let digits: String = text[20..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        let mut fraction = digits.clone();
        fraction.truncate(3);
        while fraction.len() < 3 {
            fraction.push('0');
        }
        millis = fraction.parse().ok()?;
    }
    // Days from civil, after Howard Hinnant.
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some((((days * 24 + hour) * 60 + minute) * 60 + second) * 1000 + millis)
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The sidebar's compact relative time: `now`, `5m`, `3h`, `12d`.
pub fn relative_time(iso: &str, now: i64) -> String {
    match parse_iso_ms(iso) {
        Some(then) => age(then, now),
        None => String::new(),
    }
}

/// How long before `now` the moment `then` was, both in milliseconds, as `relative_time`
/// writes it.
pub fn age(then: i64, now: i64) -> String {
    let seconds = (now - then).max(0) / 1000;
    if seconds < 60 {
        "now".into()
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

/// The sidebar's working clock: `12s`, `5m`, `1h 2m`.
pub fn working_label(ms: i64) -> String {
    let seconds = ms.max(0) / 1000;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    format!("{}h {}m", minutes / 60, minutes % 60)
}

/// The transcript's duration: `3.8s`, `12s`, `1m 5s`, `1h 2m 3s`.
pub fn duration_label(ms: i64) -> String {
    let ms = ms.max(0);
    if ms < 10_000 {
        return format!("{:.1}s", ms as f64 / 1000.0);
    }
    let seconds = ms / 1000;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m {}s", seconds % 60);
    }
    format!("{}h {}m {}s", minutes / 60, minutes % 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG 2's contrast ratio between two 24-bit colors.
    fn contrast(a: Color, b: Color) -> f64 {
        let luminance = |color: Color| {
            let Color::Rgb(r, g, b) = color else {
                panic!("{color:?} is not 24-bit");
            };
            let linear = |c: u8| {
                let c = c as f64 / 255.0;
                match c <= 0.04045 {
                    true => c / 12.92,
                    false => ((c + 0.055) / 1.055).powf(2.4),
                }
            };
            0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
        };
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn maps_hex_to_the_nearest_256_color() {
        assert_eq!(nearest_256(0, 0, 0), 16);
        assert_eq!(nearest_256(255, 255, 255), 231);
        assert_eq!(nearest_256(0x0a, 0x0a, 0x0a), 232);
        assert_eq!(nearest_256(0xf5, 0xf5, 0xf5), 255);
        assert_eq!(nearest_256(0x81, 0x81, 0x81), 244);
        // 26 is closer to step 234 (28) than to 233 (18).
        assert_eq!(nearest_256(26, 26, 26), 234);
        // T3's primary blue lands in the cube, not on the gray ramp.
        let blue = nearest_256(0x34, 0x6b, 0xf1);
        assert!((16..232).contains(&blue), "{blue}");
        assert_eq!(Theme::new(Depth::Indexed).primary, Color::Indexed(blue));
        assert_eq!(
            Theme::new(Depth::TrueColor).primary,
            Color::Rgb(0x34, 0x6b, 0xf1)
        );
    }

    #[test]
    fn each_half_tells_apart_the_backgrounds_it_draws_on_one_another() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            let t = Theme::of(appearance, Depth::TrueColor);
            assert_eq!(t.appearance, appearance);
            assert_eq!(t.depth, Depth::TrueColor);
            // Each pair is something drawn and what it is drawn on.
            let pairs = [
                ("bubble on the canvas", t.bubble, t.bg),
                ("a code block on the canvas", t.code_bg, t.bg),
                ("a code block in a bubble", t.code_bg, t.bubble),
                ("inline code on the canvas", t.inline_code_bg, t.bg),
                ("inline code in a bubble", t.inline_code_bg, t.bubble),
                ("the composer on the canvas", t.raised, t.bg),
                ("a chip in the composer", t.chip_bg, t.raised),
                ("a menu on the canvas", t.popover, t.bg),
                ("a menu's highlighted row", t.highlight, t.popover),
                ("the sidebar beside the canvas", t.sidebar_bg, t.bg),
                ("the active row in the sidebar", t.row_active, t.sidebar_bg),
                (
                    "the selected row in the sidebar",
                    t.row_selected,
                    t.sidebar_bg,
                ),
                (
                    "the active row beside the selected",
                    t.row_active,
                    t.row_selected,
                ),
                ("the approval panel on the canvas", t.warning_surface, t.bg),
                ("the question panel on the canvas", t.indigo_surface, t.bg),
            ];
            for (what, drawn, on) in pairs {
                assert_ne!(drawn, on, "{appearance:?}: {what}");
            }
            // The dark half's is 4.3 to 1. The light half's text color on its blue was 2.2.
            let reads = contrast(t.on_primary, t.primary);
            assert!(
                reads >= 4.0,
                "{appearance:?}: text on primary at {reads:.1} to 1"
            );
        }
        // The light half has the desktop's own canvas, text and blue.
        let light = Theme::light(Depth::TrueColor);
        assert_eq!(light.bg, Color::Rgb(0xfc, 0xfc, 0xfc));
        assert_eq!(light.fg, Color::Rgb(0x27, 0x27, 0x2a));
        assert_eq!(light.primary, Color::Rgb(0x1b, 0x4e, 0xd8));
        // A project keeps its place in the palette and takes its -600 shade: red-600 for red.
        assert_eq!(light.project_colors[1], Color::Rgb(0xe7, 0x00, 0x0b));
        assert_eq!(
            Theme::new(Depth::TrueColor).project_colors[1],
            Color::Rgb(0xff, 0x64, 0x67)
        );
    }

    #[test]
    fn derives_project_monograms_like_the_gui() {
        assert_eq!(monogram("t3term-demo-project"), "T3");
        assert_eq!(monogram("better fork"), "BF");
        assert_eq!(monogram("harbor"), "HR");
        assert_eq!(monogram(""), "PR");
        assert!(project_color_index("t3term-demo-project") < 18);
        assert_eq!(
            project_color_index("abc"),
            project_color_index("ABC"),
            "case does not change the color"
        );
    }

    #[test]
    fn a_badge_comes_from_the_whole_title() {
        // The last word of a long title still gives the second letter and colors the badge.
        let long = format!("alpha {} zulu", "x".repeat(10_000));
        let badge = Badge::of(&long);
        assert_eq!(badge.letters, "AZ");
        assert_eq!(badge.color, project_color_index(&long));
        assert_ne!(badge, Badge::of(&format!("alpha {}", "x".repeat(10_000))));
        // A missing project shows as "Project", and a blank title colors as one.
        assert_eq!(Badge::of("Project").letters, "PT");
        assert_eq!(Badge::of("").letters, "PR");
        assert_eq!(Badge::of("").color, Badge::of("Project").color);
    }

    #[test]
    fn formats_model_names_modes_and_times() {
        assert_eq!(model_display_name("claude-haiku-4-5"), "Claude Haiku 4.5");
        assert_eq!(model_display_name("gpt-5.5"), "GPT 5.5");
        assert_eq!(runtime_mode_label("approval-required"), "Supervised");
        assert_eq!(runtime_mode_label("full-access"), "Full access");

        assert_eq!(parse_iso_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_iso_ms("2026-10-08T00:06:52.769Z"),
            Some(1_791_418_012_769)
        );
        assert_eq!(parse_iso_ms("nope"), None);
        let now = parse_iso_ms("2026-10-08T00:10:00Z").unwrap();
        assert_eq!(relative_time("2026-10-08T00:09:30Z", now), "now");
        assert_eq!(relative_time("2026-10-08T00:06:52Z", now), "3m");
        assert_eq!(relative_time("2026-10-07T21:00:00Z", now), "3h");
        assert_eq!(relative_time("2026-09-26T00:00:00Z", now), "12d");
        assert_eq!(relative_time("nope", now), "");
        assert_eq!(age(now - 7_200_000, now), "2h");
        assert_eq!(age(now + 5_000, now), "now");
        assert_eq!(duration_label(3_800), "3.8s");
        assert_eq!(duration_label(12_400), "12s");
        assert_eq!(duration_label(152_000), "2m 32s");
        assert_eq!(duration_label(3_723_000), "1h 2m 3s");
        assert_eq!(working_label(59_000), "59s");
        assert_eq!(working_label(600_000), "10m");
        assert_eq!(working_label(3_723_000), "1h 2m");
    }
}
