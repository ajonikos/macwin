//! Parser for Chromium's `dom_code_data.inc`.
//!
//! The file is a C table of `DOM_CODE(usb, evdev, xkb, win, mac, code, id)`
//! macro invocations. Entries may span lines; commented-out entries look like
//! `//DOM_CODE(`. Every field is validated; anything unexpected is an error.

use anyhow::{Context, Result, bail, ensure};

/// The macro invocation that introduces one table entry.
const MACRO: &str = "DOM_CODE(";
/// Number of macro arguments: usb, evdev, xkb, win, mac, code, id.
const ARGS: usize = 7;

/// One parsed `DOM_CODE(...)` entry (the evdev and XKB columns are validated
/// but not kept).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomCode {
    /// 1-based line of the `DOM_CODE(` token, for error messages.
    pub line: usize,
    /// USB HID usage: `(usage_page << 16) | usage`.
    pub usb: u32,
    /// Windows scan code (`0xE0xx` if extended); 0 = none.
    pub win: u16,
    /// macOS virtual key code; `0xFFFF` = none.
    pub mac: u16,
    /// DOM `code` string; `None` for `NULL`.
    pub code: Option<String>,
    /// Chromium's `DomCode` enumerator name.
    pub id: String,
}

/// Parses every (uncommented) `DOM_CODE(...)` entry of `src`, in file order.
pub fn parse(src: &str) -> Result<Vec<DomCode>> {
    let text = strip_comments(src)?;
    let mut entries = Vec::new();
    let mut from = 0;
    while let Some(found) = text[from..].find(MACRO) {
        let start = from + found;
        let line = line_of(&text, start);
        let args_start = start + MACRO.len();
        let preceded_by_ident = text[..start]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        if preceded_by_ident {
            // Some other identifier that merely ends in `DOM_CODE`.
            from = args_start;
            continue;
        }
        let (args, len) = split_args(&text[args_start..])
            .with_context(|| format!("line {line}: malformed DOM_CODE entry"))?;
        let entry = parse_entry(line, &args)
            .with_context(|| format!("line {line}: malformed DOM_CODE entry"))?;
        entries.push(entry);
        from = args_start + len;
    }
    Ok(entries)
}

/// Removes `//` comments line by line (string-literal aware) and drops `\r`.
/// Line breaks are kept so byte offsets map back to source lines.
fn strip_comments(src: &str) -> Result<String> {
    let mut out = String::with_capacity(src.len());
    for (index, raw) in src.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let mut in_string = false;
        let mut escaped = false;
        let mut prev_slash = false;
        let mut cut = line.len();
        for (pos, c) in line.char_indices() {
            if in_string {
                match c {
                    _ if escaped => escaped = false,
                    '\\' => escaped = true,
                    '"' => in_string = false,
                    _ => {}
                }
                continue;
            }
            match c {
                '"' => in_string = true,
                '/' if prev_slash => {
                    cut = pos - 1;
                    break;
                }
                _ => {}
            }
            prev_slash = c == '/';
        }
        ensure!(
            !in_string,
            "line {}: unterminated string literal",
            index + 1
        );
        out.push_str(&line[..cut]);
    }
    Ok(out)
}

/// 1-based line number of byte offset `pos` in `text`.
fn line_of(text: &str, pos: usize) -> usize {
    text[..pos].bytes().filter(|&b| b == b'\n').count() + 1
}

/// Splits the macro arguments that follow `DOM_CODE(` at top-level commas, up
/// to the matching `)`. Returns the trimmed arguments and the number of bytes
/// consumed, including the closing parenthesis.
fn split_args(text: &str) -> Result<(Vec<&str>, usize)> {
    let mut args = Vec::new();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut arg_start = 0;
    for (pos, c) in text.char_indices() {
        if in_string {
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '(' => depth += 1,
            ')' if depth > 0 => depth -= 1,
            ')' => {
                args.push(text[arg_start..pos].trim());
                return Ok((args, pos + 1));
            }
            ',' if depth == 0 => {
                args.push(text[arg_start..pos].trim());
                arg_start = pos + 1;
            }
            _ => {}
        }
    }
    bail!("missing closing parenthesis")
}

fn parse_entry(line: usize, args: &[&str]) -> Result<DomCode> {
    ensure!(
        args.len() == ARGS,
        "expected {ARGS} arguments, found {}",
        args.len()
    );
    let usb = parse_hex(args[0]).context("usb")?;
    parse_hex(args[1]).context("evdev")?;
    parse_hex(args[2]).context("xkb")?;
    let win = parse_hex_u16(args[3]).context("win")?;
    let mac = parse_hex_u16(args[4]).context("mac")?;
    let code = parse_code(args[5]).context("code")?;
    let id = args[6];
    ensure!(
        is_identifier(id),
        "id: expected an identifier, found `{id}`"
    );
    Ok(DomCode {
        line,
        usb,
        win,
        mac,
        code,
        id: id.to_owned(),
    })
}

/// Parses `0x` followed by 1 to 8 hex digits (either case).
fn parse_hex(field: &str) -> Result<u32> {
    let digits = field
        .strip_prefix("0x")
        .or_else(|| field.strip_prefix("0X"))
        .filter(|d| (1..=8).contains(&d.len()) && d.bytes().all(|b| b.is_ascii_hexdigit()))
        .with_context(|| format!("expected a hex literal, found `{field}`"))?;
    Ok(u32::from_str_radix(digits, 16)?)
}

