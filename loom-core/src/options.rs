use std::collections::BTreeMap;

use crate::style::Style;

/// Option scope: from most-specific to most-global.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Global,
    Session,
    Window,
    Pane,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionsTableType {
    String,
    Number,
    Key,
    Colour,
    Flag,
    Choice,
    Command,
}

#[derive(Clone, Debug)]
pub enum OptionsValue {
    String(String),
    Number(i64),
    Style(Style),
    Array(OptionsArray),
    Command(String),
}

impl Default for OptionsValue {
    fn default() -> Self {
        Self::Number(0)
    }
}

#[derive(Clone, Debug, Default)]
pub struct OptionsArray {
    pub items: BTreeMap<u32, OptionsValue>,
}

/// The real defaults table (subset of tmux's options-table.c). Each entry
/// carries its scope and a default value so `show-options -g` and lookups
/// resolve to a concrete value even when never explicitly set.
pub static OPTIONS_TABLE: &[OptionsTableEntry] = &[
    opt_table("status", None, OptionsTableType::Flag, Scope::Window, 1),
    opt_table("status-interval", None, OptionsTableType::Number, Scope::Global, 15),
    opt_table("status-style", None, OptionsTableType::String, Scope::Window, 0),
    opt_table("status-fg", None, OptionsTableType::Number, Scope::Window, 250),
    opt_table("status-bg", None, OptionsTableType::Number, Scope::Window, 236),
    opt_table("status-active-fg", None, OptionsTableType::Number, Scope::Window, 16),
    opt_table("status-active-bg", None, OptionsTableType::Number, Scope::Window, 252),
    opt_table("status-alert-fg", None, OptionsTableType::Number, Scope::Window, 214),
    opt_table("status-alert-bg", None, OptionsTableType::Number, Scope::Window, 236),
    opt_table("history-limit", None, OptionsTableType::Number, Scope::Global, 2000),
    opt_table("default-shell", None, OptionsTableType::String, Scope::Global, 0),
    opt_table("set-titles", None, OptionsTableType::Flag, Scope::Global, 1),
    opt_table("display-time", None, OptionsTableType::Number, Scope::Global, 750),
    opt_table("escape-time", None, OptionsTableType::Number, Scope::Global, 500),
    opt_table("mouse", None, OptionsTableType::Flag, Scope::Global, 1),
    opt_table("base-index", None, OptionsTableType::Number, Scope::Global, 0),
    opt_table("prefix", None, OptionsTableType::String, Scope::Global, 0),
    opt_table("automatic-rename", None, OptionsTableType::Flag, Scope::Window, 1),
    opt_table("pane-active", None, OptionsTableType::Flag, Scope::Pane, 0),
];

const fn opt_table(
    name: &'static str,
    alt: Option<&'static str>,
    type_: OptionsTableType,
    scope: Scope,
    default_num: i64,
) -> OptionsTableEntry {
    OptionsTableEntry {
        name,
        alternative_name: alt,
        type_,
        scope: match scope {
            Scope::Global => 0,
            Scope::Session => 1,
            Scope::Window => 2,
            Scope::Pane => 3,
        },
        flags: 0,
        minimum: 0,
        maximum: u32::MAX,
        choices: None,
        default_str: None,
        default_num,
        text: "",
        unit: None,
    }
}

/// Look up an option by name in the static table.
pub fn find_option(name: &str) -> Option<&'static OptionsTableEntry> {
    OPTIONS_TABLE.iter().find(|e| e.name == name)
}

#[derive(Clone, Debug)]
pub struct OptionsTableEntry {
    pub name: &'static str,
    pub alternative_name: Option<&'static str>,
    pub type_: OptionsTableType,
    pub scope: u8,
    pub flags: u8,
    pub minimum: u32,
    pub maximum: u32,
    pub choices: Option<&'static [&'static str]>,
    pub default_str: Option<&'static str>,
    pub default_num: i64,
    pub text: &'static str,
    pub unit: Option<&'static str>,
}

#[derive(Clone, Debug)]
pub struct OptionsEntry {
    pub name: String,
    pub table_entry: Option<&'static OptionsTableEntry>,
    pub value: OptionsValue,
    pub style: Option<Style>,
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    tree: BTreeMap<String, OptionsEntry>,
    parent: Option<Box<Options>>,
}

impl Options {
    pub fn new() -> Self {
        Self {
            tree: BTreeMap::new(),
            parent: None,
        }
    }

    /// A fresh options container seeded with the real defaults table. Window,
    /// pane and session options start from a parent that holds these defaults
    /// so an unset option still resolves to a real value.
    pub fn with_defaults() -> Self {
        let mut opts = Self::new();
        for entry in OPTIONS_TABLE {
            let value = match entry.type_ {
                OptionsTableType::Number => OptionsValue::Number(entry.default_num),
                OptionsTableType::String | OptionsTableType::Command => {
                    OptionsValue::String(entry.default_str.unwrap_or("").to_string())
                }
                OptionsTableType::Colour => {
                    OptionsValue::Number(entry.default_num)
                }
                OptionsTableType::Flag => {
                    OptionsValue::Number(entry.default_num)
                }
                _ => OptionsValue::Number(entry.default_num),
            };
            let e = OptionsEntry {
                name: entry.name.to_string(),
                table_entry: Some(entry),
                value,
                style: None,
            };
            opts.tree.insert(entry.name.to_string(), e);
        }
        opts
    }

