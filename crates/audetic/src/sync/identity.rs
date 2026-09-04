use base64::{engine::general_purpose::STANDARD, Engine};
use http::HeaderValue;
use thiserror::Error;

/// The identity header added by Tailscale Serve for an untagged caller.
pub const TAILSCALE_USER_LOGIN_HEADER: &str = "tailscale-user-login";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LoginParseError {
    #[error("Tailscale login is not a valid HTTP header value")]
    InvalidHeader,
    #[error("Tailscale login contains malformed RFC 2047 encoding")]
    MalformedEncodedWord,
}

/// Extracts a Tailscale login exactly as it must be persisted and compared.
///
/// Tailscale leaves ASCII identities unchanged and RFC 2047 Q-encodes
/// identities containing non-ASCII text. No whitespace, case, or Unicode
/// normalization is applied here.
pub fn parse_tailscale_login(value: &HeaderValue) -> Result<String, LoginParseError> {
    let raw = value.to_str().map_err(|_| LoginParseError::InvalidHeader)?;
    if !raw.contains("=?") && !raw.contains("?=") {
        return Ok(raw.to_owned());
    }

    validate_encoded_words(raw)?;
    rfc2047_decoder::decode(raw.as_bytes()).map_err(|_| LoginParseError::MalformedEncodedWord)
}

/// Runs persisted setup input through the same HTTP-header and RFC 2047 path
/// used for request identity headers.
pub fn parse_stored_tailscale_login(raw: &str) -> Result<String, LoginParseError> {
    let value = HeaderValue::from_str(raw).map_err(|_| LoginParseError::InvalidHeader)?;
    parse_tailscale_login(&value)
}

fn validate_encoded_words(raw: &str) -> Result<(), LoginParseError> {
    let bytes = raw.as_bytes();
    let mut cursor = 0;
    let mut found = false;

    while let Some(relative_start) = raw[cursor..].find("=?") {
        let start = cursor + relative_start;
        if start > 0 && !is_word_boundary(bytes[start - 1]) {
            return Err(LoginParseError::MalformedEncodedWord);
        }

        let charset_end = raw[start + 2..]
            .find('?')
            .map(|position| start + 2 + position)
            .ok_or(LoginParseError::MalformedEncodedWord)?;
        let encoding_end = raw[charset_end + 1..]
            .find('?')
            .map(|position| charset_end + 1 + position)
            .ok_or(LoginParseError::MalformedEncodedWord)?;
        let word_end = raw[encoding_end + 1..]
            .find("?=")
            .map(|position| encoding_end + 1 + position)
            .ok_or(LoginParseError::MalformedEncodedWord)?;

        let charset = &raw[start + 2..charset_end];
        let encoding = &raw[charset_end + 1..encoding_end];
        let encoded_text = &raw[encoding_end + 1..word_end];
        let end = word_end + 2;

        if charset.is_empty()
            || !charset.bytes().all(is_rfc_token_byte)
            || encoded_text.is_empty()
            || end - start > 75
            || (end < bytes.len() && !is_word_boundary(bytes[end]))
        {
            return Err(LoginParseError::MalformedEncodedWord);
        }

        match encoding {
            "q" | "Q" => validate_q_encoding(encoded_text)?,
            "b" | "B" => {
                STANDARD
                    .decode(encoded_text)
                    .map_err(|_| LoginParseError::MalformedEncodedWord)?;
            }
            _ => return Err(LoginParseError::MalformedEncodedWord),
        }

        found = true;
        cursor = end;
    }

    if !found || raw[cursor..].contains("?=") {
        return Err(LoginParseError::MalformedEncodedWord);
    }
    Ok(())
}

fn validate_q_encoding(input: &str) -> Result<(), LoginParseError> {
    let bytes = input.as_bytes();
    let mut cursor = 0;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'=' => {
                if cursor + 2 >= bytes.len()
                    || !bytes[cursor + 1].is_ascii_hexdigit()
                    || !bytes[cursor + 2].is_ascii_hexdigit()
                {
                    return Err(LoginParseError::MalformedEncodedWord);
                }
                cursor += 3;
            }
            byte if (33..=126).contains(&byte) && byte != b'?' => cursor += 1,
            _ => return Err(LoginParseError::MalformedEncodedWord),
        }
    }
    Ok(())
}

fn is_rfc_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`{|}~".contains(&byte)
}

fn is_word_boundary(byte: u8) -> bool {
    byte.is_ascii_whitespace() || b"\"()<> ,".contains(&byte)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_login_is_preserved_byte_for_byte() {
        let value = HeaderValue::from_static(" Alice@Example.com ");

        assert_eq!(
            parse_tailscale_login(&value).unwrap(),
            " Alice@Example.com "
        );
    }

    #[test]
    fn tailscale_q_encoded_login_is_decoded() {
        let value = HeaderValue::from_static("=?utf-8?q?m=C3=A1t@example.com?=");

        assert_eq!(parse_tailscale_login(&value).unwrap(), "mát@example.com");
    }

    #[test]
    fn valid_base64_encoded_word_is_decoded() {
        let value = HeaderValue::from_static("=?UTF-8?B?bUBleGFtcGxlLmNvbQ==?=");

        assert_eq!(parse_tailscale_login(&value).unwrap(), "m@example.com");
    }

    #[test]
    fn malformed_encoded_words_fail_closed() {
        for raw in [
            "=?utf-8?q?alice=ZZ@example.com?=",
            "=?utf-8?x?alice@example.com?=",
            "=?utf-8?b?not base64?=",
            "=?utf-8?q?unterminated",
            "stray?=",
        ] {
            let value = HeaderValue::from_str(raw).unwrap();
            assert_eq!(
                parse_tailscale_login(&value),
                Err(LoginParseError::MalformedEncodedWord),
                "{raw}"
            );
        }
    }

    #[test]
    fn stored_owner_login_uses_the_identical_parser() {
        assert_eq!(
            parse_stored_tailscale_login("=?utf-8?q?m=C3=A1t@example.com?=").unwrap(),
            "mát@example.com"
        );
        assert_eq!(
            parse_stored_tailscale_login("=?utf-8?q?broken=Q0?="),
            Err(LoginParseError::MalformedEncodedWord)
        );
    }
}
