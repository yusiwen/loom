use std::collections::BTreeMap;

use crate::style::Style;

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
}