    /// A child options container (window over session over global defaults)
    /// that inherits the given parent. Values set on the child shadow the
    /// parent; unset values resolve through the parent chain.
    pub fn child_of(parent: Options) -> Self {
        Self::with_parent(parent)
    }

    pub fn with_parent(parent: Options) -> Self {
        Self {
            tree: BTreeMap::new(),
            parent: Some(Box::new(parent)),
        }
    }

    pub fn get(&self, name: &str) -> Option<&OptionsEntry> {
        self.tree.get(name).or_else(|| {
            self.parent
                .as_ref()
                .and_then(|p| p.get(name))
        })
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut OptionsEntry> {
        if self.tree.contains_key(name) {
            self.tree.get_mut(name)
        } else {
            self.parent.as_mut().and_then(|p| p.get_mut(name))
        }
    }

    pub fn set_string(&mut self, name: &str, value: &str) -> &mut OptionsEntry {
        let entry = OptionsEntry {
            name: name.to_string(),
            table_entry: None,
            value: OptionsValue::String(value.to_string()),
            style: None,
        };
        self.tree.insert(name.to_string(), entry);
        self.tree.get_mut(name).unwrap()
    }

    pub fn set_number(&mut self, name: &str, value: i64) -> &mut OptionsEntry {
        let entry = OptionsEntry {
            name: name.to_string(),
            table_entry: None,
            value: OptionsValue::Number(value),
            style: None,
        };
        self.tree.insert(name.to_string(), entry);
        self.tree.get_mut(name).unwrap()
    }

    pub fn get_number(&self, name: &str) -> i64 {
        self.get(name)
            .and_then(|e| match &e.value {
                OptionsValue::Number(n) => Some(*n),
                _ => None,
            })
            .unwrap_or(0)
    }

    pub fn get_string(&self, name: &str) -> Option<&str> {
        self.get(name).and_then(|e| match &e.value {
            OptionsValue::String(s) => Some(s.as_str()),
            _ => None,
        })
    }

    /// A flag option resolves to 0 (off) or 1 (on).
    pub fn get_flag(&self, name: &str) -> bool {
        self.get_number(name) != 0
    }

    /// Parse and set a raw string value according to the option's declared
    /// type (numbers via `set_number`, everything else as a string).
    pub fn set_value(&mut self, name: &str, raw: &str) -> &mut OptionsEntry {
        let table_entry = find_option(name);
        let is_num = matches!(
            table_entry.map(|e| e.type_),
            Some(OptionsTableType::Number)
                | Some(OptionsTableType::Colour)
                | Some(OptionsTableType::Flag)
                | None // unknown options default to number, like tmux
        );
        let entry = if is_num {
            let n: i64 = raw.trim().parse().unwrap_or(0);
            OptionsEntry {
                name: name.to_string(),
                table_entry,
                value: OptionsValue::Number(n),
                style: None,
            }
        } else {
            OptionsEntry {
                name: name.to_string(),
                table_entry,
                value: OptionsValue::String(raw.to_string()),
                style: None,
            }
        };
        self.tree.insert(name.to_string(), entry);
        self.tree.get_mut(name).unwrap()
    }

    pub fn remove(&mut self, name: &str) -> bool {
        self.tree.remove(name).is_some()
    }

    pub fn iter(&self) -> impl Iterator<Item = &OptionsEntry> {
        self.tree.values()
    }

    pub fn set_parent(&mut self, parent: Options) {
        self.parent = Some(Box::new(parent));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_get() {
        let mut opts = Options::new();
        opts.set_number("status-interval", 15);
        assert_eq!(opts.get_number("status-interval"), 15);
    }

    #[test]
    fn test_string_option() {
        let mut opts = Options::new();
        opts.set_string("default-shell", "/bin/zsh");
        assert_eq!(opts.get_string("default-shell"), Some("/bin/zsh"));
    }

    #[test]
    fn test_parent_lookup() {
        let mut parent = Options::new();
        parent.set_number("status-interval", 5);
        let mut child = Options::with_parent(parent);
        assert_eq!(child.get_number("status-interval"), 5);
        child.set_number("status-interval", 10);
        assert_eq!(child.get_number("status-interval"), 10);
    }

    #[test]
    fn test_defaults_table_resolves() {
        let opts = Options::with_defaults();
        assert_eq!(opts.get_number("status-interval"), 15);
        assert_eq!(opts.get_number("history-limit"), 2000);
        assert!(opts.get_flag("set-titles"));
        assert!(!opts.get_flag("pane-active"));
    }

    #[test]
    fn test_defaults_table_present() {
        // Every entry in the table is referenced by name.
        assert!(find_option("status-bg").is_some());
        assert!(find_option("default-shell").is_some());
        assert_eq!(find_option("status-interval").unwrap().default_num, 15);
    }

    #[test]
    fn test_set_value_types() {
        let mut opts = Options::with_defaults();
        opts.set_value("status-interval", "7");
        assert_eq!(opts.get_number("status-interval"), 7);
        opts.set_value("default-shell", "/bin/zsh");
        assert_eq!(opts.get_string("default-shell"), Some("/bin/zsh"));
    }
}
