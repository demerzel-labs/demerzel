//! Theming: named roles map to colors, and a frame paints each face
//! through the roles.
//!
//! A [`Theme`] is the config-facing value: one optional [`Color`] per
//! [`Role`]. Three ship — `terminal`, `dark`, `light` — and config
//! defines more. The interface turns a theme into a palette once
//! at boot, at the color depth the terminal declares: truecolor when
//! `COLORTERM` says so, else the nearest entry of the 256-color
//! palette, else the nearest of the terminal's own sixteen. A
//! non-empty `NO_COLOR` keeps no color at all; the attributes — bold,
//! dim, italic, underline — stay, so headings, emphasis, links,
//! marks, and thinking still read apart.
//!
//! A role with no color leaves the terminal's own foreground in place.
//! The `terminal` theme names palette entries only, never an RGB
//! value, so it looks the same on every terminal: the terminal's
//! palette is the theme.

use std::str::FromStr;

use ratatui::style::Color as TermColor;

/// One color a role takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Color {
    /// A truecolor value. A terminal without truecolor gets the
    /// nearest palette entry.
    Rgb(u8, u8, u8),
    /// One entry of the terminal's palette. Config names the
    /// terminal's own sixteen, 0–15; the fit to a 256-color terminal
    /// lands on the cube and the grays past them.
    Indexed(u8),
}

/// The palette names config accepts, in index order.
const NAMES: [&str; 16] = [
    "black",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "white",
    "bright-black",
    "bright-red",
    "bright-green",
    "bright-yellow",
    "bright-blue",
    "bright-magenta",
    "bright-cyan",
    "bright-white",
];

/// What a color value must look like, for error messages.
const COLOR_FORMS: &str = "`#rrggbb`, a palette name such as `red` or `bright-blue`, or `none`";

impl FromStr for Color {
    type Err = String;

    /// `#rrggbb`, or one of the sixteen palette names.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let bad = || format!("`{text}` is not a color; expected {COLOR_FORMS}");
        if let Some(hex) = text.strip_prefix('#') {
            // Six hex digits and nothing else: the radix parse alone
            // would take a sign.
            if hex.len() != 6 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(bad());
            }
            let [_, r, g, b] = u32::from_str_radix(hex, 16)
                .map_err(|_| bad())?
                .to_be_bytes();
            return Ok(Color::Rgb(r, g, b));
        }
        NAMES
            .iter()
            .position(|name| *name == text)
            .map(|index| Color::Indexed(index as u8))
            .ok_or_else(bad)
    }
}

/// Parse one config value: a color, or `none` for no color.
pub fn parse_color(text: &str) -> Result<Option<Color>, String> {
    if text == "none" {
        return Ok(None);
    }
    text.parse().map(Some)
}

