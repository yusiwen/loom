use core::fmt;

pub const UTF8_SIZE: usize = 32;

#[derive(Clone, Copy)]
pub struct Utf8Data {
    pub data: [u8; UTF8_SIZE],
    pub have: u8,
    pub size: u8,
    pub width: u8,
}

impl Utf8Data {
    pub fn new(ch: char) -> Self {
        let mut data = [0u8; UTF8_SIZE];
        let encoded = ch.encode_utf8(&mut data);
        let size = encoded.len() as u8;
        let width = unicode_width(ch) as u8;
        Self {
            data,
            have: size,
            size,
            width,
        }
    }

    pub fn space() -> Self {
        let mut data = [0u8; UTF8_SIZE];
        data[0] = b' ';
        Self {
            data,
            have: 1,
            size: 1,
            width: 1,
        }
    }

    pub fn is_space(&self) -> bool {
        self.size == 1 && self.data[0] == b' ' && self.have == 1
    }

    pub fn to_char(&self) -> char {
        if self.size == 0 {
            return ' ';
        }
        let len = self.size as usize;
        core::str::from_utf8(&self.data[..len])
            .ok()
            .and_then(|s| s.chars().next())
            .unwrap_or(' ')
    }
}

impl Default for Utf8Data {
    fn default() -> Self {
        Self::space()
    }
}

impl fmt::Debug for Utf8Data {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_char())
    }
}

pub enum Utf8State {
    More,
    Done,
    Error,
}

pub fn utf8_open(data: u8) -> Utf8State {
    if data & 0x80 == 0 {
        Utf8State::Done
    } else if data & 0x40 == 0 {
        Utf8State::Error
    } else if data & 0x20 == 0 {
        Utf8State::More
    } else if data & 0x10 == 0 {
        Utf8State::More
    } else if data & 0x08 == 0 {
        Utf8State::More
    } else {
        Utf8State::Error
    }
}

pub fn utf8_append(data: &mut Utf8Data, ch: u8) -> Utf8State {
    if data.size as usize >= UTF8_SIZE {
        return Utf8State::Error;
    }
    data.data[data.size as usize] = ch;
    data.size += 1;
    data.have += 1;

    if data.size >= 4 {
        let len = data.size as usize;
        return match core::str::from_utf8(&data.data[..len]) {
            Ok(s) => {
                if let Some(c) = s.chars().next() {
                    data.width = unicode_width(c) as u8;
                }
                Utf8State::Done
            }
            Err(_) => Utf8State::Error,
        };
    }
    Utf8State::More
}

pub fn utf8_from_char(c: char) -> Utf8Data {
    Utf8Data::new(c)
}

pub fn utf8_strtou(s: &str) -> u32 {
    let mut n: u32 = 0;
    for &b in s.as_bytes() {
        if b < b'0' || b > b'9' {
            break;
        }
        n = n * 10 + (b - b'0') as u32;
    }
    n
}

pub fn utf8_is_leading(data: u8) -> bool {
    data & 0xc0 != 0x80
}

pub fn utf8_combine(data: &Utf8Data) -> char {
    let len = data.size as usize;
    core::str::from_utf8(&data.data[..len])
        .ok()
        .and_then(|s| s.chars().next())
        .unwrap_or(' ')
}

fn unicode_width(c: char) -> usize {
    unicode_width_inner(c)
}

fn unicode_width_inner(c: char) -> usize {
    if c.is_ascii() {
        1
    } else {
        let code = c as u32;
        if (0x1100..=0x115F).contains(&code)
            || code == 0x2329
            || code == 0x232A
            || (0x2E80..=0x303E).contains(&code)
            || (0x3040..=0xA4CF).contains(&code)
            || (0xA960..=0xA97F).contains(&code)
            || (0xAC00..=0xD7A3).contains(&code)
            || (0xD7B0..=0xD7FF).contains(&code)
            || (0xFE10..=0xFE19).contains(&code)
            || (0xFE30..=0xFE6F).contains(&code)
            || (0xFF01..=0xFF60).contains(&code)
            || (0xFFE0..=0xFFE6).contains(&code)
            || (0x1B000..=0x1B0FF).contains(&code)
            || (0x1B100..=0x1B12F).contains(&code)
            || (0x1F004..=0x1F9CF).contains(&code)
            || (0x20000..=0x2FFFD).contains(&code)
            || (0x30000..=0x3FFFD).contains(&code)
        {
            2
        } else {
            1
        }
    }
}

pub fn wide_emoji_width(c: char) -> usize {
    // Common wide emoji ranges
    let code = c as u32;
    matches!(
        code,
        0x261D | 0x26F9 | 0x270A..=0x270D
            | 0x1F1E6..=0x1F1FF
            | 0x1F300..=0x1F64F
            | 0x1F680..=0x1F6FF
            | 0x1F900..=0x1F9FF
    )
    .then_some(2)
    .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ascii() {
        let d = Utf8Data::new('A');
        assert_eq!(d.size, 1);
        assert_eq!(d.width, 1);
        assert_eq!(d.to_char(), 'A');
    }

    #[test]
    fn test_utf8_multi() {
        let d = Utf8Data::new('€');
        assert_eq!(d.size, 3);
        assert_eq!(d.to_char(), '€');
    }

    #[test]
    fn test_space() {
        let d = Utf8Data::space();
        assert!(d.is_space());
    }

    #[test]
    fn test_cjk_width() {
        let d = Utf8Data::new('中');
        assert_eq!(d.width, 2);
    }
}
