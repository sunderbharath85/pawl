//! Hand-rolled RFC 4648 base64: standard alphabet with padding (`encode`/`decode`) and the URL-safe
//! alphabet without padding (`encode_url`/`decode_url`). Decoding is strict and canonical: no
//! whitespace, no mixed alphabets, exact padding, and the unused low bits of the last symbol must be
//! zero, so every byte string has exactly one accepted encoding.

use std::fmt;

const STD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum B64Error {
    /// A byte outside the alphabet (or `=` where padding is not allowed).
    InvalidByte { index: usize },
    /// The length cannot be produced by the encoder.
    InvalidLength,
    /// Wrong or misplaced `=` padding.
    InvalidPadding,
    /// Non-zero unused bits in the last symbol (a non-canonical encoding).
    TrailingBits,
}

impl fmt::Display for B64Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            B64Error::InvalidByte { index } => write!(f, "invalid base64 byte at index {index}"),
            B64Error::InvalidLength => f.write_str("invalid base64 length"),
            B64Error::InvalidPadding => f.write_str("invalid base64 padding"),
            B64Error::TrailingBits => f.write_str("non-canonical base64 (trailing bits)"),
        }
    }
}

impl std::error::Error for B64Error {}

/// Standard alphabet, `=` padded.
pub fn encode(b: &[u8]) -> String {
    encode_with(b, STD, true)
}

/// Standard alphabet, `=` padding required.
pub fn decode(s: &str) -> Result<Vec<u8>, B64Error> {
    decode_with(s.as_bytes(), STD, true)
}

/// URL-safe alphabet (`-`, `_`), no padding.
pub fn encode_url(b: &[u8]) -> String {
    encode_with(b, URL, false)
}

/// URL-safe alphabet, padding rejected.
pub fn decode_url(s: &str) -> Result<Vec<u8>, B64Error> {
    decode_with(s.as_bytes(), URL, false)
}

fn encode_with(b: &[u8], alphabet: &[u8; 64], pad: bool) -> String {
    let mut out = Vec::with_capacity(b.len().div_ceil(3) * 4);
    let mut chunks = b.chunks_exact(3);
    for c in &mut chunks {
        let n = (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
        out.push(alphabet[(n >> 18) as usize & 63]);
        out.push(alphabet[(n >> 12) as usize & 63]);
        out.push(alphabet[(n >> 6) as usize & 63]);
        out.push(alphabet[n as usize & 63]);
    }
    match *chunks.remainder() {
        [a] => {
            let n = u32::from(a) << 16;
            out.push(alphabet[(n >> 18) as usize & 63]);
            out.push(alphabet[(n >> 12) as usize & 63]);
            if pad {
                out.extend_from_slice(b"==");
            }
        }
        [a, b2] => {
            let n = (u32::from(a) << 16) | (u32::from(b2) << 8);
            out.push(alphabet[(n >> 18) as usize & 63]);
            out.push(alphabet[(n >> 12) as usize & 63]);
            out.push(alphabet[(n >> 6) as usize & 63]);
            if pad {
                out.push(b'=');
            }
        }
        _ => {}
    }
    // Every byte pushed comes from the ASCII alphabet or is '='.
    String::from_utf8(out).expect("base64 output is ASCII")
}

fn sextet(alphabet: &[u8; 64], c: u8) -> Option<u8> {
    // Only two alphabets exist and they differ in the last two symbols; a lookup by match keeps
    // this branch-light and free of tables built at runtime.
    let v = match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        _ if c == alphabet[62] => 62,
        _ if c == alphabet[63] => 63,
        _ => return None,
    };
    Some(v)
}

