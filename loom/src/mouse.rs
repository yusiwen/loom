//! Client-side SGR mouse decoding (Phase B, B5).
//!
//! When mouse reporting is enabled the terminal emits SGR mouse events on
//! stdin as `ESC [ < b ; x ; y M|m`, where `b` is the button (plus modifier
//! bits), `x`/`y` are 1-based terminal coordinates, and the final byte is `M`
//! (press/motion) or `m` (release). This module accumulates a byte stream and
//! extracts complete mouse events, forwarding everything else along unchanged
//! so ordinary keystrokes are unaffected.

/// An SGR mouse event decoded into a structured form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    /// Button code, including modifier bits (b from the SGR sequence).
    pub button: u32,
    /// 1-based column.
    pub sx: u32,
    /// 1-based row.
    pub sy: u32,
    /// True when the sequence ended with `m` (a release).
    pub release: bool,
}

/// A stateful decoder that turns a raw input byte stream into a mix of
/// forward (non-mouse) bytes and mouse events. It buffers a trailing partial
/// sequence so a mouse report split across two reads still decodes correctly.
pub struct MouseDecoder {
    buf: Vec<u8>,
}

impl Default for MouseDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl MouseDecoder {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Feed a chunk of input bytes. Returns the bytes that are *not* mouse
    /// events (to be forwarded to the pane) and the decoded mouse events.
    pub fn feed(&mut self, bytes: &[u8]) -> (Vec<u8>, Vec<MouseEvent>) {
        self.buf.extend_from_slice(bytes);
        let mut out: Vec<u8> = Vec::new();
        let mut events: Vec<MouseEvent> = Vec::new();
        let mut i = 0;
        while i < self.buf.len() {
            if self.buf[i..].starts_with(b"\x1b[<") {
                // Complete or partial mouse sequence.
                match parse_mouse(&self.buf[i..]) {
                    Some((ev, consumed)) => {
                        events.push(ev);
                        i += consumed;
                    }
                    None => {
                        // No complete sequence yet; hold the rest of the
                        // buffer (it may be a partial report) and stop.
                        break;
                    }
                }
            } else {
                out.push(self.buf[i]);
                i += 1;
            }
        }
        // Remove everything processed; keep the trailing partial mouse byte(s).
        self.buf.drain(..i);
        (out, events)
    }

    /// True when the decoder has buffered an incomplete sequence.
    /// Exposed for tests.
    #[cfg(test)]
    pub fn has_partial(&self) -> bool {
        !self.buf.is_empty()
    }
}

/// Parse a complete SGR mouse sequence at the start of `buf`.
/// Returns the event and the number of bytes consumed, or `None` when `buf`
/// does not contain a complete sequence at position 0.
fn parse_mouse(buf: &[u8]) -> Option<(MouseEvent, usize)> {
    if buf.len() < 6 || buf[0] != 0x1b || buf[1] != b'[' || buf[2] != b'<' {
        return None;
    }
    let mut i = 3;
    let mut nums = [0u32; 3];
    for k in 0..3 {
        let start = i;
        while i < buf.len() && buf[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return None; // missing digits
        }
        nums[k] = std::str::from_utf8(&buf[start..i]).ok()?.parse().ok()?;
        if k < 2 {
            if i >= buf.len() || buf[i] != b';' {
                return None;
            }
            i += 1;
        }
    }
    if i >= buf.len() {
        return None; // no terminating byte yet
    }
    let release = match buf[i] {
        b'M' => false,
        b'm' => true,
        _ => return None,
    };
    let event = MouseEvent {
        button: nums[0],
        sx: nums[1],
        sy: nums[2],
        release,
    };
    Some((event, i + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_left_click() {
        let mut d = MouseDecoder::new();
        // ESC [ < 0 ; 10 ; 5 M  => left button at col 10, row 5, press.
        let (fwd, ev) = d.feed(b"abc\x1b[<0;10;5Mdef");
        assert_eq!(fwd, b"abcdef");
        assert_eq!(
            ev,
            vec![MouseEvent { button: 0, sx: 10, sy: 5, release: false }]
        );
    }

    #[test]
    fn decodes_wheel_and_release() {
        let mut d = MouseDecoder::new();
        // Wheel down (65) press, then left (0) release.
        let (fwd, ev) = d.feed(b"\x1b[<65;3;2M\x1b[<0;3;4m");
        assert_eq!(fwd, Vec::new());
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].button, 65);
        assert!(!ev[0].release);
        assert_eq!(ev[1].button, 0);
        assert!(ev[1].release);
    }

    #[test]
    fn buffers_partial_across_reads() {
        let mut d = MouseDecoder::new();
        let (fwd, ev) = d.feed(b"\x1b[<0;1");
        assert_eq!(fwd, Vec::new());
        assert!(ev.is_empty());
        assert!(d.has_partial());
        let (fwd, ev) = d.feed(b"0;5M");
        assert_eq!(fwd, Vec::new());
        assert_eq!(ev, vec![MouseEvent { button: 0, sx: 10, sy: 5, release: false }]);
        assert!(!d.has_partial());
    }

    #[test]
    fn ordinary_bytes_pass_through() {
        let mut d = MouseDecoder::new();
        let (fwd, ev) = d.feed(b"ls\r\n");
        assert_eq!(fwd, b"ls\r\n");
        assert!(ev.is_empty());
    }
}
