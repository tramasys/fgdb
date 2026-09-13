//! Contiguous lowercase hexadecimal for debugger payloads and binary identifiers.

pub(crate) fn encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));

    for &byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }

    encoded
}

#[cfg(test)]
mod tests {
    #[test]
    fn encodes_every_byte_with_two_lowercase_digits() {
        assert_eq!(super::encode(&[]), "");
        let bytes = (0..=u8::MAX).collect::<Vec<_>>();
        let encoded = super::encode(&bytes);
        assert_eq!(encoded.len(), 512);
        assert!(
            encoded
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        );

        for (index, byte) in bytes.into_iter().enumerate() {
            assert_eq!(
                u8::from_str_radix(&encoded[index * 2..index * 2 + 2], 16),
                Ok(byte)
            );
        }
    }
}
