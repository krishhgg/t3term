use super::*;

/// Every role of a half. A role added to `Roles` fails to build here until it is listed.
fn all(roles: &Roles) -> [Oklch; 25] {
    let Roles {
        canvas,
        surface_raised,
        surface_overlay,
        text,
        border,
        input,
        focus,
        secondary,
        muted,
        muted_foreground,
        error,
        error_foreground,
        warning,
        warning_foreground,
        accent_surface,
        message_surface,
        message_action,
        message_action_foreground,
        code_background,
        sidebar,
        sidebar_foreground,
        sidebar_muted_foreground,
        sidebar_row_active,
        sidebar_row_selected,
        sidebar_border,
    } = *roles;
    [
        canvas,
        surface_raised,
        surface_overlay,
        text,
        border,
        input,
        focus,
        secondary,
        muted,
        muted_foreground,
        error,
        error_foreground,
        warning,
        warning_foreground,
        accent_surface,
        message_surface,
        message_action,
        message_action_foreground,
        code_background,
        sidebar,
        sidebar_foreground,
        sidebar_muted_foreground,
        sidebar_row_active,
        sidebar_row_selected,
        sidebar_border,
    ]
}

fn built_in() -> impl Iterator<Item = (ThemeName, &'static Palette)> {
    ThemeName::ALL
        .into_iter()
        .filter_map(|name| Some((name, Palette::of(name)?)))
}

#[test]
fn converts_oklch_as_the_desktop_does() {
    // Tailwind's OKLCH values with the hex Tailwind publishes for each, and T3's blues, which
    // the desktop flattens to the same hex in its T3_CODE_*_THEME_COLORS.
    let inside = [
        ("zinc-800", Oklch(0.274, 0.006, 286.033), 0x27272a),
        ("zinc-500", Oklch(0.552, 0.016, 285.938), 0x71717b),
        ("red-500", Oklch(0.637, 0.237, 25.331), 0xfb2c36),
        ("the light primary", Oklch(0.488, 0.217, 264.0), 0x1b4ed8),
        ("the dark primary", Oklch(0.571, 0.21, 264.0), 0x346bf1),
        ("white", Oklch(1.0, 0.0, 0.0), 0xffffff),
        ("black", Oklch(0.0, 0.0, 0.0), 0x000000),
    ];
    // Outside sRGB, each channel clamps to Tailwind's hex.
    let outside = [
        ("amber-500", Oklch(0.769, 0.188, 70.08), 0xfe9a00),
        ("emerald-500", Oklch(0.696, 0.17, 162.48), 0x00bc7d),
        ("blue-500", Oklch(0.623, 0.214, 259.815), 0x2b7fff),
    ];
    for (name, color, hex) in inside.into_iter().chain(outside) {
        assert_eq!(color.rgb(), hex, "{name}: {:06x}", color.rgb());
    }
    // Some of the built-in colors, as the desktop's converter gives them.
    assert_eq!(GROVE.light.canvas.rgb(), 0xf3f7f4);
    assert_eq!(T3_CHAT.light.message_action.rgb(), 0xdb2777);
    assert_eq!(T3_CHAT.dark.focus.rgb(), 0xdb2777);
    assert_eq!(IRIS.dark.canvas.rgb(), 0x1d1929);
}

#[test]
fn every_built_in_color_is_inside_srgb() {
    // So t3term's conversion, which clamps, gives what the desktop's gives.
    let mut colors = 0;
    for (name, palette) in built_in() {
        for (half, roles) in [("light", &palette.light), ("dark", &palette.dark)] {
            for color in all(roles) {
                let inside = color
                    .linear()
                    .iter()
                    .all(|channel| (-0.0001..=1.0001).contains(channel));
                assert!(inside, "{name:?} {half}: {color:?}");
                colors += 1;
            }
        }
    }
    assert_eq!(colors, 5 * 2 * 25);
}

#[test]
fn t3_code_is_the_stock_theme_and_the_rest_are_built_in() {
    assert!(Palette::of(ThemeName::T3Code).is_none());
    let labels: Vec<_> = built_in().map(|(name, _)| name.label()).collect();
    assert_eq!(labels, ["T3 Chat", "Grove", "Ocean", "Ember", "Iris"]);
    // Each half is its own, and each theme its own.
    let canvases: std::collections::HashSet<u32> = built_in()
        .flat_map(|(_, palette)| [palette.light.canvas.rgb(), palette.dark.canvas.rgb()])
        .collect();
    assert_eq!(canvases.len(), 10);
    assert!(std::ptr::eq(GROVE.half(Appearance::Dark), &GROVE.dark));
    assert!(std::ptr::eq(GROVE.half(Appearance::Light), &GROVE.light));
}
