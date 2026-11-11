//! Color themes for Astrum's own interface (tab bar, status bar, agent
//! panel, menus). Terminal content keeps the host terminal's colors unless
//! `theme_terminal = true`, in which case the theme's ANSI palette is used.

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

pub fn default_theme() -> Theme {
    SPECS[0].build()
}