/// Define [`Role`], its list, and its config names from one table,
/// so a role cannot join the enum without joining the list that
/// sizes a theme.
macro_rules! roles {
    ($( $(#[$doc:meta])* $variant:ident = $name:literal, )*) => {
        /// What a theme colors. Each role is one key of a
        /// `[theme.<name>]` table, spelled with underscores:
        /// `inline_code`.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[non_exhaustive]
        pub enum Role {
            $( $(#[$doc])* $variant, )*
        }

        impl Role {
            /// Every role, in the order a theme stores them.
            pub const ALL: &'static [Role] = &[ $( Role::$variant, )* ];

            /// The config key of the role.
            pub fn name(self) -> &'static str {
                match self {
                    $( Role::$variant => $name, )*
                }
            }
        }
    };
}

roles! {
    /// Body text: replies, cards, the input box, the status bar.
    Text = "text",
    /// Secondary text: thinking blocks, notices, a card's output
    /// tail, markdown marks, link destinations, the completions not
    /// selected.
    Dim = "dim",
    /// Emphasis the interface adds: the card header, the selected
    /// completion, the browse indicator.
    Accent = "accent",
    /// Failure messages and a failed call's mark.
    Error = "error",
    /// A finished call's mark.
    Success = "success",
    /// A waiting, declined, or interrupted call's mark, and the
    /// approval question.
    Warning = "warning",
    Heading = "heading",
    /// The text of a link.
    Link = "link",
    InlineCode = "inline_code",
    /// The lines of a code fence, under the highlight classes.
    CodeBlock = "code_block",
    /// The prompt, in the input box and on the echoed line.
    Prompt = "prompt",
    /// The status bar's ground, on a screen too small for the input
    /// box's frame; a framed screen carries the status on the frame.
    StatusBar = "status_bar",
    ProgressLine = "progress_line",
    /// A `+` line of an edit preview.
    DiffAdd = "diff_add",
    /// A `-` line of an edit preview.
    DiffRemove = "diff_remove",
    /// The highlight classes of a code fence.
    Keyword = "keyword",
    String = "string",
    Comment = "comment",
    Number = "number",
}

impl Role {
    /// The slot of the role in a theme: its discriminant, which the
    /// list above assigns in order.
    fn index(self) -> usize {
        self as usize
    }
}

impl FromStr for Role {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Role::ALL
            .iter()
            .copied()
            .find(|role| role.name() == text)
            .ok_or_else(|| format!("`{text}` is not a theme role"))
    }
}

/// One color per role. [`Theme::default`] is the `terminal` theme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    colors: [Option<Color>; Role::ALL.len()],
}

impl Default for Theme {
    fn default() -> Self {
        Self::terminal()
    }
}

impl Theme {
    /// The names of the built-in themes, in the order the docs list
    /// them.
    pub const BUILT_IN: &'static [&'static str] = &["terminal", "dark", "light"];

    /// A theme with no color on any role.
    fn empty() -> Self {
        Self {
            colors: [None; Role::ALL.len()],
        }
    }

    /// The built-in theme called `name`, if there is one.
    pub fn named(name: &str) -> Option<Self> {
        match name {
            "terminal" => Some(Self::terminal()),
            "dark" => Some(Self::dark()),
            "light" => Some(Self::light()),
            _ => None,
        }
    }

    /// The terminal's own palette: named entries on the roles that
    /// need a color to read apart, no color on the rest. The default.
    pub fn terminal() -> Self {
        let mut theme = Self::empty();
        for (role, index) in [
            (Role::Error, 1),
            (Role::Success, 2),
            (Role::Warning, 3),
            (Role::InlineCode, 6),
            (Role::CodeBlock, 6),
            (Role::DiffAdd, 2),
            (Role::DiffRemove, 1),
            (Role::Keyword, 5),
            (Role::String, 2),
            (Role::Number, 3),
        ] {
            theme.set(role, Some(Color::Indexed(index)));
        }
        theme
    }

    /// Light text on a dark ground.
    pub fn dark() -> Self {
        Self::from_rgb([
            (Role::Text, 0xd0d8e4),
            (Role::Dim, 0x7b8494),
            (Role::Accent, 0x6fa8dc),
            (Role::Error, 0xe37070),
            (Role::Success, 0x8fbf6f),
            (Role::Warning, 0xd9a65a),
            (Role::Heading, 0xeef1f6),
            (Role::Link, 0x62b7c9),
            (Role::InlineCode, 0xd9a65a),
            (Role::CodeBlock, 0xcdd3de),
            (Role::Prompt, 0x6fa8dc),
            (Role::StatusBar, 0x363e4e),
            (Role::ProgressLine, 0x6fa8dc),
            (Role::DiffAdd, 0x8fbf6f),
            (Role::DiffRemove, 0xe37070),
            (Role::Keyword, 0xba8db4),
            (Role::String, 0xa6c48c),
            (Role::Comment, 0x6c7384),
            (Role::Number, 0xe0a458),
        ])
    }

    /// Dark text on a light ground.
    pub fn light() -> Self {
        Self::from_rgb([
            (Role::Text, 0x293043),
            (Role::Dim, 0x6c7384),
            (Role::Accent, 0x1f6fb2),
            (Role::Error, 0xb02a21),
            (Role::Success, 0x2c7a35),
            (Role::Warning, 0x8a6100),
            (Role::Heading, 0x1b1f24),
            (Role::Link, 0x1f6fb2),
            (Role::InlineCode, 0x8a3b00),
            (Role::CodeBlock, 0x293043),
            (Role::Prompt, 0x1f6fb2),
            (Role::StatusBar, 0xd0d8e4),
            (Role::ProgressLine, 0x1f6fb2),
            (Role::DiffAdd, 0x2c7a35),
            (Role::DiffRemove, 0xb02a21),
            (Role::Keyword, 0x7a1fa2),
            (Role::String, 0x0e5591),
            (Role::Comment, 0x6c7384),
            (Role::Number, 0x9a5200),
        ])
    }

    fn from_rgb(colors: [(Role, u32); Role::ALL.len()]) -> Self {
        let mut theme = Self::empty();
        for (role, rgb) in colors {
            let [_, r, g, b] = rgb.to_be_bytes();
            theme.set(role, Some(Color::Rgb(r, g, b)));
        }
        theme
    }

    /// The color of one role; `None` leaves the terminal's own.
    pub fn get(&self, role: Role) -> Option<Color> {
        self.colors[role.index()]
    }

    /// Set one role. `None` takes the color away.
    pub fn set(&mut self, role: Role, color: Option<Color>) {
        self.colors[role.index()] = color;
    }
}

