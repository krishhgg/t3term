//! T3 Code's built-in themes, T3 Chat, Grove, Ocean, Ember and Iris, as the desktop defines them
//! in packages/shared/src/themePalettes.ts. Each has a light and a dark half of 57 color roles.
//! t3term keeps the 25 it draws, in the desktop's own OKLCH values.
//!
//! The desktop's index.css maps a theme's roles onto the tokens its stock theme sets (lines
//! 1219-1292) and keeps the stock's success, info, provider and channel colors. `Theme::of`
//! does the same with the stock half of the same appearance.

use super::theme::Appearance;
use crate::settings::ThemeName;

/// A color as OKLCH lightness, chroma and hue in degrees, as the desktop writes them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Oklch(pub f64, pub f64, pub f64);

impl Oklch {
    /// The color as 24-bit sRGB, converted as the desktop's `themeOklchToRgb` converts it
    /// (apps/web/src/themePalette.ts:499-564) for a color inside sRGB, as every built-in color
    /// is. For a color outside, the desktop first lowers its chroma, where t3term clamps each
    /// channel, which gives the hex Tailwind publishes for such a color.
    pub fn rgb(self) -> u32 {
        let [r, g, b] = self.linear().map(|channel| {
            let encoded = match channel <= 0.0031308 {
                true => channel * 12.92,
                false => 1.055 * channel.powf(1.0 / 2.4) - 0.055,
            };
            (encoded.clamp(0.0, 1.0) * 255.0).round() as u32
        });
        (r << 16) | (g << 8) | b
    }

    /// Linear sRGB, unclamped, as `oklchToRgbUnclamped` has it.
    fn linear(self) -> [f64; 3] {
        let Oklch(l, c, h) = self;
        let (a, b) = (c * h.to_radians().cos(), c * h.to_radians().sin());
        let long = (l + 0.3963377774 * a + 0.2158037573 * b).powi(3);
        let medium = (l - 0.1055613458 * a - 0.0638541728 * b).powi(3);
        let short = (l - 0.0894841775 * a - 1.291485548 * b).powi(3);
        [
            4.0767416621 * long - 3.3077115913 * medium + 0.2309699292 * short,
            -1.2684380046 * long + 2.6097574011 * medium - 0.3413193965 * short,
            -0.0041960863 * long - 0.7034186147 * medium + 1.707614701 * short,
        ]
    }
}

/// The roles of a theme's half that t3term draws, named as the desktop names them.
#[derive(Debug, Clone, Copy)]
pub struct Roles {
    pub canvas: Oklch,
    pub surface_raised: Oklch,
    pub surface_overlay: Oklch,
    pub text: Oklch,
    pub border: Oklch,
    pub input: Oklch,
    pub focus: Oklch,
    pub secondary: Oklch,
    pub muted: Oklch,
    pub muted_foreground: Oklch,
    pub error: Oklch,
    pub error_foreground: Oklch,
    pub warning: Oklch,
    pub warning_foreground: Oklch,
    pub accent_surface: Oklch,
    pub message_surface: Oklch,
    pub message_action: Oklch,
    pub message_action_foreground: Oklch,
    pub code_background: Oklch,
    pub sidebar: Oklch,
    pub sidebar_foreground: Oklch,
    pub sidebar_muted_foreground: Oklch,
    pub sidebar_row_active: Oklch,
    pub sidebar_row_selected: Oklch,
    pub sidebar_border: Oklch,
}

/// A built-in theme's two halves. The desktop writes each theme light, with dark as its
/// variant.
#[derive(Debug)]
pub struct Palette {
    pub light: Roles,
    pub dark: Roles,
}

impl Palette {
    /// The built-in theme `name` names, or `None` for T3 Code's own.
    pub fn of(name: ThemeName) -> Option<&'static Palette> {
        match name {
            ThemeName::T3Code => None,
            ThemeName::T3Chat => Some(&T3_CHAT),
            ThemeName::Grove => Some(&GROVE),
            ThemeName::Ocean => Some(&OCEAN),
            ThemeName::Ember => Some(&EMBER),
            ThemeName::Iris => Some(&IRIS),
        }
    }

    pub fn half(&self, appearance: Appearance) -> &Roles {
        match appearance {
            Appearance::Light => &self.light,
            Appearance::Dark => &self.dark,
        }
    }
}

// The desktop's values at f570bd2, as palettes/table.py writes them from its file.

