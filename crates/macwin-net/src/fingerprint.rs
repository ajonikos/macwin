//! SHA-256 certificate fingerprints, the unit of trust between two peers.

use std::fmt;
use std::str::FromStr;

use crate::NetError;

/// SHA-256 of a DER-encoded certificate.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint(pub [u8; 32]);

impl Fingerprint {
    pub fn of_cert(der: &[u8]) -> Fingerprint {
        let digest = ring::digest::digest(&ring::digest::SHA256, der);
        let mut out = [0u8; 32];
        out.copy_from_slice(digest.as_ref());
        Fingerprint(out)
    }
}

/// 64 lowercase hex digits.
impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({self})")
    }
}

/// Accepts exactly 64 hex digits, case-insensitive, optionally separated by
/// `:` (as many tools print fingerprints).
impl FromStr for Fingerprint {
    type Err = NetError;

    fn from_str(s: &str) -> Result<Fingerprint, NetError> {
        let invalid = || NetError::InvalidFingerprint(format!("expected 64 hex digits, got {s:?}"));
        let digits: Vec<u8> = s.bytes().filter(|&b| b != b':').collect();
        if digits.len() != 64 {
            return Err(invalid());
        }
        let mut out = [0u8; 32];
        for (i, pair) in digits.chunks_exact(2).enumerate() {
            let hi = hex_value(pair[0]).ok_or_else(invalid)?;
            let lo = hex_value(pair[1]).ok_or_else(invalid)?;
            out[i] = (hi << 4) | lo;
        }
        Ok(Fingerprint(out))
    }
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_digest() {
        // SHA-256 of the empty input.
        assert_eq!(
            Fingerprint::of_cert(b"").to_string(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn parse_and_format_round_trip() {
        let fp = Fingerprint::of_cert(b"macwin");
        let hex = fp.to_string();
        assert_eq!(hex.len(), 64);
        assert!(
            hex.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert_eq!(hex.parse::<Fingerprint>().unwrap(), fp);
        assert_eq!(hex.to_uppercase().parse::<Fingerprint>().unwrap(), fp);
        let colons: Vec<String> = hex
            .as_bytes()
            .chunks(2)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect();
        assert_eq!(colons.join(":").parse::<Fingerprint>().unwrap(), fp);
        assert_eq!(format!("{fp:?}"), format!("Fingerprint({hex})"));
    }

    #[test]
    fn rejects_malformed_input() {
        for bad in [
            "",
            "abc",
            &"0".repeat(63),
            &"0".repeat(65),
            &format!("{}g", "0".repeat(63)),
            &format!(" {}", "0".repeat(64)),
        ] {
            assert!(
                matches!(
                    bad.parse::<Fingerprint>(),
                    Err(NetError::InvalidFingerprint(_))
                ),
                "{bad:?}"
            );
        }
    }
}
