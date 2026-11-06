//! Minimal standard base64 encoder (UVTT embeds the map picture as base64 PNG).

const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Decode standard base64 (padding optional, ASCII whitespace ignored).
/// `None` for any other byte or a malformed length.
pub fn decode(text: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some(u32::from(c - b'A')),
            b'a'..=b'z' => Some(u32::from(c - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(c - b'0') + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    };
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    let (mut data_chars, mut pad) = (0usize, 0usize);
    for c in text.bytes() {
        if c.is_ascii_whitespace() {
            continue;
        }
        if c == b'=' {
            // Padding only closes a final group that already has 2 or 3 data chars.
            if pad == 0 && !matches!(data_chars % 4, 2 | 3) {
                return None;
            }
            pad += 1;
            continue;
        }
        if pad > 0 {
            return None; // data after padding
        }
        acc = (acc << 6) | val(c)?;
        data_chars += 1;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    if pad > 0 && data_chars % 4 + pad != 4 {
        return None;
    }
    // Leftover bits must be zero padding of a valid length (2 or 3 chars of a quad).
    if bits >= 6 || acc != 0 {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::{decode, encode};

    #[test]
    fn decode_round_trips_and_refuses_junk() {
        for n in 0..70usize {
            let data: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(decode(&encode(&data)).unwrap(), data, "len {n}");
        }
        assert_eq!(
            decode("Zm9v\nYmFy").unwrap(),
            b"foobar",
            "whitespace is ignored"
        );
        assert_eq!(decode("Zg").unwrap(), b"f", "padding is optional");
        for bad in ["Zm9v!", "Z", "Zm9vY", "====", "Zg==Zg", "Zh=="] {
            assert!(decode(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn rfc4648_vectors() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foob"), "Zm9vYg==");
        assert_eq!(encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
    }
}
