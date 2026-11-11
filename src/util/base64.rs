//! Base64 in its two alphabets: standard with padding (RFC 4648 section 4), and url-safe with
//! no padding (section 5), as a JWT needs it.

const STANDARD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL_SAFE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// The standard form with `=` padding, such as `eDp5` for `x:y`.
pub fn encode(bytes: &[u8]) -> String {
    encode_with(bytes, STANDARD, true)
}

/// The url-safe form with no padding.
pub fn encode_url(bytes: &[u8]) -> String {
    encode_with(bytes, URL_SAFE, false)
}

fn encode_with(bytes: &[u8], alphabet: &[u8; 64], pad: bool) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let digits = chunk.len() + 1;
        for i in 0..4 {
            if i < digits {
                out.push(char::from(alphabet[((n >> (18 - 6 * i)) & 63) as usize]));
            } else if pad {
                out.push('=');
            }
        }
    }
    out
}

/// Reads the standard or the url-safe form. White space is skipped, and padding is optional.
/// `None` for any other character, or for a length that no input gives.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let mut values = Vec::with_capacity(text.len());
    let mut padding = 0;
    for c in text.bytes() {
        if c.is_ascii_whitespace() {
            continue;
        }
        if c == b'=' {
            padding += 1;
            continue;
        }
        if padding > 0 {
            return None;
        }
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        values.push(value);
    }
    if values.len() % 4 == 1 || padding > 2 {
        return None;
    }
    let mut out = Vec::with_capacity(values.len() * 3 / 4);
    for chunk in values.chunks(4) {
        let mut n = 0u32;
        for (i, v) in chunk.iter().enumerate() {
            n |= u32::from(*v) << (18 - 6 * i);
        }
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&bytes[..chunk.len() - 1]);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_length_round_trips_in_both_forms() {
        let cases = [
            ("", "", ""),
            ("f", "Zg==", "Zg"),
            ("fo", "Zm8=", "Zm8"),
            ("foo", "Zm9v", "Zm9v"),
            ("foob", "Zm9vYg==", "Zm9vYg"),
            ("fooba", "Zm9vYmE=", "Zm9vYmE"),
            ("foobar", "Zm9vYmFy", "Zm9vYmFy"),
        ];
        for (plain, standard, url) in cases {
            assert_eq!(encode(plain.as_bytes()), standard);
            assert_eq!(encode_url(plain.as_bytes()), url);
            assert_eq!(decode(standard).unwrap(), plain.as_bytes());
            assert_eq!(decode(url).unwrap(), plain.as_bytes());
        }
        assert_eq!(encode(&[0xfb, 0xff]), "+/8=");
        assert_eq!(encode_url(&[0xfb, 0xff]), "-_8");
        assert_eq!(decode("+/8=").unwrap(), [0xfb, 0xff]);
        assert_eq!(decode("Zm9v\nYmFy\n").unwrap(), b"foobar");
        assert_eq!(decode("Zm9v!"), None);
        assert_eq!(decode("Z"), None);
        assert_eq!(decode("Zg==Zg"), None);
    }
}
