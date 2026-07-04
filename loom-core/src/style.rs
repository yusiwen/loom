use crate::grid_cell::{GridCell, GRID_ATTR_BLINK, GRID_ATTR_BRIGHT, GRID_ATTR_DIM, GRID_ATTR_HIDDEN, GRID_ATTR_ITALICS, GRID_ATTR_OVERLINE, GRID_ATTR_REVERSE, GRID_ATTR_STRIKETHROUGH, GRID_ATTR_UNDERSCORE};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StyleAlign {
    Default,
    Left,
    Centre,
    Right,
    AbsoluteCentre,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StyleList {
    Off,
    On,
    Focus,
    LeftMarker,
    RightMarker,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StyleRangeType {
    None,
    Left,
    Right,
    Pane,
    Window,
    Session,
    User,
    Control,
}

#[derive(Clone, Copy, Debug)]
pub struct StyleRange {
    pub range_type: StyleRangeType,
    pub argument: u32,
    pub string: [u8; 16],
    pub start: u32,
    pub end: u32,
}

#[derive(Clone, Copy, Debug)]
pub enum StyleDefaultType {
    Base,
    Push,
    Pop,
    Set,
}

pub const STYLE_WIDTH_DEFAULT: i32 = -1;
pub const STYLE_PAD_DEFAULT: i32 = -1;

#[derive(Clone, Copy, Debug)]
pub struct Style {
    pub gc: GridCell,
    pub ignore: bool,
    pub dim: bool,
    pub fill: i32,
    pub align: StyleAlign,
    pub list: StyleList,
    pub range_type: StyleRangeType,
    pub range_argument: u32,
    pub range_string: [u8; 16],
    pub width: i32,
    pub width_percentage: i32,
    pub pad: i32,
    pub default_type: StyleDefaultType,
    pub link: u32,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            gc: GridCell::default_cell(),
            ignore: false,
            dim: false,
            fill: 8,
            align: StyleAlign::Default,
            list: StyleList::Off,
            range_type: StyleRangeType::None,
            range_argument: 0,
            range_string: [0u8; 16],
            width: STYLE_WIDTH_DEFAULT,
            width_percentage: 0,
            pad: STYLE_PAD_DEFAULT,
            default_type: StyleDefaultType::Base,
            link: 0,
        }
    }
}

impl Style {
    pub fn is_default(&self) -> bool {
        self.gc.fg == 8
            && self.gc.bg == 8
            && self.gc.attr == 0
            && self.fill == 8
            && self.align == StyleAlign::Default
    }

    pub fn apply(&self, gc: &mut GridCell) {
        if self.ignore {
            return;
        }
        if self.gc.fg != 8 {
            gc.fg = self.gc.fg;
            gc.flags = (gc.flags & !0x03) | (self.gc.flags & 0x03);
        }
        if self.gc.bg != 8 {
            gc.bg = self.gc.bg;
            gc.flags = (gc.flags & !0x06) | (self.gc.flags & 0x06);
        }
        if self.gc.us != 8 {
            gc.us = self.gc.us;
        }
        if self.gc.attr != 0 {
            gc.attr |= self.gc.attr;
        }
        if self.dim {
            gc.attr |= GRID_ATTR_DIM;
        }
    }
}

pub const STYLE_ATTR_MASK: u16 = !0;

pub fn attr_to_string(attr: u16) -> String {
    let mut parts = Vec::new();
    if attr & GRID_ATTR_BRIGHT != 0 { parts.push("bright"); }
    if attr & GRID_ATTR_DIM != 0 { parts.push("dim"); }
    if attr & GRID_ATTR_UNDERSCORE != 0 { parts.push("underscore"); }
    if attr & GRID_ATTR_BLINK != 0 { parts.push("blink"); }
    if attr & GRID_ATTR_REVERSE != 0 { parts.push("reverse"); }
    if attr & GRID_ATTR_HIDDEN != 0 { parts.push("hidden"); }
    if attr & GRID_ATTR_ITALICS != 0 { parts.push("italics"); }
    if attr & GRID_ATTR_STRIKETHROUGH != 0 { parts.push("strikethrough"); }
    if attr & GRID_ATTR_OVERLINE != 0 { parts.push("overline"); }
    if attr & GRID_ATTR_UNDERSCORE != 0 {
        parts.push("underscore");
    }
    parts.join(",")
}

pub fn attr_parse(s: &str) -> u16 {
    let mut attr = 0u16;
    for part in s.split(',') {
        let p = part.trim();
        match p {
            "bright" | "bold" => attr |= GRID_ATTR_BRIGHT,
            "dim" => attr |= GRID_ATTR_DIM,
            "underscore" | "underline" | "us" => attr |= GRID_ATTR_UNDERSCORE,
            "blink" => attr |= GRID_ATTR_BLINK,
            "reverse" => attr |= GRID_ATTR_REVERSE,
            "hidden" => attr |= GRID_ATTR_HIDDEN,
            "italics" | "italic" => attr |= GRID_ATTR_ITALICS,
            "strikethrough" | "strike" | "s" => attr |= GRID_ATTR_STRIKETHROUGH,
            "overline" | "ol" => attr |= GRID_ATTR_OVERLINE,
            _ => {}
        }
    }
    attr
}

