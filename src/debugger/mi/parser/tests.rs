use super::*;

fn scalar_string(parser: &mut Parser<'_>) -> Result<String, String> {
    parser.expect(b'"')?;
    let mut bytes = Vec::new();

    loop {
        match parser.next() {
            Some(b'"') => break,
            Some(b'\\') => parser.escape(&mut bytes)?,
            Some(byte) => bytes.push(byte),
            None => return Err(String::from("unterminated MI string")),
        }
    }

    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn compare(input: &str) {
    let mut parser = Parser::new(input);
    let mut scalar = Parser::new(input);
    let result = parser.c_string();
    assert_eq!(result, scalar_string(&mut scalar), "{input:?}");

    if result.is_ok() {
        assert_eq!(parser.position, scalar.position);
    }
}

#[test]
fn simd_string_scanning_preserves_escapes_unicode_and_malformed_input() {
    for prefix in 0..80 {
        for suffix in [
            "\"",
            "\\n\"",
            "\\\\\"",
            "\\\"λ界\"",
            "\\377\"",
            "\\xFF\"",
            "\\x\"",
            "\\",
            "",
            "\\x100000000\"",
        ] {
            compare(&format!("\"{}{suffix}", "a".repeat(prefix)));
        }

        let contents = format!("{}λ界\r\n\t\\\"", "abc".repeat(prefix));
        let encoded = quote(&contents);
        assert_eq!(Parser::new(&encoded).c_string().unwrap(), contents);
        compare(&encoded);
    }

    for input in ["", "not a string", "\"\"trailing", "\"λ界\"suffix"] {
        compare(input);
    }

    let large = quote(&"λabc\\\n".repeat(16 * 1024));
    compare(&large);
}
