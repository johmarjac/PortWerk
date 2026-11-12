//! Turning typed text into bytes and bytes back into readable text,
//! plus the checksum helpers used by the tools tab.

/// Parse a hex string such as "41 42 0d0a" or "0x41,0x42" into bytes.
pub fn parse_hex(input: &str) -> Result<Vec<u8>, String> {
    let mut digits = String::new();
    let cleaned = input.replace("0x", " ").replace("0X", " ");
    for c in cleaned.chars() {
        if c.is_ascii_hexdigit() {
            digits.push(c);
        } else if c.is_whitespace() || c == ',' || c == ';' || c == ':' || c == '-' {
            // separators: an odd digit before a separator stands alone ("A B" = 0A 0B)
            if digits.len() % 2 == 1 {
                let last = digits.pop().unwrap();
                digits.push('0');
                digits.push(last);
            }
        } else {
            return Err(format!("Ungültiges Hex-Zeichen: '{c}'"));
        }
    }
    if digits.len() % 2 == 1 {
        let last = digits.pop().unwrap();
        digits.push('0');
        digits.push(last);
    }
    (0..digits.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&digits[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

/// Parse text with escapes into bytes.
///
/// * `$XX`  – one byte given as two hex digits (e.g. `$0D`)
/// * `$$`   – a literal dollar sign
/// * `#DDD` – one byte given as up to three decimal digits (e.g. `#013`)
/// * `##`   – a literal hash sign
/// * `\r \n \t \0 \\` – the usual C escapes
pub fn parse_text(input: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '$' => {
                if chars.get(i + 1) == Some(&'$') {
                    out.push(b'$');
                    i += 2;
                    continue;
                }
                let h: String = chars.iter().skip(i + 1).take(2).collect();
                if h.len() == 2 && h.chars().all(|c| c.is_ascii_hexdigit()) {
                    out.push(u8::from_str_radix(&h, 16).unwrap());
                    i += 3;
                } else {
                    return Err(format!("Nach '$' werden zwei Hex-Ziffern erwartet (Stelle {})", i + 1));
                }
            }
            '#' => {
                if chars.get(i + 1) == Some(&'#') {
                    out.push(b'#');
                    i += 2;
                    continue;
                }
                let d: String = chars
                    .iter()
                    .skip(i + 1)
                    .take(3)
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if d.is_empty() {
                    return Err(format!("Nach '#' wird eine Dezimalzahl erwartet (Stelle {})", i + 1));
                }
                let v: u32 = d.parse().unwrap();
                if v > 255 {
                    return Err(format!("#{d} ist größer als 255"));
                }
                out.push(v as u8);
                i += 1 + d.len();
            }
            '\\' => {
                let e = chars.get(i + 1).copied();
                let b = match e {
                    Some('r') => Some(b'\r'),
                    Some('n') => Some(b'\n'),
                    Some('t') => Some(b'\t'),
                    Some('0') => Some(0),
                    Some('\\') => Some(b'\\'),
                    _ => None,
                };
                if let Some(b) = b {
                    out.push(b);
                    i += 2;
                } else {
                    out.push(b'\\');
                    i += 1;
                }
            }
            _ => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                i += 1;
            }
        }
    }
    Ok(out)
}

pub fn to_hex(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len() * 3);
    for (i, b) in data.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&format!("{b:02X}"));
    }
    s
}

/// Readable text. Control characters become `{0D}`-style markers when
/// `show_special` is set; CR/LF/TAB are otherwise passed through.
pub fn to_text(data: &[u8], show_special: bool) -> String {
    let mut s = String::with_capacity(data.len());
    for &b in data {
        match b {
            b'\n' if !show_special => s.push('\n'),
            b'\r' if !show_special => {}
            b'\t' if !show_special => s.push('\t'),
            0x20..=0x7E => s.push(b as char),
            0xA0..=0xFF if !show_special => s.push(char::from_u32(b as u32).unwrap_or('?')),
            _ => {
                if show_special {
                    s.push_str(&format!("{{{b:02X}}}"));
                    if b == b'\n' {
                        s.push('\n');
                    }
                } else {
                    s.push('·');
                }
            }
        }
    }
    s
}

pub fn to_dec(data: &[u8]) -> String {
    data.iter().map(|b| b.to_string()).collect::<Vec<_>>().join(" ")
}

pub fn to_bin(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:08b}")).collect::<Vec<_>>().join(" ")
}

// ---------- checksums ----------

pub fn sum8(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |a, b| a.wrapping_add(*b))
}

pub fn xor8(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |a, b| a ^ b)
}

pub fn crc8(data: &[u8]) -> u8 {
    // CRC-8 (poly 0x07, init 0x00)
    let mut crc = 0u8;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 { (crc << 1) ^ 0x07 } else { crc << 1 };
        }
    }
    crc
}

pub fn crc16_modbus(data: &[u8]) -> u16 {
    let mut crc = 0xFFFFu16;
    for &b in data {
        crc ^= b as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xA001 } else { crc >> 1 };
        }
    }
    crc
}

pub fn crc16_ccitt(data: &[u8]) -> u16 {
    // CRC-16/CCITT-FALSE (poly 0x1021, init 0xFFFF)
    let mut crc = 0xFFFFu16;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_escapes() {
        assert_eq!(parse_text("A$0D$0A").unwrap(), b"A\r\n");
        assert_eq!(parse_text("$$#065##").unwrap(), b"$A#");
        assert_eq!(parse_text("x\\r\\n").unwrap(), b"x\r\n");
        assert!(parse_text("$G1").is_err());
    }

    #[test]
    fn hex_parse() {
        assert_eq!(parse_hex("41 42 0d0a").unwrap(), b"AB\r\n");
        assert_eq!(parse_hex("0x1,0x2").unwrap(), vec![1, 2]);
    }

    #[test]
    fn checksums() {
        assert_eq!(crc16_modbus(b"123456789"), 0x4B37);
        assert_eq!(crc16_ccitt(b"123456789"), 0x29B1);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc8(b"123456789"), 0xF4);
    }
}
