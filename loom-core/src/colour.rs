use core::fmt;

pub const COLOUR_FLAG_256: i32 = 0x01000000;
pub const COLOUR_FLAG_RGB: i32 = 0x02000000;
pub const COLOUR_FLAG_THEME: i32 = 0x04000000;

pub fn colour_default(c: i32) -> bool {
    c == 8 || c == 9
}

pub fn colour_is_256(c: i32) -> bool {
    c & COLOUR_FLAG_256 != 0
}

pub fn colour_is_rgb(c: i32) -> bool {
    c & COLOUR_FLAG_RGB != 0
}

pub fn colour_is_theme(c: i32) -> bool {
    c & COLOUR_FLAG_THEME != 0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColourTheme {
    Black,
    White,
    LightGrey,
    DarkGrey,
    Green,
    Yellow,
    Red,
    Blue,
    Cyan,
    Magenta,
}

impl ColourTheme {
    pub const COUNT: usize = 10;

    pub fn from_idx(i: usize) -> Option<Self> {
        match i {
            0 => Some(Self::Black),
            1 => Some(Self::White),
            2 => Some(Self::LightGrey),
            3 => Some(Self::DarkGrey),
            4 => Some(Self::Green),
            5 => Some(Self::Yellow),
            6 => Some(Self::Red),
            7 => Some(Self::Blue),
            8 => Some(Self::Cyan),
            9 => Some(Self::Magenta),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Colour {
    Default,
    Index(u8),
    Palette256(u8),
    Rgb(u8, u8, u8),
    Theme(ColourTheme),
    Terminal(u8),
}

impl Colour {
    pub fn to_256(&self) -> u8 {
        match *self {
            Self::Default => 8,
            Self::Index(i) | Self::Palette256(i) | Self::Terminal(i) => i,
            Self::Rgb(r, g, b) => rgb_to_256(r, g, b),
            Self::Theme(_) => 8,
        }
    }

    pub fn to_colour_flags(&self) -> (i32, i32) {
        match *self {
            Self::Default => (8, 0),
            Self::Index(i) => (i as i32, 0),
            Self::Palette256(i) => (i as i32 | COLOUR_FLAG_256, COLOUR_FLAG_256),
            Self::Rgb(r, g, b) => {
                let v = COLOUR_FLAG_RGB | ((r as i32) << 16) | ((g as i32) << 8) | b as i32;
                (v, COLOUR_FLAG_RGB)
            }
            Self::Theme(t) => (t as i32 | COLOUR_FLAG_THEME, COLOUR_FLAG_THEME),
            Self::Terminal(i) => (i as i32, 0),
        }
    }

    pub fn from_colour_flags(v: i32) -> Self {
        if colour_default(v) {
            return Self::Default;
        }
        if colour_is_theme(v) {
            let idx = (v & 0xff) as usize;
            return ColourTheme::from_idx(idx)
                .map(Self::Theme)
                .unwrap_or(Self::Default);
        }
        if colour_is_rgb(v) {
            let r = ((v >> 16) & 0xff) as u8;
            let g = ((v >> 8) & 0xff) as u8;
            let b = (v & 0xff) as u8;
            return Self::Rgb(r, g, b);
        }
        if colour_is_256(v) {
            return Self::Palette256((v & 0xff) as u8);
        }
        Self::Index((v & 0xff) as u8)
    }
}

impl fmt::Display for Colour {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Default => write!(f, "default"),
            Self::Index(i) => write!(f, "colour{}", i),
            Self::Palette256(i) => write!(f, "colour{}", i),
            Self::Rgb(r, g, b) => write!(f, "#{:02x}{:02x}{:02x}", r, g, b),
            Self::Theme(t) => write!(f, "theme{:?}", t),
            Self::Terminal(i) => write!(f, "terminal{}", i),
        }
    }
}

fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    if r == g && g == b {
        return if r < 8 {
            16
        } else {
            let mut v = (r as u16 - 8) / 10;
            if v > 23 {
                v = 23;
            }
            232 + v as u8
        };
    }
    let r6 = ((r as u16 * 5) / 255) as u8;
    let g6 = ((g as u16 * 5) / 255) as u8;
    let b6 = ((b as u16 * 5) / 255) as u8;
    16 + (r6 * 36) + (g6 * 6) + b6
}

#[derive(Clone, Copy, Debug)]
pub struct ColourPalette {
    pub fg: i32,
    pub bg: i32,
    pub palette: Option<[i32; 256]>,
    pub default_palette: Option<[i32; 256]>,
}

impl Default for ColourPalette {
    fn default() -> Self {
        Self {
            fg: 8,
            bg: 8,
            palette: None,
            default_palette: None,
        }
    }
}

pub fn colour_parse(s: &str) -> Option<Colour> {
    let s = s.trim();
    if s.eq_ignore_ascii_case("default") || s.eq_ignore_ascii_case("none") {
        return Some(Colour::Default);
    }
    if s.eq_ignore_ascii_case("terminal") || s.eq_ignore_ascii_case("end") {
        return Some(Colour::Default);
    }

    if let Some(rest) = s.strip_prefix("colour") {
        if let Ok(n) = rest.parse::<u8>() {
            return if n < 8 {
                Some(Colour::Index(n))
            } else {
                Some(Colour::Palette256(n))
            };
        }
    }

    if s.starts_with('#') {
        let hex = &s[1..];
        if hex.len() == 6 {
            if let Ok(v) = u32::from_str_radix(hex, 16) {
                let r = ((v >> 16) & 0xff) as u8;
                let g = ((v >> 8) & 0xff) as u8;
                let b = (v & 0xff) as u8;
                return Some(Colour::Rgb(r, g, b));
            }
        }
    }

    if let Some(rest) = s.strip_prefix("bright") {
        if let Ok(n) = rest.parse::<u8>() {
            if n < 8 {
                return Some(Colour::Index(n + 8));
            }
        }
    }

    if let Some(rest) = s.strip_prefix("theme") {
        let names = [
            "black", "white", "lightgrey", "darkgrey", "green", "yellow", "red", "blue", "cyan",
            "magenta",
        ];
        for (i, name) in names.iter().enumerate() {
            if rest.eq_ignore_ascii_case(name) || rest.eq_ignore_ascii_case(&format!("_{}", name))
            {
                return ColourTheme::from_idx(i).map(Colour::Theme);
            }
        }
    }

    let named = [
        ("black", 0),
        ("red", 1),
        ("green", 2),
        ("yellow", 3),
        ("blue", 4),
        ("magenta", 5),
        ("cyan", 6),
        ("white", 7),
    ];
    for (name, idx) in &named {
        if s.eq_ignore_ascii_case(name) {
            return Some(Colour::Index(*idx));
        }
    }

    None
}

pub fn colour_tostring(c: i32) -> String {
    Colour::from_colour_flags(c).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_basic() {
        assert_eq!(colour_parse("red"), Some(Colour::Index(1)));
        assert_eq!(colour_parse("black"), Some(Colour::Index(0)));
        assert_eq!(colour_parse("default"), Some(Colour::Default));
    }

    #[test]
    fn test_parse_colour() {
        assert_eq!(colour_parse("colour42"), Some(Colour::Palette256(42)));
        assert_eq!(colour_parse("colour7"), Some(Colour::Index(7)));
    }

    #[test]
    fn test_parse_rgb() {
        assert_eq!(colour_parse("#ff8800"), Some(Colour::Rgb(255, 136, 0)));
    }

    #[test]
    fn test_to_256() {
        assert_eq!(Colour::Rgb(255, 0, 0).to_256(), 196);
    }
}