/// The color depth the terminal declares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Depth {
    /// `NO_COLOR`: attributes only.
    None,
    Ansi16,
    Ansi256,
    TrueColor,
}

impl Depth {
    /// Read the depth from the environment, through `env` so tests
    /// pass their own. A non-empty `NO_COLOR` wins; then `COLORTERM`
    /// declares truecolor; then `TERM` declares 256 colors or a
    /// direct-color terminal; anything else is sixteen.
    pub(crate) fn detect(env: impl Fn(&str) -> Option<String>) -> Self {
        if env("NO_COLOR").is_some_and(|value| !value.is_empty()) {
            return Depth::None;
        }
        if env("COLORTERM").is_some_and(|value| value == "truecolor" || value == "24bit") {
            return Depth::TrueColor;
        }
        match env("TERM") {
            Some(term) if term.contains("direct") => Depth::TrueColor,
            Some(term) if term.contains("256color") => Depth::Ansi256,
            _ => Depth::Ansi16,
        }
    }

    /// `var_os`, decoded lossily: a `NO_COLOR` that is not UTF-8 is
    /// still set and non-empty, so it still turns color off.
    pub(crate) fn from_env() -> Self {
        Self::detect(|name| {
            std::env::var_os(name).map(|value| value.to_string_lossy().into_owned())
        })
    }
}

/// A theme fitted to one terminal: each role's color at the depth
/// the terminal declares, in the renderer's own type. Built once at
/// boot; a frame looks colors up by role.
#[derive(Debug, Clone)]
pub(crate) struct Palette {
    colors: [Option<TermColor>; Role::ALL.len()],
}

impl Palette {
    pub(crate) fn new(theme: &Theme, depth: Depth) -> Self {
        let mut colors = [None; Role::ALL.len()];
        for &role in Role::ALL {
            colors[role.index()] = theme
                .get(role)
                .and_then(|color| fit(color, depth))
                .map(term);
        }
        Self { colors }
    }

    /// The color of one role, or `None` for the terminal's own.
    pub(crate) fn color(&self, role: Role) -> Option<TermColor> {
        self.colors[role.index()]
    }
}