/// T3 Chat, `t3-chat`.
pub const T3_CHAT: Palette = Palette {
    light: Roles {
        canvas: Oklch(0.982446, 0.010114, 325.653),
        surface_raised: Oklch(0.988235, 0.005049, 325.615),
        surface_overlay: Oklch(1.0, 0.0, 0.0),
        text: Oklch(0.325698, 0.116116, 325.037),
        border: Oklch(0.923531, 0.021247, 328.096),
        input: Oklch(0.851713, 0.055822, 336.6),
        focus: Oklch(0.591646, 0.217985, 0.584),
        secondary: Oklch(0.869588, 0.06751, 334.899),
        muted: Oklch(0.802407, 0.090963, 345.892),
        muted_foreground: Oklch(0.428932, 0.163929, 354.332),
        error: Oklch(0.627117, 0.248974, 7.734),
        error_foreground: Oklch(0.458704, 0.169677, 3.815),
        warning: Oklch(0.76859, 0.164659, 70.08),
        warning_foreground: Oklch(0.54612, 0.143036, 48.949),
        accent_surface: Oklch(0.939552, 0.024286, 321.664),
        message_surface: Oklch(0.926746, 0.037898, 332.6),
        message_action: Oklch(0.591646, 0.217985, 0.584),
        message_action_foreground: Oklch(1.0, 0.0, 0.0),
        code_background: Oklch(0.953855, 0.019695, 315.668),
        sidebar: Oklch(0.928886, 0.031178, 322.592),
        sidebar_foreground: Oklch(0.396296, 0.025134, 285.196),
        sidebar_muted_foreground: Oklch(0.494754, 0.190937, 354.544),
        sidebar_row_active: Oklch(0.978851, 0.001321, 106.424),
        sidebar_row_selected: Oklch(0.978851, 0.001321, 106.424),
        sidebar_border: Oklch(0.938313, 0.002552, 48.717),
    },
    dark: Roles {
        canvas: Oklch(0.22813, 0.020366, 307.469),
        surface_raised: Oklch(0.279864, 0.021572, 309.532),
        surface_overlay: Oklch(0.154761, 0.01316, 338.901),
        text: Oklch(0.980735, 0.004092, 301.426),
        border: Oklch(0.266943, 0.015262, 302.425),
        input: Oklch(0.266817, 0.02897, 344.461),
        focus: Oklch(0.591646, 0.217985, 0.584),
        secondary: Oklch(0.313674, 0.030572, 310.061),
        muted: Oklch(0.360924, 0.021469, 316.83),
        muted_foreground: Oklch(0.880303, 0.03077, 342.696),
        error: Oklch(0.458704, 0.169677, 3.815),
        error_foreground: Oklch(0.901233, 0.057189, 343.694),
        warning: Oklch(0.76859, 0.164659, 70.08),
        warning_foreground: Oklch(0.836861, 0.164422, 84.429),
        accent_surface: Oklch(0.364912, 0.050794, 308.491),
        message_surface: Oklch(0.273791, 0.025541, 309.079),
        message_action: Oklch(0.460685, 0.185347, 4.099),
        message_action_foreground: Oklch(0.901233, 0.057189, 343.694),
        code_background: Oklch(0.22813, 0.020366, 307.469),
        sidebar: Oklch(0.185778, 0.019368, 322.159),
        sidebar_foreground: Oklch(0.967434, 0.001326, 286.375),
        sidebar_muted_foreground: Oklch(0.880303, 0.03077, 342.696),
        sidebar_row_active: Oklch(0.23366, 0.026081, 338.196),
        sidebar_row_selected: Oklch(0.23366, 0.026081, 338.196),
        sidebar_border: Oklch(0.269132, 0.030766, 351.067),
    },
};

