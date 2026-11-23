//! Standard RFC 4648 padded Base64 encoding and strict decoding.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Encodes `input` as standard RFC 4648 Base64 with `=` padding.
pub fn encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= input.len() {
        let n =
            (u32::from(input[i]) << 16) | (u32::from(input[i + 1]) << 8) | u32::from(input[i + 2]);
        out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
        out.push(ALPHABET[((n >> 6) & 0x3F) as usize] as char);
        out.push(ALPHABET[(n & 0x3F) as usize] as char);
        i += 3;
    }
    let rem = input.len() - i;
    if rem == 1 {
        let n = u32::from(input[i]) << 16;
        out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
        out.push('=');
        out.push('=');
    } else if rem == 2 {
        let n = (u32::from(input[i]) << 16) | (u32::from(input[i + 1]) << 8);
        out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
        out.push(ALPHABET[((n >> 6) & 0x3F) as usize] as char);
        out.push('=');
    }
    out
}

fn decode_val(b: u8) -> Result<u8, String> {
    match b {
        b'A'..=b'Z' => Ok(b - b'A'),
        b'a'..=b'z' => Ok(b - b'a' + 26),
        b'0'..=b'9' => Ok(b - b'0' + 52),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => Err(format!("invalid base64 byte 0x{b:02x}")),
    }
}

/// Strictly decodes standard RFC 4648 padded Base64, rejecting non-canonical padding or bad chars.
pub fn decode(input: &str) -> Result<Vec<u8>, String> {
    let bytes = input.as_bytes();
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    if !bytes.len().is_multiple_of(4) {
        return Err("base64 length must be a multiple of 4".into());
    }
    let mut out = Vec::with_capacity((bytes.len() / 4) * 3);
    let n_chunks = bytes.len() / 4;
    for (idx, chunk) in bytes.chunks_exact(4).enumerate() {
        let is_last = idx + 1 == n_chunks;
        let a = decode_val(chunk[0])?;
        let b = decode_val(chunk[1])?;
        match (chunk[2], chunk[3]) {
            (b'=', b'=') => {
                if !is_last {
                    return Err("base64 padding before final block".into());
                }
                if (b & 0x0F) != 0 {
                    return Err("non-zero base64 padding bits".into());
                }
                out.push((a << 2) | (b >> 4));
            }
            (c_byte, b'=') => {
                if !is_last {
                    return Err("base64 padding before final block".into());
                }
                let c = decode_val(c_byte)?;
                if (c & 0x03) != 0 {
                    return Err("non-zero base64 padding bits".into());
                }
                out.push((a << 2) | (b >> 4));
                out.push((b << 4) | (c >> 2));
            }
            (b'=', _) => {
                return Err("invalid base64 padding".into());
            }
            (c_byte, d_byte) => {
                let c = decode_val(c_byte)?;
                let d = decode_val(d_byte)?;
                out.push((a << 2) | (b >> 4));
                out.push((b << 4) | (c >> 2));
                out.push((c << 6) | d);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc4648_section_10_vectors() {
        let vectors = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (plain, b64) in vectors {
            assert_eq!(encode(plain.as_bytes()), b64);
            assert_eq!(decode(b64).unwrap(), plain.as_bytes());
        }
    }

    #[test]
    fn binary_round_trips_and_rejects_malformed() {
        let h32: [u8; 32] = std::array::from_fn(|i| (i as u8).wrapping_mul(17).wrapping_add(3));
        let enc32 = encode(&h32);
        assert_eq!(enc32.len(), 44);
        assert_eq!(decode(&enc32).unwrap().as_slice(), &h32);

        let sig68: [u8; 68] = std::array::from_fn(|i| (i as u8).wrapping_mul(31));
        let enc68 = encode(&sig68);
        assert_eq!(decode(&enc68).unwrap().as_slice(), &sig68);

        assert!(decode("Zg").is_err());
        assert!(decode("Z===").is_err());
        assert!(decode("Zg=A").is_err());
        assert!(decode("Zg==AAAA").is_err());
        // Non-zero trailing padding bit: 'h' instead of 'g' in "Zg==".
        assert!(decode("Zh==").is_err());
        assert!(decode("Zm9=").is_err());
        assert!(decode("Zm!v").is_err());
    }
}