/// `color` at `depth`: dropped at no color, the nearest palette entry
/// under truecolor.
fn fit(color: Color, depth: Depth) -> Option<Color> {
    match (depth, color) {
        (Depth::None, _) => None,
        (Depth::TrueColor, color) => Some(color),
        (Depth::Ansi256, Color::Rgb(r, g, b)) => Some(Color::Indexed(nearest_256(r, g, b))),
        (Depth::Ansi16, Color::Rgb(r, g, b)) => Some(Color::Indexed(nearest_16(r, g, b))),
        (Depth::Ansi16, Color::Indexed(index)) if index >= 16 => {
            let (r, g, b) = palette_rgb(index);
            Some(Color::Indexed(nearest_16(r, g, b)))
        }
        (_, indexed @ Color::Indexed(_)) => Some(indexed),
    }
}

/// The value of a 256-color palette entry past the terminal's own
/// sixteen: the cube, then the grays.
fn palette_rgb(index: u8) -> (u8, u8, u8) {
    if index >= 232 {
        let gray = 8 + 10 * (index - 232);
        (gray, gray, gray)
    } else {
        let cube = usize::from(index.saturating_sub(16));
        (CUBE[cube / 36], CUBE[cube / 6 % 6], CUBE[cube % 6])
    }
}

/// The renderer's color. The first sixteen entries go out as the
/// terminal's named colors, which every terminal takes; the rest as
/// palette indexes.
fn term(color: Color) -> TermColor {
    match color {
        Color::Rgb(r, g, b) => TermColor::Rgb(r, g, b),
        Color::Indexed(index) => match index {
            0 => TermColor::Black,
            1 => TermColor::Red,
            2 => TermColor::Green,
            3 => TermColor::Yellow,
            4 => TermColor::Blue,
            5 => TermColor::Magenta,
            6 => TermColor::Cyan,
            7 => TermColor::Gray,
            8 => TermColor::DarkGray,
            9 => TermColor::LightRed,
            10 => TermColor::LightGreen,
            11 => TermColor::LightYellow,
            12 => TermColor::LightBlue,
            13 => TermColor::LightMagenta,
            14 => TermColor::LightCyan,
            15 => TermColor::White,
            index => TermColor::Indexed(index),
        },
    }
}

/// The 6x6x6 cube levels of the 256-color palette.
const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

fn distance(a: (u8, u8, u8), b: (u8, u8, u8)) -> u32 {
    let d = |x: u8, y: u8| {
        let d = i32::from(x) - i32::from(y);
        (d * d) as u32
    };
    d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
}

/// The nearest entry among the cube and the grays, 16–255. The first
/// sixteen entries are the terminal's own and vary, so they are not
/// candidates.
fn nearest_256(r: u8, g: u8, b: u8) -> u8 {
    let level = |channel: u8| {
        CUBE.iter()
            .enumerate()
            .min_by_key(|&(_, &value)| distance((channel, 0, 0), (value, 0, 0)))
            .map_or(0, |(index, _)| index)
    };
    let (ri, gi, bi) = (level(r), level(g), level(b));
    let cube = (CUBE[ri], CUBE[gi], CUBE[bi]);
    let cube_index = 16 + 36 * ri + 6 * gi + bi;

    // Grays run 8, 18, … 238 over indexes 232–255.
    let mean = (u32::from(r) + u32::from(g) + u32::from(b)) / 3;
    let gray_step = ((mean.saturating_sub(3)) / 10).min(23) as u8;
    let gray_value = 8 + 10 * gray_step;
    let gray = (gray_value, gray_value, gray_value);

    if distance((r, g, b), gray) < distance((r, g, b), cube) {
        232 + gray_step
    } else {
        cube_index as u8
    }
}