/// Grove, `grove`.
pub const GROVE: Palette = Palette {
    light: Roles {
        canvas: Oklch(0.972369, 0.005497, 157.15),
        surface_raised: Oklch(0.949276, 0.004496, 159.002),
        surface_overlay: Oklch(0.932695, 0.003778, 160.944),
        text: Oklch(0.222003, 0.03479, 328.979),
        border: Oklch(0.864831, 0.01312, 167.255),
        input: Oklch(0.829746, 0.016084, 168.234),
        focus: Oklch(0.523295, 0.112292, 158.089),
        secondary: Oklch(0.936464, 0.014601, 163.554),
        muted: Oklch(0.945455, 0.012308, 162.879),
        muted_foreground: Oklch(0.527266, 0.012309, 320.683),
        error: Oklch(0.637823, 0.237287, 25.436),
        error_foreground: Oklch(0.509494, 0.208583, 28.513),
        warning: Oklch(0.772406, 0.172798, 65.367),
        warning_foreground: Oklch(0.545036, 0.155019, 45.359),
        accent_surface: Oklch(0.909438, 0.021521, 164.612),
        message_surface: Oklch(0.891377, 0.026164, 164.929),
        message_action: Oklch(0.535028, 0.106403, 77.549),
        message_action_foreground: Oklch(0.990339, 0.008411, 325.64),
        code_background: Oklch(0.955888, 0.004783, 158.391),
        sidebar: Oklch(0.936464, 0.014601, 163.554),
        sidebar_foreground: Oklch(0.222003, 0.03479, 328.979),
        sidebar_muted_foreground: Oklch(0.515606, 0.011938, 318.897),
        sidebar_row_active: Oklch(0.85335, 0.03597, 165.158),
        sidebar_row_selected: Oklch(0.836654, 0.040284, 165.149),
        sidebar_border: Oklch(0.860274, 0.010287, 168.339),
    },
    dark: Roles {
        canvas: Oklch(0.260865, 0.02152, 162.75),
        surface_raised: Oklch(0.363192, 0.016572, 165.32),
        surface_overlay: Oklch(0.411828, 0.014378, 166.627),
        text: Oklch(0.990339, 0.008411, 325.64),
        border: Oklch(0.457475, 0.044046, 160.971),
        input: Oklch(0.519849, 0.049896, 160.863),
        focus: Oklch(0.796228, 0.133058, 157.319),
        secondary: Oklch(0.380487, 0.048313, 159.608),
        muted: Oklch(0.339728, 0.039456, 160.274),
        muted_foreground: Oklch(0.715427, 0.010896, 171.428),
        error: Oklch(0.655108, 0.221148, 23.473),
        error_foreground: Oklch(0.704237, 0.187511, 22.228),
        warning: Oklch(0.772406, 0.172798, 65.367),
        warning_foreground: Oklch(0.829017, 0.171221, 81.038),
        accent_surface: Oklch(0.437021, 0.060312, 158.962),
        message_surface: Oklch(0.470111, 0.067221, 158.676),
        message_action: Oklch(0.791603, 0.129713, 83.299),
        message_action_foreground: Oklch(0.222003, 0.03479, 328.979),
        code_background: Oklch(0.312979, 0.018942, 164.082),
        sidebar: Oklch(0.309925, 0.032827, 160.944),
        sidebar_foreground: Oklch(0.990339, 0.008411, 325.64),
        sidebar_muted_foreground: Oklch(0.711387, 0.007643, 175.89),
        sidebar_row_active: Oklch(0.41688, 0.056069, 159.165),
        sidebar_row_selected: Oklch(0.437466, 0.060406, 158.958),
        sidebar_border: Oklch(0.569253, 0.015933, 167.062),
    },
};

/// Ocean, `ocean`.
pub const OCEAN: Palette = Palette {
    light: Roles {
        canvas: Oklch(0.974199, 0.002856, 241.597),
        surface_raised: Oklch(0.951058, 0.002962, 258.339),
        surface_overlay: Oklch(0.934442, 0.003181, 269.1),
        text: Oklch(0.222003, 0.03479, 328.979),
        border: Oklch(0.867646, 0.013482, 252.362),
        input: Oklch(0.832939, 0.017389, 252.598),
        focus: Oklch(0.536684, 0.120219, 247.01),
        secondary: Oklch(0.939254, 0.01193, 241.729),
        muted: Oklch(0.948004, 0.009649, 241.695),
        muted_foreground: Oklch(0.528741, 0.01828, 313.823),
        error: Oklch(0.637823, 0.237287, 25.436),
        error_foreground: Oklch(0.509494, 0.208583, 28.513),
        warning: Oklch(0.772406, 0.172798, 65.367),
        warning_foreground: Oklch(0.546927, 0.155556, 45.359),
        accent_surface: Oklch(0.91295, 0.018827, 241.836),
        message_surface: Oklch(0.895373, 0.023469, 241.913),
        message_action: Oklch(0.493961, 0.08175, 201.584),
        message_action_foreground: Oklch(0.990339, 0.008411, 325.64),
        code_background: Oklch(0.957684, 0.002906, 253.68),
        sidebar: Oklch(0.939254, 0.01193, 241.729),
        sidebar_foreground: Oklch(0.222003, 0.03479, 328.979),
        sidebar_muted_foreground: Oklch(0.517366, 0.018944, 311.433),
        sidebar_row_active: Oklch(0.858363, 0.033325, 242.089),
        sidebar_row_selected: Oklch(0.842113, 0.037689, 242.174),
        sidebar_border: Oklch(0.862823, 0.011384, 256.926),
    },
    dark: Roles {
        canvas: Oklch(0.242641, 0.024125, 250.573),
        surface_raised: Oklch(0.348439, 0.019942, 253.696),
        surface_overlay: Oklch(0.398517, 0.018232, 255.72),
        text: Oklch(0.990339, 0.008411, 325.64),
        border: Oklch(0.438653, 0.039496, 245.44),
        input: Oklch(0.500905, 0.043574, 244.781),
        focus: Oklch(0.758933, 0.105833, 241.548),
        secondary: Oklch(0.358725, 0.043145, 244.911),
        muted: Oklch(0.319287, 0.036766, 246.065),
        muted_foreground: Oklch(0.691936, 0.016294, 261.588),
        error: Oklch(0.655108, 0.221148, 23.473),
        error_foreground: Oklch(0.702184, 0.189226, 22.228),
        warning: Oklch(0.772406, 0.172798, 65.367),
        warning_foreground: Oklch(0.829017, 0.171221, 81.038),
        accent_surface: Oklch(0.413315, 0.051874, 243.855),
        message_surface: Oklch(0.445224, 0.056936, 243.413),
        message_action: Oklch(0.793363, 0.105022, 199.893),
        message_action_foreground: Oklch(0.222003, 0.03479, 328.979),
        code_background: Oklch(0.29661, 0.021883, 251.968),
        sidebar: Oklch(0.290387, 0.032043, 247.274),
        sidebar_foreground: Oklch(0.990339, 0.008411, 325.64),
        sidebar_muted_foreground: Oklch(0.69099, 0.01395, 266.424),
        sidebar_row_active: Oklch(0.393878, 0.048778, 244.179),
        sidebar_row_selected: Oklch(0.413744, 0.051943, 243.848),
        sidebar_border: Oklch(0.55859, 0.019001, 256.223),
    },
};

