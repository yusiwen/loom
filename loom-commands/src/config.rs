use std::path::Path;

/// A parsed config directive.
#[derive(Clone, Debug)]
pub enum ConfigDirective {
    Set {
        flags: Vec<char>,
        option: String,
        value: String,
    },
    SetOption {
        flags: Vec<char>,
        option: String,
        value: String,
    },
    SetWindowOption {
        flags: Vec<char>,
        option: String,
        value: String,
    },
    Bind {
        flags: Vec<char>,
        key: String,
        command: String,
    },
    Unbind {
        flags: Vec<char>,
        key: String,
    },
    Source {
        path: String,
    },
    IfShell {
        condition: String,
        command: String,
        alternate: Option<String>,
    },
    Command {
        cmdline: String,
    },
}

/// Parse a single config line.
fn parse_line(line: &str) -> Option<ConfigDirective> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }

    let (cmd, rest) = match line.find(char::is_whitespace) {
        Some(pos) => (&line[..pos], line[pos..].trim()),
        None => (line, ""),
    };

    match cmd {
        "set" | "set-option" => {
            let (flags, rest) = extract_flags(rest);
            let is_window = flags.contains(&'w');
            let (option, value) = split_option_value(rest)?;
            if is_window {
                let mut wf = flags.clone();
                wf.retain(|&c| c != 'w');
                Some(ConfigDirective::SetWindowOption { flags: wf, option, value })
            } else {
                Some(ConfigDirective::SetOption { flags, option, value })
            }
        }
        "set-window-option" | "setw" => {
            let (flags, rest) = extract_flags(rest);
            let (option, value) = split_option_value(rest)?;
            Some(ConfigDirective::SetWindowOption { flags, option, value })
        }
        "bind-key" | "bind" => {
            let (flags, rest) = extract_flags(rest);
            let parts: Vec<&str> = split_keep_quoted(rest);
            if parts.len() >= 2 {
                Some(ConfigDirective::Bind {
                    flags,
                    key: parts[0].to_string(),
                    command: parts[1..].join(" "),
                })
            } else {
                None
            }
        }
        "unbind-key" | "unbind" => {
            let (flags, rest) = extract_flags(rest);
            if !rest.is_empty() {
                Some(ConfigDirective::Unbind { flags, key: rest.to_string() })
            } else {
                None
            }
        }
        "source-file" | "source" => {
            if !rest.is_empty() {
                Some(ConfigDirective::Source {
                    path: strip_quotes(rest).to_string(),
                })
            } else {
                None
            }
        }
        "if-shell" => {
            let parts: Vec<&str> = split_keep_quoted(rest);
            if parts.len() == 2 {
                Some(ConfigDirective::IfShell {
                    condition: strip_quotes(parts[0]).to_string(),
                    command: strip_quotes(parts[1]).to_string(),
                    alternate: None,
                })
            } else if parts.len() >= 3 {
                Some(ConfigDirective::IfShell {
                    condition: strip_quotes(parts[0]).to_string(),
                    command: strip_quotes(parts[1]).to_string(),
                    alternate: Some(strip_quotes(&parts[2..].join(" ")).to_string()),
                })
            } else {
                None
            }
        }
        _ => {
            // Treat as raw command
            Some(ConfigDirective::Command {
                cmdline: line.to_string(),
            })
        }
    }
}

fn extract_flags(s: &str) -> (Vec<char>, &str) {
    let s = s.trim();
    if s.starts_with('-') {
        let end = s[1..].find(|c: char| c.is_whitespace()).map(|p| p + 1).unwrap_or(s.len());
        let flags: Vec<char> = s[1..end].chars().collect();
        (flags, s[end..].trim())
    } else {
        (vec![], s)
    }
}

fn split_option_value(s: &str) -> Option<(String, String)> {
    let s = s.trim();
    if s.is_empty() { return None; }
    let parts: Vec<&str> = split_keep_quoted(s);
    if parts.len() >= 2 {
        Some((parts[0].to_string(), parts[1..].join(" ")))
    } else if parts.len() == 1 {
        Some((parts[0].to_string(), String::new()))
    } else {
        None
    }
}

/// Split on whitespace but keep quoted strings intact.
fn split_keep_quoted(s: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut start = None;
    let mut in_quote = false;
    let mut quote_char = '"';

    for (i, ch) in s.char_indices() {
        if !in_quote {
            if ch == '"' || ch == '\'' {
                in_quote = true;
                quote_char = ch;
                start = Some(i + 1);
            } else if !ch.is_whitespace() {
                if start.is_none() {
                    start = Some(i);
                }
            } else if let Some(s_start) = start {
                result.push(&s[s_start..i]);
                start = None;
            }
        } else if ch == quote_char {
            in_quote = false;
            if let Some(s_start) = start {
                result.push(&s[s_start..i]);
            }
            start = None;
        }
    }
    if let Some(s_start) = start {
        result.push(&s[s_start..]);
    }
    result
}

fn strip_quotes(s: &str) -> &str {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        &s[1..s.len()-1]
    } else {
        s
    }
}

/// Parse a complete config file content.
pub fn parse_config(input: &str) -> Result<Vec<ConfigDirective>, String> {
    let mut directives = Vec::new();
    for (lineno, line) in input.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        match parse_line(trimmed) {
            Some(dir) => directives.push(dir),
            None => return Err(format!("line {}: parse error", lineno + 1)),
        }
    }
    Ok(directives)
}

/// Load and parse a config file.
pub fn load_config<P: AsRef<Path>>(path: P) -> Result<Vec<ConfigDirective>, String> {
    let content = std::fs::read_to_string(path.as_ref())
        .map_err(|e| format!("cannot read {}: {}", path.as_ref().display(), e))?;
    parse_config(&content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_set() {
        let config = r#"
set -g status-interval 5
set -g default-shell /bin/zsh
set-option -w main-pane-width 80
"#;
        let directives = parse_config(config).unwrap();
        assert_eq!(directives.len(), 3);
        match &directives[0] {
            ConfigDirective::SetOption { flags, option, value } => {
                assert!(flags.contains(&'g'));
                assert_eq!(option, "status-interval");
                assert_eq!(value, "5");
            }
            _ => panic!("expected SetOption"),
        }
    }

    #[test]
    fn test_parse_bind() {
        let config = r#"
bind-key -n C-a send-prefix
bind '"' split-window -h
"#;
        let directives = parse_config(config).unwrap();
        assert_eq!(directives.len(), 2);
        match &directives[0] {
            ConfigDirective::Bind { flags, key, command } => {
                assert!(flags.contains(&'n'));
                assert_eq!(key, "C-a");
                assert_eq!(command, "send-prefix");
            }
            _ => panic!("expected Bind"),
        }
    }

    #[test]
    fn test_parse_source() {
        let config = "source-file ~/.loom/extra.conf";
        let directives = parse_config(config).unwrap();
        assert_eq!(directives.len(), 1);
        match &directives[0] {
            ConfigDirective::Source { path } => {
                assert_eq!(path, "~/.loom/extra.conf");
            }
            _ => panic!("expected Source"),
        }
    }

    #[test]
    fn test_empty_and_comments() {
        let config = "# comment\n\nset -g a b\n";
        let directives = parse_config(config).unwrap();
        assert_eq!(directives.len(), 1);
    }
}
