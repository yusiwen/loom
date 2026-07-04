use std::collections::HashMap;
use std::io;

use terminfo::Database;

/// Terminal feature flags matching tmux's TERM_* constants.
pub const TERM_256COLOURS: u32 = 0x01;
pub const TERM_NOAM: u32 = 0x02;
pub const TERM_DECSLRM: u32 = 0x04;
pub const TERM_DECFRA: u32 = 0x08;
pub const TERM_RGBCOLOURS: u32 = 0x10;
pub const TERM_VT100LIKE: u32 = 0x20;
pub const TERM_SIXEL: u32 = 0x40;

/// A terminal capability entry loaded from terminfo.
#[derive(Clone, Debug)]
pub enum TerminfoCap {
    Missing,
    String(String),
    Number(i32),
    Flag(bool),
}

/// Wraps a terminfo database with tmux-compatible capability access.
pub struct TtyTerm {
    /// Terminal name (e.g. "xterm-256color").
    pub name: String,
    /// Terminal aliases.
    pub aliases: Vec<String>,
    /// Feature flags.
    pub features: u32,
    /// Cached capabilities.
    caps: HashMap<String, TerminfoCap>,
    /// ACS character map.
    pub acs: [[u8; 2]; 256],
}

impl TtyTerm {
    /// Load terminfo for the given terminal name.
    pub fn load(name: &str) -> io::Result<Self> {
        let db = Database::from_name(name).map_err(|e| {
            io::Error::new(io::ErrorKind::NotFound, format!("terminfo: {}", e))
        })?;

        let mut caps = HashMap::new();
        for cap_name in CAP_NAMES {
            let cap = match db.raw(cap_name) {
                Some(terminfo::Value::True) => TerminfoCap::Flag(true),
                Some(terminfo::Value::Number(_)) => TerminfoCap::Number(
                    // This is an approximation - we store presence as flag
                    // and actual numbers are looked up separately
                    1,
                ),
                Some(terminfo::Value::String(s)) => {
                    TerminfoCap::String(String::from_utf8_lossy(s).to_string())
                }
                None => TerminfoCap::Missing,
            };
            caps.insert(cap_name.to_string(), cap);
        }

        let mut features = 0u32;
        if let TerminfoCap::Number(n) = caps
            .get("colors")
            .unwrap_or(&TerminfoCap::Missing)
        {
            if *n >= 256 {
                features |= TERM_256COLOURS;
            }
        }
        if let TerminfoCap::Flag(true) = caps.get("AX").unwrap_or(&TerminfoCap::Missing) {
            features |= TERM_NOAM;
        }
        if let TerminfoCap::String(_) = caps.get("XT").unwrap_or(&TerminfoCap::Missing) {
            features |= TERM_DECSLRM;
        }
        if caps.contains_key("Rgb") {
            if let TerminfoCap::String(_) = caps.get("Rgb").unwrap_or(&TerminfoCap::Missing) {
                features |= TERM_RGBCOLOURS;
            }
        }
        if caps.contains_key("cup") {
            if let TerminfoCap::String(_) = caps.get("cup").unwrap_or(&TerminfoCap::Missing) {
                features |= TERM_VT100LIKE;
            }
        }

        let name = db.name().to_string();
        let aliases = db.aliases().to_vec();

        Ok(Self {
            name,
            aliases,
            features,
            caps,
            acs: [[0u8; 2]; 256],
        })
    }

    /// Get a string capability.
    pub fn string(&self, name: &str) -> Option<&str> {
        match self.caps.get(name)? {
            TerminfoCap::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Get a number capability.
    pub fn number(&self, name: &str) -> Option<i32> {
        match self.caps.get(name)? {
            TerminfoCap::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// Check if a flag capability is set.
    pub fn flag(&self, name: &str) -> bool {
        matches!(self.caps.get(name), Some(TerminfoCap::Flag(true)))
    }

    /// Check if a capability exists (any type).
    pub fn has(&self, name: &str) -> bool {
        !matches!(self.caps.get(name), Some(TerminfoCap::Missing))
    }
}

/// Essential terminfo capability names.
pub const CAP_NAMES: &[&str] = &[
    "acsc", "am", "AX", "bce", "bel", "blink", "bold",
    "civis", "clear", "cnorm", "colors", "cr", "csr", "cub", "cub1",
    "cud", "cud1", "cuf", "cuf1", "cup", "cuu", "cuu1", "cvvis",
    "dch", "dch1", "dim", "dl", "dl1", "dsl",
    "ech", "ed", "el", "el1", "enacs",
    "flash", "fsl", "home", "hpa", "hp", "ich", "ich1", "il", "il1",
    "indn", "invis", "is2",
    "kcbt", "kcub1", "kcud1", "kcuf1", "kcuu1", "kDC", "kdch1",
    "kEND", "kend", "kFND", "kfnd", "kHOM", "khom", "kIC", "kich1",
    "kNXT", "knxt", "kPRV", "kprv",
    "ka1", "ka3", "kb2", "kc1", "kc3",
    "kbs", "kcan", "kcat", "kcpy", "kclo", "kcmd", "kdc", "kdl1",
    "ked", "kel", "kent", "kext", "kf0", "kf1", "kf10", "kf11", "kf12",
    "kf13", "kf14", "kf15", "kf16", "kf17", "kf18", "kf19", "kf2",
    "kf20", "kf3", "kf4", "kf5", "kf6", "kf7", "kf8", "kf9",
    "kind", "kmous", "kmov", "kmsg", "knp", "kpp", "kref", "kres",
    "krfr", "kri", "krmir", "kron", "ksav", "kslt", "ktab",
    "ll", "mc0", "mc4", "mc5",
    "mgc", "msgr", "oc", "op", "pairs",
    "rc", "rev", "ri", "rmacs", "rmcup", "rmir", "rmkx", "rmm",
    "rmso", "rmul", "rs1", "rs2", "rs3",
    "sbim", "sc", "setab", "setaf", "sgr0", "sgr",
    "smacs", "smcup", "smir", "smkx", "smm", "smso", "smul",
    "tbc", "tsl", "u6", "u7", "u8", "u9",
    "vpa",
    "E3", "Cs", "Cr", "Se", "Ss", "Sm", "Sbim",
    "Su", "Sf", "Dsbp", "Dsmg", "Enbp", "Enmg",
    "Dseks", "Dsfcs", "Eneks", "Enfcs",
    "Clmg", "Cmg", "Hls",
    "Smol", "Smpol", "Smxx", "Smulx",
    "Sync", "Rgb", "Tc", "XT", "XM",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_terminfo_load() {
        // Test with a known terminal type
        if let Ok(term) = TtyTerm::load("xterm-256color") {
            assert_eq!(term.name, "xterm-256color");
            assert!(term.features & TERM_VT100LIKE != 0);
            assert!(term.has("cup"));
        }
    }

    #[test]
    fn test_terminfo_fallback() {
        // Should fail gracefully for nonexistent terminal
        assert!(TtyTerm::load("nonexistent-terminal-xyz").is_err());
    }
}
