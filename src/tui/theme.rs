//! T3 Code's dark palette as terminal colors, plus the small formatting helpers the GUI uses
//! for times, durations, model names and project badges.
//!
//! True color is used when `COLORTERM` says the terminal supports it. Otherwise every value
//! degrades to the nearest xterm-256 color at startup, so drawing never pays for the mapping.

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

/// Semantic colors, named after the CSS variables in T3's `index.css` where one exists.
#[derive(Debug, Clone)]
pub struct Theme {
    pub bg: Color,
    pub fg: Color,
    pub muted: Color,
    pub border: Color,
    pub border_strong: Color,
    pub primary: Color,
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
    pub fn new(depth: Depth) -> Theme {
        let c = |v: u32| hex(depth, v);
        Theme {
            bg: c(0x0a0a0a),
            fg: c(0xf5f5f5),
            muted: c(0x818181),
            border: c(0x262626),
            border_strong: c(0x3a3a3a),
            primary: c(0x346bf1),
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

    pub fn detect() -> Theme {
        Theme::new(Depth::detect())
    }

    pub fn text(&self) -> Style {
        Style::new().fg(self.fg)
    }

    pub fn project_color(&self, name: &str) -> Color {
        self.project_colors[project_color_index(name)]
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
    let Some(then) = parse_iso_ms(iso) else {
        return String::new();
    };
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
        assert_eq!(duration_label(3_800), "3.8s");
        assert_eq!(duration_label(12_400), "12s");
        assert_eq!(duration_label(152_000), "2m 32s");
        assert_eq!(duration_label(3_723_000), "1h 2m 3s");
        assert_eq!(working_label(59_000), "59s");
        assert_eq!(working_label(600_000), "10m");
        assert_eq!(working_label(3_723_000), "1h 2m");
    }
}
