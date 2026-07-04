use nom::{
    branch::alt,
    bytes::complete::{is_not, tag, take_while1},
    character::complete::{char, multispace0, one_of},
    combinator::{map, opt},
    sequence::{delimited, preceded},
    IResult,
};

use crate::cmd::Args;

/// Parse a command string like "new-session -s mysess -d" or
/// 'new-window -n "my window"'.
pub fn parse_command_line(input: &str) -> Result<(&str, Args), String> {
    let input = input.trim();
    let (rest, name) = parse_ident(input).map_err(|e| format!("parse error: {}", e))?;
    let (_rest, args) = parse_args(rest.trim())
        .map_err(|e| format!("arg parse error: {}", e))?;
    Ok((name, args))
}

fn parse_ident(input: &str) -> IResult<&str, &str> {
    take_while1(|c: char| c.is_alphanumeric() || c == '-')(input)
}

fn parse_args(input: &str) -> IResult<&str, Args> {
    let (input, _) = multispace0(input)?;
    let mut args = Args::default();
    let mut rest = input;

    loop {
        let (r, _) = multispace0(rest)?;
        if r.is_empty() {
            break;
        }
        // Check for flag like -s or -svalue
        if let Ok((r2, flag_char)) = map::<_, _, _, nom::error::Error<&str>, _, _>(
            preceded(char('-'), one_of("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ")),
            |c| c,
        )(r)
        {
            // Check if next char is a value (non-flag)
            let (r3, val) = opt(alt((
                delimited(char('"'), is_not("\""), char('"')),
                delimited(char('\''), is_not("'"), char('\'')),
                take_while1(|c: char| !c.is_whitespace() && c != '-'),
            )))(r2.trim_start())?;

            if let Some(v) = val {
                args.flags.insert(flag_char, v.to_string());
                rest = r3;
            } else {
                args.flags.insert(flag_char, String::new());
                rest = r2;
            }
        } else if let Ok((r2, _)) = tag::<_, _, nom::error::Error<&str>>("--")(r) {
            // Everything after -- is positional
            let (r3, pos) = is_not("")(r2.trim_start())?;
            for p in pos.split_whitespace() {
                args.positional.push(p.to_string());
            }
            rest = r3;
            break;
        } else {
            // Positional argument
            let (r2, val) = alt((
                delimited(char('"'), is_not("\""), char('"')),
                delimited(char('\''), is_not("'"), char('\'')),
                take_while1(|c: char| !c.is_whitespace()),
            ))(r)?;
            args.positional.push(val.to_string());
            rest = r2;
        }
        rest = rest.trim_start();
    }

    Ok((rest, args))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple() {
        let (name, args) = parse_command_line("new-session -s mysess -d").unwrap();
        assert_eq!(name, "new-session");
        assert_eq!(args.get('s'), Some("mysess"));
        assert!(args.has('d'));
    }

    #[test]
    fn test_parse_positional() {
        let (name, args) = parse_command_line("new-window vim README.md").unwrap();
        assert_eq!(name, "new-window");
        assert_eq!(args.positional.len(), 2);
        assert_eq!(args.positional[0], "vim");
        assert_eq!(args.positional[1], "README.md");
    }

    #[test]
    fn test_parse_quoted() {
        let (name, args) = parse_command_line("send-keys -t 1 \"hello world\" Enter").unwrap();
        assert_eq!(name, "send-keys");
        assert_eq!(args.get('t'), Some("1"));
        assert_eq!(args.positional[0], "hello world");
        assert_eq!(args.positional[1], "Enter");
    }
}
