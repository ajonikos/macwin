//! keymap-gen: generates `crates/macwin-core/src/keymap/generated.rs` from
//! Chromium's `dom_code_data.inc` (fetched once by `fetch.sh`, checked in).
//!
//! Usage: `cargo run -p keymap-gen` rewrites the table;
//! `cargo run -p keymap-gen -- --check` exits 1 if the checked-in table is stale.
#![forbid(unsafe_code)]

mod emit;
mod parse;

use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, bail, ensure};

/// Input file, relative to the workspace root.
const INPUT: &str = "tools/keymap-gen/data/dom_code_data.inc";
/// Output file, relative to the workspace root.
const OUTPUT: &str = "crates/macwin-core/src/keymap/generated.rs";
/// Chromium's table has about 250 entries; far fewer means the input or the
/// parser is broken.
const MIN_ENTRIES: usize = 200;
const STALE: &str = "generated keymap is stale; run `cargo run -p keymap-gen`";

enum Mode {
    Write,
    Check,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("keymap-gen: error: {err:#}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<ExitCode> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = match args.as_slice() {
        [] => Mode::Write,
        [flag] if flag == "--check" => Mode::Check,
        _ => bail!("usage: keymap-gen [--check]"),
    };

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let src = fs::read_to_string(root.join(INPUT)).with_context(|| format!("reading {INPUT}"))?;
    let generated = generate(&src).with_context(|| format!("processing {INPUT}"))?;
    let output = root.join(OUTPUT);
    let current = match fs::read_to_string(&output) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == ErrorKind::NotFound => None,
        Err(err) => return Err(err).with_context(|| format!("reading {OUTPUT}")),
    };
    let up_to_date = current.as_deref() == Some(generated.as_str());

    match mode {
        Mode::Check if up_to_date => Ok(ExitCode::SUCCESS),
        Mode::Check => {
            eprintln!("{STALE}");
            Ok(ExitCode::from(1))
        }
        Mode::Write if up_to_date => {
            eprintln!("{OUTPUT} is up to date");
            Ok(ExitCode::SUCCESS)
        }
        Mode::Write => {
            fs::write(&output, &generated).with_context(|| format!("writing {OUTPUT}"))?;
            eprintln!("wrote {OUTPUT}");
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Parses the Chromium table and renders the generated module.
fn generate(src: &str) -> Result<String> {
    let entries = parse::parse(src)?;
    ensure!(
        entries.len() >= MIN_ENTRIES,
        "only {} DOM_CODE entries parsed, expected at least {MIN_ENTRIES}",
        entries.len()
    );
    let keys = emit::select(&entries)?;
    Ok(emit::render(&keys))
}

/// Shared test input in the shape of `dom_code_data.inc`: a multi-line entry,
/// commented-out entries (one of them multi-line), NULL codes, and entries
/// that must be excluded (pages 0x00/0x01, usage < 4, no native code).
#[cfg(test)]
const FIXTURE: &str = r#"// Copyright 2013 The Chromium Authors
DOM_CODE_DECLARATION {
  //            USB     evdev    XKB     Win     Mac   Code
  DOM_CODE(0x000000, 0x0000, 0x0000, 0x0000, 0xffff, NULL, NONE), // Invalid
  DOM_CODE(0x010082, 0x008e, 0x0096, 0xe05f, 0xffff, "Sleep", SLEEP), // SystemSleep
  DOM_CODE(0x070001, 0x0000, 0x0000, 0x00ff, 0xffff, NULL, USB_ERROR_ROLL_OVER),
  DOM_CODE(0x070004, 0x001e, 0x0026, 0x001e, 0x0000, "KeyA", US_A), // aA
  DOM_CODE(0x070055, 0x0037, 0x003f, 0x0037, 0x0043, "NumpadMultiply",
             NUMPAD_MULTIPLY),  // Keypad_*
  //DOM_CODE(0x070032, 0x0000, 0x0000, 0x0000, 0xffff, NULL, INTL_HASH),
  //DOM_CODE(0x0700ca, 0x0000, 0x0000, 0x0000, 0xffff, NULL,
  //           NUMPAD_DOUBLE_VERTICAL_BAR),  // Keypad_||
  DOM_CODE(0x070074, 0x0086, 0x008e, 0x0000, 0xffff, "Open", OPEN), // Execute
  DOM_CODE(0x0700e3, 0x007d, 0x0085, 0xe05b, 0x0037, "MetaLeft", META_LEFT),
  DOM_CODE(0x0c00cd, 0x00a4, 0x00ac, 0xe022, 0xffff, "MediaPlayPause",
             MEDIA_PLAY_PAUSE),
  DOM_CODE(0x0c00e2, 0x0071, 0x0079, 0xe020, 0xffff, NULL, FIXTURE_MUTE),
};
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn too_few_entries_is_an_error() {
        let err = generate(FIXTURE).unwrap_err().to_string();
        assert_eq!(err, "only 9 DOM_CODE entries parsed, expected at least 200");
    }

    #[test]
    fn enough_entries_generate() {
        // Pad the fixture with page-0x0D entries (parsed and counted, never kept).
        let mut src = FIXTURE.to_owned();
        for usage in 0..MIN_ENTRIES {
            src.push_str(&format!(
                "DOM_CODE(0x0d{usage:04x}, 0x0, 0x0, 0x0000, 0xffff, NULL, PAD),\n"
            ));
        }
        let out = generate(&src).unwrap();
        assert!(out.contains("pub(crate) static KEYS: [KeyEntry; 5] = ["));
    }
}
