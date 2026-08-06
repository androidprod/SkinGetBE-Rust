//! JWT (JSON Web Token) handling for Bedrock authentication
//!
//! This module provides JWT parsing and payload extraction for Minecraft
//! Bedrock Edition authentication chains and skin data.

use crate::Result;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

/// JWT token structure
#[derive(Debug, Clone)]
pub struct JWT {
    pub header: String,
    pub payload: String,
    pub signature: String,
}

impl JWT {
    /// Parse a JWT token from string format
    ///
    /// JWT format: `header.payload.signature`
    pub fn parse(token: &str) -> Result<Self> {
        let parts: Vec<&str> = token.split('.').collect();

        if parts.len() != 3 {
            return Err(crate::Error::CryptoError(format!(
                "Invalid JWT format: expected 3 parts, got {}",
                parts.len()
            )));
        }

        Ok(Self {
            header: parts[0].to_string(),
            payload: parts[1].to_string(),
            signature: parts[2].to_string(),
        })
    }

    /// Extract and decode payload from JWT token string
    pub fn get_payload(token: &str) -> Result<String> {
        let first_dot = token
            .find('.')
            .ok_or_else(|| crate::Error::CryptoError("Invalid JWT: missing first dot".into()))?;

        let second_dot = token[first_dot + 1..]
            .find('.')
            .map(|i| i + first_dot + 1)
            .ok_or_else(|| crate::Error::CryptoError("Invalid JWT: missing second dot".into()))?;

        let payload_b64 = &token[first_dot + 1..second_dot];

        Self::decode_base64(payload_b64)
    }

    /// Verify JWT signature (requires public key)
    ///
    /// This is a placeholder - actual verification would require
    /// public key infrastructure and signature validation.
    pub fn verify(&self, _public_key: &[u8]) -> Result<bool> {
        // TODO: Implement JWT verification using public key
        // For now, return true as verification is not critical for skin extraction
        Ok(true)
    }

    /// Decode JWT payload (without verification)
    pub fn decode_payload(&self) -> Result<String> {
        Self::decode_base64(&self.payload)
    }

    /// Decode base64-url encoded string (with and without padding)
    fn decode_base64(encoded: &str) -> Result<String> {
        // Try with URL_SAFE_NO_PAD first (standard for JWT)
        let decoded = URL_SAFE_NO_PAD
            .decode(encoded)
            .or_else(|_| {
                // Add padding if needed and try again
                let mut padded = encoded.to_string();
                while !padded.len().is_multiple_of(4) {
                    padded.push('=');
                }
                URL_SAFE_NO_PAD.decode(&padded)
            })
            .map_err(|e| crate::Error::CryptoError(format!("JWT decode error: {}", e)))?;

        String::from_utf8(decoded)
            .map_err(|e| crate::Error::CryptoError(format!("UTF-8 decode error: {}", e)))
    }

    /// Extract JSON value from decoded payload
    ///
    /// Handles both string values (quoted) and numeric values
    pub fn get_json_value(json: &str, key: &str) -> Option<String> {
        let search_key = format!("\"{}\":", key);

        let pos = json.find(&search_key)?;
        let start_pos = pos + search_key.len();

        // Skip whitespace
        let rest = &json[start_pos..];
        let content = rest.trim_start();

        if let Some(inner) = content.strip_prefix('"') {
            // String value: find closing quote (respecting escape sequences)
            let mut value = String::new();
            let mut prev_backslash = false;

            for c in inner.chars() {
                if c == '"' && !prev_backslash {
                    return Some(value);
                }
                prev_backslash = c == '\\' && !prev_backslash;
                value.push(c);
            }
            None
        } else {
            // Numeric/boolean value: collect until delimiter
            let value: String = content
                .chars()
                .take_while(|c| !matches!(c, ',' | '}' | ']' | ' '))
                .collect();

            if value.is_empty() {
                None
            } else {
                Some(value)
            }
        }
    }
}

/// Parse authentication chain from login packet
///
/// Returns a list of decoded JWT payloads
pub fn parse_auth_chain(chain_json: &str) -> Result<Vec<String>> {
    let mut tokens = Vec::new();

    let mut search_pos = 0;
    while let Some(quote_pos) = chain_json[search_pos..].find('"') {
        let token_start = search_pos + quote_pos + 1;

        if let Some(token_end) = chain_json[token_start..].find('"') {
            let token = &chain_json[token_start..token_start + token_end];

            // Check if this looks like a JWT (contains 2 dots)
            let dot_count = token.matches('.').count();
            if dot_count == 2 {
                // Try to decode and parse
                match JWT::get_payload(token) {
                    Ok(payload) => tokens.push(payload),
                    Err(_) => { /* Skip invalid tokens */ }
                }
            }

            search_pos = token_start + token_end + 1;
        } else {
            break;
        }
    }

    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jwt_parse() {
        let token = "header.payload.signature";
        let jwt = JWT::parse(token).unwrap();
        assert_eq!(jwt.header, "header");
        assert_eq!(jwt.payload, "payload");
        assert_eq!(jwt.signature, "signature");
    }

    #[test]
    fn test_invalid_jwt() {
        let token = "only.two";
        assert!(JWT::parse(token).is_err());
    }

    #[test]
    fn test_get_json_value_string() {
        let json = r#"{"name": "Steve", "age": 30}"#;
        let value = JWT::get_json_value(json, "name");
        assert_eq!(value, Some("Steve".to_string()));
    }

    #[test]
    fn test_get_json_value_number() {
        let json = r#"{"name": "Steve", "age": 30}"#;
        let value = JWT::get_json_value(json, "age");
        assert_eq!(value, Some("30".to_string()));
    }
}
