//! Color themes for Astrum's own interface (tab bar, status bar, agent
//! panel, menus). With `theme_terminal = true`, the default, a theme's
//! palette is used for what programs draw as well. The default theme is
//! the one that has no colors of its own: everything is drawn in the host
//! terminal's.

use ratatui::style::Color;

#[derive(Clone, Debug)]
pub struct Theme {
    pub name: &'static str,
    pub label: &'static str,
    /// Uses only the host terminal's own palette; no RGB backgrounds.
    pub flat: bool,
    /// Tab bar and status bar background.
    pub bar: Color,
    /// Panel / menu background.
    pub bg: Color,
    /// Raised surfaces: active tab, input box, cards.
    pub surface: Color,
    /// Hover and selected-row background.
    pub overlay: Color,
    pub border: Color,
    pub text: Color,
    pub subtext: Color,
    pub muted: Color,
    pub accent: Color,
    pub accent2: Color,
    pub green: Color,
    pub yellow: Color,
    pub red: Color,
    pub blue: Color,
    pub selection: Color,
    /// Text drawn on top of an accent-colored chip.
    pub on_accent: Color,
    pub term_fg: Color,
    pub term_bg: Color,
    pub ansi: [Color; 16],
}

const fn hex(v: u32) -> Color {
    Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

const fn pal(v: [u32; 16]) -> [Color; 16] {
    let mut out = [Color::Reset; 16];
    let mut i = 0;
    while i < 16 {
        out[i] = hex(v[i]);
        i += 1;
    }
    out
}

struct Spec {
    name: &'static str,
    label: &'static str,
    // bar, bg, surface, overlay, border, text, subtext, muted
    base: [u32; 8],
    // accent, accent2, green, yellow, red, blue, selection
    hues: [u32; 7],
    ansi: [u32; 16],
}

impl Spec {
    const fn build(&self) -> Theme {
        Theme {
            name: self.name,
            label: self.label,
            flat: false,
            bar: hex(self.base[0]),
            bg: hex(self.base[1]),
            surface: hex(self.base[2]),
            overlay: hex(self.base[3]),
            border: hex(self.base[4]),
            text: hex(self.base[5]),
            subtext: hex(self.base[6]),
            muted: hex(self.base[7]),
            accent: hex(self.hues[0]),
            accent2: hex(self.hues[1]),
            green: hex(self.hues[2]),
            yellow: hex(self.hues[3]),
            red: hex(self.hues[4]),
            blue: hex(self.hues[5]),
            selection: hex(self.hues[6]),
            on_accent: hex(self.base[0]),
            term_fg: hex(self.base[5]),
            term_bg: hex(self.base[1]),
            ansi: pal(self.ansi),
        }
    }
}

const SPECS: &[Spec] = &[
    Spec {
        name: "astrum",
        label: "Astrum Night",
        base: [0x0b0c16, 0x10111c, 0x1b1d2e, 0x272a42, 0x363a5c, 0xdcdff5, 0xa9aed1, 0x6a6f94],
        hues: [0x9d7cff, 0x5de4ff, 0x7ee6a8, 0xffd479, 0xff7a90, 0x7aa8ff, 0x353a66],
        ansi: [
            0x1b1d2e, 0xff7a90, 0x7ee6a8, 0xffd479, 0x7aa8ff, 0xc59bff, 0x5de4ff, 0xc8cbe6, 0x4a4f75, 0xff9aab,
            0xa2f0c2, 0xffe2a1, 0xa3c3ff, 0xd8bcff, 0x92eeff, 0xf2f3ff,
        ],
    },
    Spec {
        name: "nebula",
        label: "Nebula",
        base: [0x130b1c, 0x1a1024, 0x281a36, 0x38254a, 0x4e3566, 0xf1e4ff, 0xc9b3e0, 0x80699a],
        hues: [0xff6ac1, 0x8be9fd, 0x5af78e, 0xf3f99d, 0xff5c57, 0x57c7ff, 0x4a2f63],
        ansi: [
            0x281a36, 0xff5c57, 0x5af78e, 0xf3f99d, 0x57c7ff, 0xff6ac1, 0x9aedfe, 0xf1e4ff, 0x80699a, 0xff8a86,
            0x8dfab0, 0xf8fcc0, 0x8dd8ff, 0xff9dd6, 0xc0f5ff, 0xffffff,
        ],
    },
    Spec {
        name: "tokyo-night",
        label: "Tokyo Night",
        base: [0x16161e, 0x1a1b26, 0x24283b, 0x2f3549, 0x3b4261, 0xc0caf5, 0xa9b1d6, 0x565f89],
        hues: [0x7aa2f7, 0xbb9af7, 0x9ece6a, 0xe0af68, 0xf7768e, 0x7dcfff, 0x283457],
        ansi: [
            0x15161e, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xa9b1d6, 0x414868, 0xf7768e,
            0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xc0caf5,
        ],
    },
    Spec {
        name: "catppuccin",
        label: "Catppuccin Mocha",
        base: [0x11111b, 0x1e1e2e, 0x313244, 0x45475a, 0x585b70, 0xcdd6f4, 0xbac2de, 0x6c7086],
        hues: [0xcba6f7, 0x89dceb, 0xa6e3a1, 0xf9e2af, 0xf38ba8, 0x89b4fa, 0x45475a],
        ansi: [
            0x45475a, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xbac2de, 0x585b70, 0xf38ba8,
            0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xa6adc8,
        ],
    },
    Spec {
        name: "catppuccin-latte",
        label: "Catppuccin Latte (light)",
        base: [0xdce0e8, 0xeff1f5, 0xccd0da, 0xbcc0cc, 0xacb0be, 0x4c4f69, 0x5c5f77, 0x8c8fa1],
        hues: [0x8839ef, 0x04a5e5, 0x40a02b, 0xdf8e1d, 0xd20f39, 0x1e66f5, 0xbcc0cc],
        ansi: [
            0x5c5f77, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xacb0be, 0x6c6f85, 0xd20f39,
            0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xbcc0cc,
        ],
    },
    Spec {
        name: "gruvbox",
        label: "Gruvbox Dark",
        base: [0x1d2021, 0x282828, 0x3c3836, 0x504945, 0x665c54, 0xebdbb2, 0xd5c4a1, 0x928374],
        hues: [0xfabd2f, 0x8ec07c, 0xb8bb26, 0xfabd2f, 0xfb4934, 0x83a598, 0x504945],
        ansi: [
            0x282828, 0xcc241d, 0x98971a, 0xd79921, 0x458588, 0xb16286, 0x689d6a, 0xa89984, 0x928374, 0xfb4934,
            0xb8bb26, 0xfabd2f, 0x83a598, 0xd3869b, 0x8ec07c, 0xebdbb2,
        ],
    },
    Spec {
        name: "nord",
        label: "Nord",
        base: [0x272c36, 0x2e3440, 0x3b4252, 0x434c5e, 0x4c566a, 0xeceff4, 0xd8dee9, 0x7b88a1],
        hues: [0x88c0d0, 0xb48ead, 0xa3be8c, 0xebcb8b, 0xbf616a, 0x81a1c1, 0x434c5e],
        ansi: [
            0x3b4252, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x88c0d0, 0xe5e9f0, 0x4c566a, 0xbf616a,
            0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x8fbcbb, 0xeceff4,
        ],
    },
    Spec {
        name: "dracula",
        label: "Dracula",
        base: [0x21222c, 0x282a36, 0x343746, 0x44475a, 0x6272a4, 0xf8f8f2, 0xe2e2dc, 0x7c88b4],
        hues: [0xbd93f9, 0xff79c6, 0x50fa7b, 0xf1fa8c, 0xff5555, 0x8be9fd, 0x44475a],
        ansi: [
            0x21222c, 0xff5555, 0x50fa7b, 0xf1fa8c, 0xbd93f9, 0xff79c6, 0x8be9fd, 0xf8f8f2, 0x6272a4, 0xff6e6e,
            0x69ff94, 0xffffa5, 0xd6acff, 0xff92df, 0xa4ffff, 0xffffff,
        ],
    },
    Spec {
        name: "rose-pine",
        label: "Rosé Pine",
        base: [0x191724, 0x1f1d2e, 0x26233a, 0x403d52, 0x524f67, 0xe0def4, 0xc4c0dd, 0x6e6a86],
        hues: [0xc4a7e7, 0xebbcba, 0x9ccfd8, 0xf6c177, 0xeb6f92, 0x3e8fb0, 0x403d52],
        ansi: [
            0x26233a, 0xeb6f92, 0x31748f, 0xf6c177, 0x9ccfd8, 0xc4a7e7, 0xebbcba, 0xe0def4, 0x6e6a86, 0xeb6f92,
            0x31748f, 0xf6c177, 0x9ccfd8, 0xc4a7e7, 0xebbcba, 0xe0def4,
        ],
    },
    Spec {
        name: "kanagawa",
        label: "Kanagawa",
        base: [0x16161d, 0x1f1f28, 0x2a2a37, 0x363646, 0x54546d, 0xdcd7ba, 0xc8c093, 0x727169],
        hues: [0x7e9cd8, 0x957fb8, 0x98bb6c, 0xe6c384, 0xe46876, 0x7fb4ca, 0x2d4f67],
        ansi: [
            0x090618, 0xc34043, 0x76946a, 0xc0a36e, 0x7e9cd8, 0x957fb8, 0x6a9589, 0xc8c093, 0x727169, 0xe82424,
            0x98bb6c, 0xe6c384, 0x7fb4ca, 0x938aa9, 0x7aa89f, 0xdcd7ba,
        ],
    },
    Spec {
        name: "everforest",
        label: "Everforest",
        base: [0x232a2e, 0x2d353b, 0x343f44, 0x3d484d, 0x4f585e, 0xd3c6aa, 0x9da9a0, 0x7a8478],
        hues: [0xa7c080, 0x7fbbb3, 0xa7c080, 0xdbbc7f, 0xe67e80, 0x7fbbb3, 0x475258],
        ansi: [
            0x343f44, 0xe67e80, 0xa7c080, 0xdbbc7f, 0x7fbbb3, 0xd699b6, 0x83c092, 0xd3c6aa, 0x7a8478, 0xe67e80,
            0xa7c080, 0xdbbc7f, 0x7fbbb3, 0xd699b6, 0x83c092, 0xd3c6aa,
        ],
    },
    Spec {
        name: "one-dark",
        label: "One Dark",
        base: [0x21252b, 0x282c34, 0x2c313a, 0x3e4451, 0x4b5263, 0xabb2bf, 0x9da5b4, 0x5c6370],
        hues: [0x61afef, 0xc678dd, 0x98c379, 0xe5c07b, 0xe06c75, 0x56b6c2, 0x3e4451],
        ansi: [
            0x282c34, 0xe06c75, 0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xabb2bf, 0x5c6370, 0xe06c75,
            0x98c379, 0xe5c07b, 0x61afef, 0xc678dd, 0x56b6c2, 0xffffff,
        ],
    },
    // Neon and high-contrast themes. Each is built from a handful of
    // signature colors; the surfaces, text shades and terminal palette
    // around them were filled in to match.
    Spec {
        name: "synthwave",
        label: "Synthwave",
        base: [0x241b2f, 0x191325, 0x2a2139, 0x362b54, 0x495495, 0xf4eeff, 0xc5bfe6, 0x848bbd],
        hues: [0xff2e97, 0x00f0ff, 0x72f1b8, 0xfede5d, 0xfe4450, 0xb967ff, 0x3b2f66],
        ansi: [
            0x2a2139, 0xfe4450, 0x72f1b8, 0xfede5d, 0x7b8cff, 0xff7edb, 0x36f9f6, 0xe6defa, 0x6b5f94, 0xff6b75,
            0x9bf6cf, 0xffe98a, 0xa3b0ff, 0xffa3e6, 0x7dfbfa, 0xffffff,
        ],
    },
    Spec {
        name: "outrun",
        label: "Outrun",
        base: [0x261447, 0x1a0e31, 0x2b1852, 0x3e2473, 0x5f3696, 0xf7ecff, 0xcdb9ea, 0x9484b0],
        hues: [0xff6c11, 0x2de2e6, 0x61e786, 0xf9c80e, 0xff3864, 0x9d7bff, 0x46267d],
        ansi: [
            0x2b1852, 0xff3864, 0x61e786, 0xf9c80e, 0x8c6cff, 0xf85ec9, 0x2de2e6, 0xe6d8f5, 0x6b5a8e, 0xff6c8a,
            0x8ff0a8, 0xffdb5c, 0xb09bff, 0xff8fdd, 0x7ff1f3, 0xffffff,
        ],
    },
    Spec {
        name: "blade-runner-2049",
        label: "Blade Runner 2049",
        base: [0x001414, 0x001f1f, 0x063131, 0x0a4646, 0x0b8a79, 0xd8fff9, 0xa5ded6, 0x62a89f],
        hues: [0x00ffe5, 0xffb74d, 0x79e96d, 0xe6e373, 0xff6e6e, 0x5cc8ff, 0x0d5c55],
        ansi: [
            0x063131, 0xff6e6e, 0x79e96d, 0xe6e373, 0x4db8ff, 0xe89cff, 0x00ffe5, 0xc9ece7, 0x3d8f86, 0xff9696,
            0xa4f29b, 0xf2f0a1, 0x8dd3ff, 0xf0bfff, 0x7dfff1, 0xf2fffd,
        ],
    },
    Spec {
        name: "cyberpunk-2077",
        label: "Cyberpunk 2077",
        base: [0x060144, 0x03102c, 0x0a1e4d, 0x112f70, 0x1d55a6, 0xe2f7ff, 0xa9cdf2, 0x6289c9],
        hues: [0x0ef3ff, 0xff2e97, 0x3dd69c, 0xffd400, 0xff5a36, 0x3e8efd, 0x123a7a],
        ansi: [
            0x0a1e4d, 0xff5a36, 0x3dd69c, 0xffd400, 0x3e8efd, 0xff2e97, 0x0ef3ff, 0xcfe6f7, 0x3a5a94, 0xff8566,
            0x6fe8b8, 0xffe14d, 0x74aeff, 0xff6bb5, 0x6ff8ff, 0xffffff,
        ],
    },
    Spec {
        name: "chaos-theory",
        label: "Chaos Theory",
        base: [0x20261d, 0x0d0d0d, 0x1a1d18, 0x2b3127, 0x495945, 0xeaf2de, 0xb7c4a5, 0x818c69],
        hues: [0x61f21d, 0x0fc9f2, 0x61f21d, 0xedf25e, 0xf2594b, 0x4aa8ff, 0x2f3d24],
        ansi: [
            0x1a1d18, 0xf2594b, 0x61f21d, 0xedf25e, 0x3aa0ff, 0xd97bf2, 0x0fc9f2, 0xd5dec8, 0x5c664d, 0xff8275,
            0x92ff5c, 0xf6f99a, 0x7cc0ff, 0xe9a6ff, 0x6fe0ff, 0xfafff0,
        ],
    },
    Spec {
        name: "matrix",
        label: "Matrix",
        base: [0x081c0d, 0x04120a, 0x0a2414, 0x0f381d, 0x147a32, 0xb8ffc8, 0x7fe89a, 0x4e9a63],
        hues: [0x00ff41, 0x7fff9e, 0x00ff41, 0xadff2f, 0xff5555, 0x3fd8a0, 0x0f4d24],
        ansi: [
            0x0a2414, 0xff5555, 0x00ff41, 0xadff2f, 0x35c7a8, 0xb0e56b, 0x7fff9e, 0xb8ffc8, 0x2f7a45, 0xff8080,
            0x66ff8a, 0xcaff70, 0x6fe3c8, 0xd0f58f, 0xb5ffc6, 0xe6ffec,
        ],
    },
    // The Matrix theme's shape (one color on near-black), built around
    // #ff0068, and made for long hours in a dark room: the text itself is
    // that color, not a lighter tint of it, and nothing is brighter than
    // it has to be.
    Spec {
        name: "mcdermott",
        label: "McDermott",
        base: [0x0a0205, 0x040102, 0x12040a, 0x240813, 0x7a1440, 0xff0068, 0xda3a75, 0x93214a],
        hues: [0xff0068, 0xff7fae, 0xff0068, 0xffd04a, 0xff6b2c, 0xc46bff, 0x4d0f2a],
        ansi: [
            0x12040a, 0xff6b2c, 0xff0068, 0xffb347, 0xb86bff, 0xff5fd0, 0xe0447f, 0xff7fae, 0x7a2f4c, 0xff9466,
            0xff5c9a, 0xffd27a, 0xd09bff, 0xff93e0, 0xff7fae, 0xffb8d0,
        ],
    },
    Spec {
        name: "monokai",
        label: "Monokai",
        base: [0x1d1e19, 0x272822, 0x34352d, 0x45443a, 0x625f50, 0xf8f8f2, 0xcfcfc2, 0x8f8b76],
        hues: [0xf92672, 0x66d9ef, 0xa6e22e, 0xe6db74, 0xf92672, 0xae81ff, 0x49483e],
        ansi: [
            0x272822, 0xf92672, 0xa6e22e, 0xf4bf75, 0x66d9ef, 0xae81ff, 0xa1efe4, 0xf8f8f2, 0x75715e, 0xf92672,
            0xa6e22e, 0xe6db74, 0x66d9ef, 0xae81ff, 0xa1efe4, 0xf9f8f5,
        ],
    },
    Spec {
        name: "solarized",
        label: "Solarized Dark",
        base: [0x00212b, 0x002b36, 0x073642, 0x0b4555, 0x586e75, 0xa7b6b6, 0x839496, 0x586e75],
        hues: [0x268bd2, 0x2aa198, 0x859900, 0xb58900, 0xdc322f, 0x268bd2, 0x0b4555],
        ansi: [
            0x073642, 0xdc322f, 0x859900, 0xb58900, 0x268bd2, 0xd33682, 0x2aa198, 0xeee8d5, 0x002b36, 0xcb4b16,
            0x586e75, 0x657b83, 0x839496, 0x6c71c4, 0x93a1a1, 0xfdf6e3,
        ],
    },
];

/// A theme that only uses the host terminal's own 16-color palette, so it
/// matches whatever color scheme the terminal app is already using.
fn terminal_theme() -> Theme {
    let mut ansi = [Color::Reset; 16];
    for (i, c) in ansi.iter_mut().enumerate() {
        *c = Color::Indexed(i as u8);
    }
    Theme {
        name: "terminal",
        label: "Terminal (follow host colors)",
        flat: true,
        bar: Color::Reset,
        bg: Color::Reset,
        surface: Color::Reset,
        overlay: Color::Reset,
        border: Color::Indexed(8),
        text: Color::Reset,
        subtext: Color::Reset,
        muted: Color::Indexed(8),
        accent: Color::Indexed(5),
        accent2: Color::Indexed(6),
        green: Color::Indexed(2),
        yellow: Color::Indexed(3),
        red: Color::Indexed(1),
        blue: Color::Indexed(4),
        selection: Color::Indexed(8),
        on_accent: Color::Indexed(0),
        term_fg: Color::Reset,
        term_bg: Color::Reset,
        ansi,
    }
}

pub fn all() -> Vec<Theme> {
    let mut themes: Vec<Theme> = SPECS.iter().map(Spec::build).collect();
    themes.push(terminal_theme());
    themes
}

pub fn by_name(name: &str) -> Option<Theme> {
    let wanted = name.trim().to_ascii_lowercase().replace([' ', '_'], "-");
    all().into_iter().find(|t| t.name == wanted)
}

/// The theme when none is chosen (or the one named is unknown): the
/// terminal's own colors.
pub fn default_theme() -> Theme {
    terminal_theme()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Relative luminance of a color, as used for contrast ratios.
    fn luminance(c: Color) -> f64 {
        let Color::Rgb(r, g, b) = c else { panic!("not an RGB color: {c:?}") };
        let channel = |v: u8| {
            let v = v as f64 / 255.0;
            if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
    }

    fn contrast(a: Color, b: Color) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    #[test]
    fn names_are_unique_and_found() {
        let themes = all();
        for t in &themes {
            assert_eq!(themes.iter().filter(|o| o.name == t.name).count(), 1, "{}", t.name);
            assert!(t.name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'), "{}", t.name);
            assert_eq!(by_name(t.name).map(|f| f.label), Some(t.label));
        }
        assert_eq!(by_name("Blade Runner 2049").map(|t| t.name), Some("blade-runner-2049"));
    }

    #[test]
    fn every_theme_is_readable() {
        let mut problems = Vec::new();
        for t in all().into_iter().filter(|t| !t.flat) {
            let mut check = |what: &str, fg: Color, bg: Color, at_least: f64| {
                let ratio = contrast(fg, bg);
                if ratio < at_least {
                    problems.push(format!("{}: {what} has contrast {ratio:.2}, wanted {at_least}", t.name));
                }
            };
            // A light theme cannot give its colored text as much contrast
            // as a dark one gives bright colors on black.
            let light = luminance(t.bg) > 0.5;
            // Body text on the panel and on raised surfaces. One theme is
            // dim on purpose, for a dark room; it still has to meet the
            // usual standard for body text (4.5).
            let dim = t.name == "mcdermott";
            check("text on bg", t.text, t.bg, if dim { 4.5 } else { 6.5 });
            check("text on surface", t.text, t.surface, 5.0);
            check("subtext on bg", t.subtext, t.bg, 4.5);
            // Hints and bar labels.
            check("muted on bg", t.muted, t.bg, 2.3);
            check("subtext on bar", t.subtext, t.bar, 4.5);
            // Colored text, and dark text on a colored chip.
            for (name, hue) in [("accent", t.accent), ("accent2", t.accent2), ("green", t.green), ("yellow", t.yellow), ("red", t.red)] {
                check(&format!("{name} on bg"), hue, t.bg, if light { 2.3 } else { 3.0 });
            }
            check("chip text on accent", t.on_accent, t.accent, 4.0);
        }
        assert!(problems.is_empty(), "\n{}", problems.join("\n"));
    }
}