pub fn style_parse(s: &str) -> Style {
    let mut style = Style::default();
    if s.is_empty() {
        return style;
    }
    let delimiters = [' ', ',', '\n'];
    let mut pos = 0;
    let bytes = s.as_bytes();
    while pos < bytes.len() {
        while pos < bytes.len() && delimiters.contains(&(bytes[pos] as char)) {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }
        let end = pos;
        while end < bytes.len() && !delimiters.contains(&(bytes[end] as char)) {
            let _ = end;
        }
        // parse kv pair
        let eq_pos = s[pos..].find('=');
        if let Some(eq) = eq_pos {
            let key = s[pos..pos + eq].trim();
            let val_end = s[pos + eq + 1..]
                .find(|c: char| delimiters.contains(&c))
                .map(|i| pos + eq + 1 + i)
                .unwrap_or(s.len());
            let val = s[pos + eq + 1..val_end].trim();
            apply_style_key(&mut style, key, val);
            pos = val_end;
        } else {
            let end2 = s[pos..]
                .find(|c: char| delimiters.contains(&c))
                .map(|i| pos + i)
                .unwrap_or(s.len());
            let key = s[pos..end2].trim();
            if !key.is_empty() {
                // Boolean attribute
                style.gc.attr |= attr_parse(key);
            }
            pos = end2;
        }
    }
    style
}

fn apply_style_key(style: &mut Style, key: &str, val: &str) {
    match key {
        "fg" | "foreground" => {
            if let Some(c) = crate::colour::colour_parse(val) {
                let (v, _) = c.to_colour_flags();
                style.gc.fg = v;
            }
        }
        "bg" | "background" => {
            if let Some(c) = crate::colour::colour_parse(val) {
                let (v, _) = c.to_colour_flags();
                style.gc.bg = v;
            }
        }
        "us" | "ul" | "underline" => {
            if let Some(c) = crate::colour::colour_parse(val) {
                let (v, _) = c.to_colour_flags();
                style.gc.us = v;
            }
        }
        "nobold" | "nodim" | "nounderscore" | "noblink" | "noreverse" | "nohidden"
        | "noitalics" | "nostrikethrough" | "no_overline" => {
            style.gc.attr &= !attr_parse(&key[2..]);
        }
        "align" => {
            style.align = match val {
                "left" => StyleAlign::Left,
                "centre" | "center" => StyleAlign::Centre,
                "right" => StyleAlign::Right,
                "absolute-centre" | "absolute-center" => StyleAlign::AbsoluteCentre,
                _ => StyleAlign::Default,
            };
        }
        "fill" => {
            if let Some(c) = crate::colour::colour_parse(val) {
                let (v, _) = c.to_colour_flags();
                style.fill = v;
            }
        }
        "list" => {
            style.list = match val {
                "on" => StyleList::On,
                "focus" => StyleList::Focus,
                "left-marker" => StyleList::LeftMarker,
                "right-marker" => StyleList::RightMarker,
                _ => StyleList::Off,
            };
        }
        "width" => {
            if let Some(n) = val.parse::<i32>().ok() {
                style.width = n;
            }
        }
        "pad" => {
            if let Some(n) = val.parse::<i32>().ok() {
                style.pad = n;
            }
        }
        "range" => {
            style.range_type = match val {
                "left" => StyleRangeType::Left,
                "right" => StyleRangeType::Right,
                "pane" => StyleRangeType::Pane,
                "window" => StyleRangeType::Window,
                "session" => StyleRangeType::Session,
                "user" => StyleRangeType::User,
                _ => StyleRangeType::None,
            };
        }
        "default" => {
            style.default_type = match val {
                "push" => StyleDefaultType::Push,
                "pop" => StyleDefaultType::Pop,
                "set" => StyleDefaultType::Set,
                _ => StyleDefaultType::Base,
            };
        }
        _ => {
            // Unknown key, maybe an attribute
            style.gc.attr |= attr_parse(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_attr_parse() {
        let attr = attr_parse("bright,underscore");
        assert!(attr & GRID_ATTR_BRIGHT != 0);
        assert!(attr & GRID_ATTR_UNDERSCORE != 0);
    }

    #[test]
    fn test_style_parse_fg_bg() {
        let s = style_parse("fg=red bg=blue");
        assert_eq!(s.gc.fg, 1);
        assert_eq!(s.gc.bg, 4);
    }

    #[test]
    fn test_style_parse_bold() {
        let s = style_parse("bold");
        assert!(s.gc.attr & GRID_ATTR_BRIGHT != 0);
    }
}