/// Ember, `ember`.
pub const EMBER: Palette = Palette {
    light: Roles {
        canvas: Oklch(0.976527, 0.002685, 60.725),
        surface_raised: Oklch(0.953321, 0.002701, 42.266),
        surface_overlay: Oklch(0.936659, 0.002879, 29.96),
        text: Oklch(0.222003, 0.03479, 328.979),
        border: Oklch(0.870631, 0.013204, 39.431),
        input: Oklch(0.836213, 0.017153, 38.661),
        focus: Oklch(0.552831, 0.129438, 44.656),
        secondary: Oklch(0.942267, 0.01151, 50.785),
        muted: Oklch(0.950842, 0.009273, 51.528),
        muted_foreground: Oklch(0.530413, 0.018453, 341.181),
        error: Oklch(0.637823, 0.237287, 25.436),
        error_foreground: Oklch(0.509494, 0.208583, 28.513),
        warning: Oklch(0.772406, 0.172798, 65.367),
        warning_foreground: Oklch(0.549154, 0.156188, 45.359),
        accent_surface: Oklch(0.916502, 0.01832, 49.597),
        message_surface: Oklch(0.899296, 0.022939, 49.163),
        message_action: Oklch(0.516323, 0.161628, 24.82),
        message_action_foreground: Oklch(0.990339, 0.008411, 325.64),
        code_background: Oklch(0.959965, 0.002668, 47.512),
        sidebar: Oklch(0.942267, 0.01151, 50.785),
        sidebar_foreground: Oklch(0.222003, 0.03479, 328.979),
        sidebar_muted_foreground: Oklch(0.519146, 0.019214, 343.427),
        sidebar_row_active: Oklch(0.863104, 0.032855, 48.586),
        sidebar_row_selected: Oklch(0.84723, 0.037292, 48.403),
        sidebar_border: Oklch(0.865593, 0.011154, 35.246),
    },
    dark: Roles {
        canvas: Oklch(0.245899, 0.019144, 42.044),
        surface_raised: Oklch(0.351262, 0.01565, 37.592),
        surface_overlay: Oklch(0.401111, 0.014308, 34.896),
        text: Oklch(0.990339, 0.008411, 325.64),
        border: Oklch(0.44099, 0.040202, 48.807),
        input: Oklch(0.503003, 0.045721, 49.44),
        focus: Oklch(0.762174, 0.124117, 52.082),
        secondary: Oklch(0.361499, 0.044052, 49.515),
        muted: Oklch(0.322144, 0.03574, 48.309),
        muted_foreground: Oklch(0.692479, 0.015227, 30.963),
        error: Oklch(0.655108, 0.221148, 23.473),
        error_foreground: Oklch(0.702184, 0.189226, 22.228),
        warning: Oklch(0.772406, 0.172798, 65.367),
        warning_foreground: Oklch(0.829017, 0.171221, 81.038),
        accent_surface: Oklch(0.416048, 0.055354, 50.484),
        message_surface: Oklch(0.447961, 0.061874, 50.849),
        message_action: Oklch(0.747955, 0.135578, 29.432),
        message_action_foreground: Oklch(0.222003, 0.03479, 328.979),
        code_background: Oklch(0.299662, 0.017229, 39.973),
        sidebar: Oklch(0.293349, 0.029554, 46.882),
        sidebar_foreground: Oklch(0.990339, 0.008411, 325.64),
        sidebar_muted_foreground: Oklch(0.691874, 0.012538, 24.638),
        sidebar_row_active: Oklch(0.396617, 0.051353, 50.201),
        sidebar_row_selected: Oklch(0.416477, 0.055442, 50.489),
        sidebar_border: Oklch(0.560372, 0.016998, 36.179),
    },
};