fn parse_hex_u16(field: &str) -> Result<u16> {
    let value = parse_hex(field)?;
    u16::try_from(value).with_context(|| format!("`{field}` does not fit in 16 bits"))
}

/// `NULL` or a string literal of ASCII letters, digits and underscores.
fn parse_code(field: &str) -> Result<Option<String>> {
    if field == "NULL" {
        return Ok(None);
    }
    let inner = field
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        .with_context(|| format!("expected NULL or a plain string literal, found `{field}`"))?;
    Ok(Some(inner.to_owned()))
}

fn is_identifier(s: &str) -> bool {
    let mut bytes = s.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FIXTURE;

    fn find(entries: &[DomCode], usb: u32) -> Option<&DomCode> {
        entries.iter().find(|e| e.usb == usb)
    }

    #[test]
    fn parses_fixture_entries_in_order() {
        let entries = parse(FIXTURE).unwrap();
        let usbs: Vec<u32> = entries.iter().map(|e| e.usb).collect();
        assert_eq!(
            usbs,
            [
                0x00_0000, 0x01_0082, 0x07_0001, 0x07_0004, 0x07_0055, 0x07_0074, 0x07_00E3,
                0x0C_00CD, 0x0C_00E2
            ]
        );
        let key_a = find(&entries, 0x07_0004).unwrap();
        assert_eq!(
            *key_a,
            DomCode {
                line: 7,
                usb: 0x07_0004,
                win: 0x001E,
                mac: 0x0000,
                code: Some("KeyA".into()),
                id: "US_A".into()
            }
        );
    }

    #[test]
    fn multi_line_entry() {
        let entries = parse(FIXTURE).unwrap();
        let entry = find(&entries, 0x07_0055).unwrap();
        assert_eq!(entry.line, 8);
        assert_eq!((entry.win, entry.mac), (0x0037, 0x0043));
        assert_eq!(entry.code.as_deref(), Some("NumpadMultiply"));
        assert_eq!(entry.id, "NUMPAD_MULTIPLY");
    }

    #[test]
    fn commented_out_entries_are_skipped() {
        let entries = parse(FIXTURE).unwrap();
        assert!(find(&entries, 0x07_0032).is_none());
        assert!(find(&entries, 0x07_00CA).is_none());
    }

    #[test]
    fn null_code() {
        let entries = parse(FIXTURE).unwrap();
        assert_eq!(find(&entries, 0x00_0000).unwrap().code, None);
        assert_eq!(find(&entries, 0x0C_00E2).unwrap().code, None);
    }

    #[test]
    fn mixed_case_hex() {
        let src = "DOM_CODE(0x0c019C, 0x01b1, 0x01b9, 0xE06D, 0xFFFF, NULL, LOG_OFF),";
        let entry = &parse(src).unwrap()[0];
        assert_eq!(
            (entry.usb, entry.win, entry.mac),
            (0x0C_019C, 0xE06D, 0xFFFF)
        );
    }

    #[test]
    fn tolerates_crlf() {
        let crlf = FIXTURE.replace('\n', "\r\n");
        assert_eq!(parse(&crlf).unwrap(), parse(FIXTURE).unwrap());
    }

    #[test]
    fn comment_markers_inside_strings_are_not_comments() {
        assert_eq!(
            strip_comments(r#"a "//b" c // d"#).unwrap(),
            r#"a "//b" c "#
        );
        assert!(strip_comments("\"open").is_err());
    }

    #[test]
    fn longer_identifiers_are_not_entries() {
        let src = "MY_DOM_CODE(1, 2), DOM_CODE_DECLARATION {";
        assert!(parse(src).unwrap().is_empty());
    }

    #[test]
    fn malformed_entries_fail_with_line_numbers() {
        let cases = [
            (
                "DOM_CODE(0x07zz04, 0x0, 0x0, 0x001e, 0x0000, \"KeyA\", US_A),",
                "usb",
            ),
            (
                "DOM_CODE(70004, 0x0, 0x0, 0x001e, 0x0000, \"KeyA\", US_A),",
                "usb",
            ),
            (
                "DOM_CODE(0x070004, 0x0, 0x0, 0x10000, 0x0000, \"KeyA\", US_A),",
                "win",
            ),
            (
                "DOM_CODE(0x070004, 0x0, 0x0, 0x001e, 0x, \"KeyA\", US_A),",
                "mac",
            ),
            (
                "DOM_CODE(0x070004, 0x0, 0x0, 0x001e, 0x0000, KeyA, US_A),",
                "code",
            ),
            (
                "DOM_CODE(0x070004, 0x0, 0x0, 0x001e, 0x0000, \"Key A\", US_A),",
                "code",
            ),
            (
                "DOM_CODE(0x070004, 0x0, 0x0, 0x001e, 0x0000, \"KeyA\", 1A),",
                "id",
            ),
            (
                "DOM_CODE(0x070004, 0x0, 0x0, 0x001e, 0x0000, \"KeyA\"),",
                "expected 7 arguments",
            ),
            (
                "DOM_CODE(0x070004, 0x0, 0x0, 0x001e, 0x0000, \"KeyA\", US_A",
                "closing parenthesis",
            ),
        ];
        for (body, expected) in cases {
            let src = format!("// header\n\n  {body}\n");
            let err = format!("{:#}", parse(&src).unwrap_err());
            assert!(
                err.starts_with("line 3: malformed DOM_CODE entry"),
                "{body}: {err}"
            );
            assert!(err.contains(expected), "{body}: {err}");
        }
    }
}