/// The nearest of the sixteen named colors, by hue. A straight
/// distance to the palette's values pulls every muted color to gray,
/// and a theme's classes then merge; so a color with chroma keeps its
/// hue, bright above mid-lightness, and only a color with little
/// chroma lands on the four grays.
fn nearest_16(r: u8, g: u8, b: u8) -> u8 {
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let chroma = f32::from(max) - f32::from(min);
    let light = (u16::from(max) + u16::from(min)) / 2;
    if chroma < 32.0 {
        return match light {
            0..=63 => 0,
            64..=159 => 8,
            160..=223 => 7,
            _ => 15,
        };
    }
    let (rf, gf, bf) = (f32::from(r), f32::from(g), f32::from(b));
    let hue = if max == r {
        ((gf - bf) / chroma).rem_euclid(6.0)
    } else if max == g {
        (bf - rf) / chroma + 2.0
    } else {
        (rf - gf) / chroma + 4.0
    } * 60.0;
    // Sectors of 60 degrees, centered on the six palette hues.
    let base = match ((hue + 30.0) % 360.0) as u16 / 60 {
        0 => 1,
        1 => 3,
        2 => 2,
        3 => 6,
        4 => 4,
        _ => 5,
    };
    if light >= 128 { base + 8 } else { base }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_parse_from_hex_and_palette_names() {
        assert_eq!("#5fafd7".parse(), Ok(Color::Rgb(0x5f, 0xaf, 0xd7)));
        assert_eq!("#FFFFFF".parse(), Ok(Color::Rgb(255, 255, 255)));
        assert_eq!("red".parse(), Ok(Color::Indexed(1)));
        assert_eq!("bright-blue".parse(), Ok(Color::Indexed(12)));
        assert_eq!(parse_color("none"), Ok(None));
        assert_eq!(parse_color("cyan"), Ok(Some(Color::Indexed(6))));
        for bad in [
            "#5faf",
            "#5fafd7ff",
            "#gggggg",
            "#+1+2+3",
            "# 1 2 3",
            "Red",
            "",
            "5fafd7",
        ] {
            let error = bad.parse::<Color>().unwrap_err();
            assert!(error.contains("not a color"), "{bad}: {error}");
        }
    }

    #[test]
    fn every_role_has_one_name_that_parses_back() {
        for &role in Role::ALL {
            assert_eq!(role.name().parse::<Role>(), Ok(role));
        }
        assert!("card_frame".parse::<Role>().is_err());
        // The list assigns each role its slot.
        for (slot, &role) in Role::ALL.iter().enumerate() {
            assert_eq!(role.index(), slot);
        }
    }

    #[test]
    fn the_built_in_themes_resolve_by_name() {
        for &name in Theme::BUILT_IN {
            assert!(Theme::named(name).is_some(), "{name}");
        }
        assert!(Theme::named("ocean").is_none());
        assert_eq!(Theme::default(), Theme::terminal());
        // The terminal theme names palette entries only.
        for &role in Role::ALL {
            assert!(!matches!(Theme::terminal().get(role), Some(Color::Rgb(..))));
        }
        // The other two color every role.
        for theme in [Theme::dark(), Theme::light()] {
            for &role in Role::ALL {
                assert!(matches!(theme.get(role), Some(Color::Rgb(..))), "{role:?}");
            }
        }
    }

    #[test]
    fn depth_comes_from_no_color_then_colorterm_then_term() {
        let env = |vars: &'static [(&str, &str)]| {
            move |name: &str| {
                vars.iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| value.to_string())
            }
        };
        assert_eq!(Depth::detect(env(&[])), Depth::Ansi16);
        assert_eq!(Depth::detect(env(&[("TERM", "xterm")])), Depth::Ansi16);
        assert_eq!(
            Depth::detect(env(&[("TERM", "xterm-256color")])),
            Depth::Ansi256
        );
        assert_eq!(
            Depth::detect(env(&[("TERM", "xterm-direct")])),
            Depth::TrueColor
        );
        assert_eq!(
            Depth::detect(env(&[("TERM", "xterm"), ("COLORTERM", "truecolor")])),
            Depth::TrueColor
        );
        assert_eq!(
            Depth::detect(env(&[("COLORTERM", "24bit")])),
            Depth::TrueColor
        );
        assert_eq!(
            Depth::detect(env(&[("COLORTERM", "truecolor"), ("NO_COLOR", "1")])),
            Depth::None
        );
        // An empty NO_COLOR is unset, per the convention.
        assert_eq!(
            Depth::detect(env(&[("COLORTERM", "truecolor"), ("NO_COLOR", "")])),
            Depth::TrueColor
        );
    }

    #[test]
    fn a_palette_fits_each_color_to_the_depth() {
        let theme = Theme::dark();
        let truecolor = Palette::new(&theme, Depth::TrueColor);
        assert_eq!(
            truecolor.color(Role::Accent),
            Some(TermColor::Rgb(0x6f, 0xa8, 0xdc))
        );
        let indexed = Palette::new(&theme, Depth::Ansi256);
        assert!(matches!(
            indexed.color(Role::Accent),
            Some(TermColor::Indexed(16..=255))
        ));
        let sixteen = Palette::new(&theme, Depth::Ansi16);
        assert!(!matches!(
            sixteen.color(Role::Accent),
            Some(TermColor::Rgb(..) | TermColor::Indexed(_))
        ));
        let none = Palette::new(&theme, Depth::None);
        for &role in Role::ALL {
            assert_eq!(none.color(role), None);
        }
        // Palette entries pass through every depth but none.
        let terminal = Palette::new(&Theme::terminal(), Depth::Ansi16);
        assert_eq!(terminal.color(Role::Error), Some(TermColor::Red));
        assert_eq!(terminal.color(Role::Text), None);
        // An entry past sixteen — reachable through the API, not
        // config — fits to sixteen like the value it stands for.
        let mut past = Theme::terminal();
        past.set(Role::Accent, Some(Color::Indexed(196)));
        past.set(Role::Dim, Some(Color::Indexed(244)));
        let sixteen = Palette::new(&past, Depth::Ansi16);
        assert_eq!(sixteen.color(Role::Accent), Some(TermColor::Red));
        assert_eq!(sixteen.color(Role::Dim), Some(TermColor::DarkGray));
        assert_eq!(palette_rgb(196), (255, 0, 0));
        assert_eq!(palette_rgb(67), (95, 135, 175));
        assert_eq!(palette_rgb(232), (8, 8, 8));
        assert_eq!(palette_rgb(255), (238, 238, 238));
    }

    #[test]
    fn nearest_entries_land_on_the_cube_and_the_grays() {
        assert_eq!(nearest_256(0, 0, 0), 16);
        assert_eq!(nearest_256(255, 255, 255), 231);
        assert_eq!(nearest_256(255, 0, 0), 196);
        assert_eq!(nearest_256(95, 135, 175), 67);
        // A mid gray is closer to the gray ramp than the cube.
        assert_eq!(nearest_256(128, 128, 128), 244);
        assert_eq!(nearest_256(8, 8, 8), 232);
        assert_eq!(nearest_256(238, 238, 238), 255);
        assert_eq!(nearest_16(0, 0, 0), 0);
        assert_eq!(nearest_16(200, 10, 10), 1);
        assert_eq!(nearest_16(255, 255, 255), 15);
        assert_eq!(nearest_16(100, 100, 255), 12);
        // A pastel keeps its hue; a muted gray-blue is gray.
        assert_eq!(nearest_16(186, 141, 180), 13);
        assert_eq!(nearest_16(123, 132, 148), 8);
        assert_eq!(nearest_16(238, 241, 246), 15);
        assert_eq!(nearest_16(140, 100, 0), 3);
    }

    #[test]
    fn the_built_in_themes_keep_their_classes_apart_at_sixteen_colors() {
        for theme in [Theme::dark(), Theme::light()] {
            let palette = Palette::new(&theme, Depth::Ansi16);
            let classes = [Role::Keyword, Role::String, Role::Number, Role::Comment]
                .map(|role| palette.color(role));
            for (i, a) in classes.iter().enumerate() {
                for b in &classes[i + 1..] {
                    assert_ne!(a, b, "{classes:?}");
                }
            }
            assert_ne!(palette.color(Role::Text), palette.color(Role::Dim));
        }
    }
}