/// Iris, `iris`.
pub const IRIS: Palette = Palette {
    light: Roles {
        canvas: Oklch(0.976531, 0.003855, 303.226),
        surface_raised: Oklch(0.953326, 0.004536, 307.676),
        surface_overlay: Oklch(0.936665, 0.005041, 310.132),
        text: Oklch(0.222003, 0.03479, 328.979),
        border: Oklch(0.869608, 0.018226, 303.859),
        input: Oklch(0.834773, 0.023405, 303.676),
        focus: Oklch(0.525348, 0.15373, 294.176),
        secondary: Oklch(0.941387, 0.014687, 300.474),
        muted: Oklch(0.950194, 0.011956, 300.733),
        muted_foreground: Oklch(0.529955, 0.022319, 321.556),
        error: Oklch(0.637823, 0.237287, 25.436),
        error_foreground: Oklch(0.509494, 0.208583, 28.513),
        warning: Oklch(0.772406, 0.172798, 65.367),
        warning_foreground: Oklch(0.549154, 0.156188, 45.359),
        accent_surface: Oklch(0.914882, 0.022965, 299.986),
        message_surface: Oklch(0.897143, 0.028558, 299.758),
        message_action: Oklch(0.516084, 0.185229, 340.776),
        message_action_foreground: Oklch(0.990339, 0.008411, 325.64),
        code_background: Oklch(0.95997, 0.004338, 306.542),
        sidebar: Oklch(0.941387, 0.014687, 300.474),
        sidebar_foreground: Oklch(0.222003, 0.03479, 328.979),
        sidebar_muted_foreground: Oklch(0.518417, 0.023683, 320.681),
        sidebar_row_active: Oklch(0.85971, 0.040501, 299.36),
        sidebar_row_selected: Oklch(0.843236, 0.045818, 299.198),
        sidebar_border: Oklch(0.864805, 0.015938, 305.371),
    },
    dark: Roles {
        canvas: Oklch(0.225975, 0.031062, 293.741),
        surface_raised: Oklch(0.335291, 0.026008, 296.394),
        surface_overlay: Oklch(0.386739, 0.024023, 297.509),
        text: Oklch(0.990339, 0.008411, 325.64),
        border: Oklch(0.40874, 0.058536, 295.893),
        input: Oklch(0.46756, 0.065775, 296.265),
        focus: Oklch(0.671712, 0.169136, 293.929),
        secondary: Oklch(0.325405, 0.063614, 294.23),
        muted: Oklch(0.291515, 0.05276, 294.209),
        muted_foreground: Oklch(0.663321, 0.025932, 301.862),
        error: Oklch(0.655108, 0.221148, 23.473),
        error_foreground: Oklch(0.702184, 0.189226, 22.228),
        warning: Oklch(0.772406, 0.172798, 65.367),
        warning_foreground: Oklch(0.829017, 0.171221, 81.038),
        accent_surface: Oklch(0.372436, 0.07841, 294.204),
        message_surface: Oklch(0.399975, 0.086965, 294.177),
        message_action: Oklch(0.789904, 0.130063, 337.621),
        message_action_foreground: Oklch(0.222003, 0.03479, 328.979),
        code_background: Oklch(0.281873, 0.028308, 295.193),
        sidebar: Oklch(0.266743, 0.044689, 294.138),
        sidebar_foreground: Oklch(0.990339, 0.008411, 325.64),
        sidebar_muted_foreground: Oklch(0.668773, 0.021522, 302.949),
        sidebar_row_active: Oklch(0.355677, 0.073167, 294.217),
        sidebar_row_selected: Oklch(0.372806, 0.078525, 294.203),
        sidebar_border: Oklch(0.545895, 0.027522, 299.871),
    },
};

#[cfg(test)]
mod tests;