fn decode_with(s: &[u8], alphabet: &[u8; 64], padded: bool) -> Result<Vec<u8>, B64Error> {
    let body: &[u8] = if padded {
        if !s.len().is_multiple_of(4) {
            return Err(B64Error::InvalidLength);
        }
        let pad = s.iter().rev().take_while(|&&c| c == b'=').count();
        if pad > 2 {
            return Err(B64Error::InvalidPadding);
        }
        let body = &s[..s.len() - pad];
        // '=' anywhere else is an error.
        if let Some(i) = body.iter().position(|&c| c == b'=') {
            return Err(B64Error::InvalidByte { index: i });
        }
        body
    } else {
        if let Some(i) = s.iter().position(|&c| c == b'=') {
            return Err(B64Error::InvalidByte { index: i });
        }
        s
    };
    if body.len() % 4 == 1 {
        return Err(B64Error::InvalidLength);
    }
    let mut out = Vec::with_capacity(body.len() / 4 * 3 + 2);
    let mut chunks = body.chunks_exact(4);
    let mut index = 0usize;
    let val = |c: u8, i: usize| sextet(alphabet, c).ok_or(B64Error::InvalidByte { index: i });
    for c in &mut chunks {
        let n = (u32::from(val(c[0], index)?) << 18)
            | (u32::from(val(c[1], index + 1)?) << 12)
            | (u32::from(val(c[2], index + 2)?) << 6)
            | u32::from(val(c[3], index + 3)?);
        out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8, n as u8]);
        index += 4;
    }
    match *chunks.remainder() {
        [a, b] => {
            let (a, b) = (val(a, index)?, val(b, index + 1)?);
            if b & 0x0f != 0 {
                return Err(B64Error::TrailingBits);
            }
            out.push((a << 2) | (b >> 4));
        }
        [a, b, c] => {
            let (a, b, c) = (val(a, index)?, val(b, index + 1)?, val(c, index + 2)?);
            if c & 0x03 != 0 {
                return Err(B64Error::TrailingBits);
            }
            out.push((a << 2) | (b >> 4));
            out.push((b << 4) | (c >> 2));
        }
        _ => {}
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 4648 section 10 test vectors.
    const VECTORS: &[(&str, &str)] = &[
        ("", ""),
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ];

    #[test]
    fn rfc4648_standard() {
        for (plain, enc) in VECTORS {
            assert_eq!(encode(plain.as_bytes()), *enc);
            assert_eq!(decode(enc).unwrap(), plain.as_bytes());
        }
    }

    #[test]
    fn rfc4648_url_unpadded() {
        for (plain, enc) in VECTORS {
            let unpadded = enc.trim_end_matches('=');
            assert_eq!(encode_url(plain.as_bytes()), unpadded);
            assert_eq!(decode_url(unpadded).unwrap(), plain.as_bytes());
        }
    }

    #[test]
    fn alphabets_differ_in_62_63() {
        let b = [0xfb, 0xff, 0xbf];
        assert_eq!(encode(&b), "+/+/");
        assert_eq!(encode_url(&b), "-_-_");
        assert_eq!(decode("+/+/").unwrap(), b);
        assert_eq!(decode_url("-_-_").unwrap(), b);
        assert!(decode("-_-_").is_err());
        assert!(decode_url("+/+/").is_err());
    }

    #[test]
    fn round_trip_all_lengths() {
        let data: Vec<u8> = (0..=255u8).collect();
        for n in 0..data.len() {
            let d = &data[..n];
            assert_eq!(decode(&encode(d)).unwrap(), d);
            assert_eq!(decode_url(&encode_url(d)).unwrap(), d);
        }
    }

    #[test]
    fn rejects_malformed() {
        assert_eq!(decode("Zg="), Err(B64Error::InvalidLength));
        assert_eq!(decode("Zg"), Err(B64Error::InvalidLength));
        assert_eq!(decode("Z==="), Err(B64Error::InvalidPadding));
        assert_eq!(decode("Zg=a"), Err(B64Error::InvalidByte { index: 2 }));
        assert_eq!(decode("Zh=="), Err(B64Error::TrailingBits));
        assert_eq!(decode("Zm9="), Err(B64Error::TrailingBits));
        assert_eq!(decode("Zm9v\n"), Err(B64Error::InvalidLength));
        assert_eq!(decode("Zm 9"), Err(B64Error::InvalidByte { index: 2 }));
        assert_eq!(decode_url("Zg=="), Err(B64Error::InvalidByte { index: 2 }));
        assert_eq!(decode_url("Z"), Err(B64Error::InvalidLength));
        assert_eq!(decode_url("Zh"), Err(B64Error::TrailingBits));
        assert_eq!(decode_url("Zm9vY"), Err(B64Error::InvalidLength));
    }
}
