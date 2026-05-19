//! Bedrock login packet handling and player authentication

use crate::{util::Buffer, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::collections::HashSet;
use tracing::debug;

/// Login packet data structure
#[derive(Debug, Clone)]
pub struct LoginData {
    pub protocol: u32,
    pub username: String,
    pub xuid: String,
    pub uuid: String,
    pub device_os: i32,
    pub device_model: String,
}

impl LoginData {
    /// Parse login packet from raw buffer
    ///
    /// # Arguments
    /// * `data` - Raw packet data
    /// * `protocol` - Server protocol version from config
    pub fn from_buffer(data: &[u8], protocol: u32) -> Result<Self> {
        if data.len() < 8 {
            return Err(crate::Error::InvalidData("Login packet too short".into()));
        }

        // Skip protocol version parsing for now
        // Full implementation would parse JWT chains and extract client data
        let username = format!("Player_{}", rand::random::<u32>() % 10000);

        Ok(LoginData {
            protocol, // Use config protocol version
            username,
            xuid: String::new(),
            uuid: String::new(),
            device_os: 0,
            device_model: "Unknown".to_string(),
        })
    }
}

/// Parsed Bedrock login capture data
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct LoginMetadata {
    pub xuid: Option<String>,
    pub device_os: Option<i32>,
    pub device_model: Option<String>,
    pub playfab_id: Option<String>,
    pub client_random_id: Option<String>,
    pub skin_geometry: Option<String>,
    pub geometry_data_engine_version: Option<String>,
    pub animation_data: Option<String>,
    pub cape_data: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ParsedLoginPacket {
    pub protocol: u32,
    pub player_name: String,
    pub skin_id: String,
    pub chain_json: String,
    pub skin_jwt: String,
    pub skin_json: String,
    pub skin_data: Vec<u8>,
    pub skin_width: u32,
    pub skin_height: u32,
    pub metadata: LoginMetadata,
}

#[derive(Debug, Clone, Copy)]
enum ProtocolEncoding {
    U32Le,
    U32Be,
    VarInt,
}

/// Parse a Bedrock login packet using the same variant probing as the C++ version.
pub fn parse_login_packet(data: &[u8]) -> Result<Option<ParsedLoginPacket>> {
    #[derive(Debug)]
    struct LoginFields {
        protocol: u32,
        chain_json: String,
        skin_jwt: String,
        chain_len: usize,
        skin_len: usize,
        layout: &'static str,
    }

    fn try_parse_login(
        source: &[u8],
        protocol_encoding: ProtocolEncoding,
        has_payload_length: bool,
        layout: &'static str,
    ) -> Option<LoginFields> {
        let mut tmp = Buffer::from(source);

        // Bedrock Login packet layout after packet id:
        // protocol version, optional payload length, then u32 LE token chain length + JSON,
        // then u32 LE skin data length + bytes.
        let protocol = match protocol_encoding {
            ProtocolEncoding::U32Le => tmp.read_u32_le().ok()?,
            ProtocolEncoding::U32Be => tmp.read_u32().ok()?,
            ProtocolEncoding::VarInt => tmp.read_var_int().ok()?,
        };
        if protocol == 0 || protocol > 2000 {
            return None;
        }

        if has_payload_length {
            let payload_len = tmp.read_var_int().ok()? as usize;
            if payload_len == 0 || payload_len > tmp.remaining() {
                return None;
            }
        }

        let chain_len = tmp.read_u32_le().ok()? as usize;
        if chain_len == 0 || tmp.remaining() < chain_len {
            return None;
        }

        let chain_json = String::from_utf8(tmp.read_bytes(chain_len).ok()?).ok()?;
        let skin_len = tmp.read_u32_le().ok()? as usize;
        if skin_len == 0 || tmp.remaining() < skin_len {
            return None;
        }

        let skin_jwt = String::from_utf8(tmp.read_bytes(skin_len).ok()?).ok()?;
        if !chain_json.contains('{') {
            return None;
        }
        if skin_jwt.matches('.').count() != 2 {
            return None;
        }

        Some(LoginFields {
            protocol,
            chain_json,
            skin_jwt,
            chain_len,
            skin_len,
            layout,
        })
    }

    fn decode_loose_base64(input: &str) -> Option<Vec<u8>> {
        let cleaned: String = input
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
            .collect();

        if cleaned.is_empty() {
            return None;
        }

        let mut padded = cleaned;
        while padded.len() % 4 != 0 {
            padded.push('=');
        }

        STANDARD.decode(padded.as_bytes()).ok()
    }

    fn sanitize_component(value: &str) -> String {
        value
            .chars()
            .map(|c| match c {
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
                _ => c,
            })
            .collect()
    }

    fn extract_string(payloads: &[String], keys: &[&str]) -> Option<String> {
        for payload in payloads {
            for key in keys {
                if let Some(value) = crate::crypto::jwt::JWT::get_json_value(payload, key) {
                    if !value.is_empty() {
                        return Some(sanitize_component(&value));
                    }
                }
            }
        }
        None
    }

    fn extract_i32(payloads: &[String], keys: &[&str]) -> Option<i32> {
        for payload in payloads {
            for key in keys {
                if let Some(value) = crate::crypto::jwt::JWT::get_json_value(payload, key) {
                    if let Ok(parsed) = value.parse::<i32>() {
                        return Some(parsed);
                    }
                }
            }
        }
        None
    }

    fn extract_player_name(chain_json: &str) -> String {
        let mut search_pos = 0;
        let mut seen = HashSet::new();

        while let Some(start_quote) = chain_json[search_pos..].find('"') {
            let token_start = search_pos + start_quote + 1;
            let Some(end_quote_rel) = chain_json[token_start..].find('"') else {
                break;
            };
            let token_end = token_start + end_quote_rel;
            search_pos = token_end + 1;

            let token = &chain_json[token_start..token_end];
            if token.len() == 50 || token.matches('.').count() != 2 {
                continue;
            }
            if !seen.insert(token.to_string()) {
                continue;
            }

            let payload = match crate::crypto::jwt::JWT::get_payload(token) {
                Ok(payload) => payload,
                Err(_) => continue,
            };

            for key in ["xname", "ThirdPartyName", "displayName"] {
                if let Some(name) = crate::crypto::jwt::JWT::get_json_value(&payload, key) {
                    if !name.is_empty() {
                        return sanitize_component(&name);
                    }
                }
            }
        }

        "UnknownPlayer".to_string()
    }

    fn collect_metadata(payloads: &[String]) -> LoginMetadata {
        let all_payloads: Vec<String> = payloads.to_vec();

        LoginMetadata {
            xuid: extract_string(&all_payloads, &["XUID", "xuid"]),
            device_os: extract_i32(&all_payloads, &["DeviceOS", "device_os"]),
            device_model: extract_string(&all_payloads, &["DeviceModel", "device_model"]),
            playfab_id: extract_string(&all_payloads, &["PlayFabId", "playfabId"]),
            client_random_id: extract_string(&all_payloads, &["ClientRandomId", "clientRandomId"]),
            skin_geometry: extract_string(
                &all_payloads,
                &["SkinGeometryData", "SkinGeometry", "geometryData"],
            ),
            geometry_data_engine_version: extract_string(
                &all_payloads,
                &["geometryDataEngineVersion", "GeometryDataEngineVersion"],
            ),
            animation_data: extract_string(&all_payloads, &["animationData", "AnimationData"]),
            cape_data: extract_string(&all_payloads, &["capeData", "CapeData"]),
        }
    }

    debug!(
        "LOGIN parse: trying protocol/layout variants (data_len={})",
        data.len()
    );

    let parsed = try_parse_login(data, ProtocolEncoding::U32Le, false, "u32_le")
        .or_else(|| try_parse_login(data, ProtocolEncoding::U32Be, false, "u32_be"))
        .or_else(|| try_parse_login(data, ProtocolEncoding::VarInt, false, "varint"))
        .or_else(|| try_parse_login(data, ProtocolEncoding::U32Le, true, "u32_le+payload_len"))
        .or_else(|| try_parse_login(data, ProtocolEncoding::U32Be, true, "u32_be+payload_len"))
        .or_else(|| try_parse_login(data, ProtocolEncoding::VarInt, true, "varint+payload_len"));

    let Some(parsed) = parsed else {
        debug!("LOGIN parse: no known layout matched");
        return Ok(None);
    };

    debug!(
        "LOGIN parse: matched layout={} chain_len={} skin_len={}",
        parsed.layout, parsed.chain_len, parsed.skin_len
    );

    let player_name = extract_player_name(&parsed.chain_json);
    let skin_json = match crate::crypto::jwt::JWT::get_payload(&parsed.skin_jwt) {
        Ok(payload) => payload,
        Err(_) => return Ok(None),
    };

    let mut metadata_payloads =
        crate::crypto::jwt::parse_auth_chain(&parsed.chain_json).unwrap_or_default();
    metadata_payloads.push(skin_json.clone());
    let metadata = collect_metadata(&metadata_payloads);

    let skin_id = crate::crypto::jwt::JWT::get_json_value(&skin_json, "SkinId")
        .map(|v| sanitize_component(&v))
        .unwrap_or_default();

    let skin_image = match crate::crypto::jwt::JWT::get_json_value(&skin_json, "SkinData") {
        Some(v) if !v.is_empty() => v,
        _ => return Ok(None),
    };

    let skin_data = match decode_loose_base64(&skin_image) {
        Some(bytes) if !bytes.is_empty() && bytes.len() % 4 == 0 => bytes,
        _ => return Ok(None),
    };

    let width_str = crate::crypto::jwt::JWT::get_json_value(&skin_json, "SkinImageWidth");
    let height_str = crate::crypto::jwt::JWT::get_json_value(&skin_json, "SkinImageHeight");
    let pixels = skin_data.len() / 4;

    let mut img_w = width_str
        .as_deref()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);
    let mut img_h = height_str
        .as_deref()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);

    if img_w == 0 || img_h == 0 || (img_w as usize * img_h as usize) != pixels {
        if pixels == 64 * 32 {
            img_w = 64;
            img_h = 32;
        } else if pixels == 64 * 64 {
            img_w = 64;
            img_h = 64;
        } else if pixels == 128 * 64 {
            img_w = 128;
            img_h = 64;
        } else if pixels == 128 * 128 {
            img_w = 128;
            img_h = 128;
        } else {
            img_w = 64;
            img_h = (pixels as u32) / 64;
            if img_h == 0 {
                img_h = 64;
            }
        }
    }

    Ok(Some(ParsedLoginPacket {
        protocol: parsed.protocol,
        player_name,
        skin_id,
        chain_json: parsed.chain_json,
        skin_jwt: parsed.skin_jwt,
        skin_json,
        skin_data,
        skin_width: img_w,
        skin_height: img_h,
        metadata,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{
        engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
        Engine,
    };

    fn jwt_with_payload(payload: &str) -> String {
        format!(
            "{}.{}.{}",
            URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#),
            URL_SAFE_NO_PAD.encode(payload),
            "sig"
        )
    }

    fn login_payload(
        protocol: u32,
        protocol_encoding: ProtocolEncoding,
        has_payload_length: bool,
    ) -> Vec<u8> {
        let chain_jwt = jwt_with_payload(
            r#"{"xname":"Steve","identity":"00000000-0000-0000-0000-000000000000"}"#,
        );
        let chain_json = format!(r#"{{"chain":["{}"]}}"#, chain_jwt);

        let skin_data = vec![0u8; 64 * 64 * 4];
        let skin_b64 = STANDARD.encode(skin_data);
        let skin_jwt = jwt_with_payload(&format!(
            r#"{{"SkinId":"Standard_Steve","SkinData":"{}","SkinImageWidth":64,"SkinImageHeight":64}}"#,
            skin_b64
        ));

        let mut body = crate::util::Buffer::new();
        body.write_u32_le(chain_json.len() as u32).unwrap();
        body.write_bytes(chain_json.as_bytes()).unwrap();
        body.write_u32_le(skin_jwt.len() as u32).unwrap();
        body.write_bytes(skin_jwt.as_bytes()).unwrap();
        let body = body.to_vec();

        let mut out = crate::util::Buffer::new();
        match protocol_encoding {
            ProtocolEncoding::U32Le => out.write_u32_le(protocol).unwrap(),
            ProtocolEncoding::U32Be => out.write_u32(protocol).unwrap(),
            ProtocolEncoding::VarInt => out.write_var_int(protocol).unwrap(),
        }
        if has_payload_length {
            out.write_var_int(body.len() as u32).unwrap();
        }
        out.write_bytes(&body).unwrap();
        out.to_vec()
    }

    #[test]
    fn parses_legacy_u32_login_layout() {
        let parsed = parse_login_packet(&login_payload(766, ProtocolEncoding::U32Le, false))
            .unwrap()
            .expect("login should parse");

        assert_eq!(parsed.protocol, 766);
        assert_eq!(parsed.player_name, "Steve");
        assert_eq!(parsed.skin_id, "Standard_Steve");
        assert_eq!(parsed.skin_width, 64);
        assert_eq!(parsed.skin_height, 64);
    }

    #[test]
    fn parses_varint_payload_length_login_layout() {
        let parsed = parse_login_packet(&login_payload(975, ProtocolEncoding::VarInt, true))
            .unwrap()
            .expect("login should parse");

        assert_eq!(parsed.protocol, 975);
        assert_eq!(parsed.player_name, "Steve");
        assert_eq!(parsed.skin_data.len(), 64 * 64 * 4);
    }

    #[test]
    fn parses_big_endian_payload_length_login_layout() {
        let parsed = parse_login_packet(&login_payload(975, ProtocolEncoding::U32Be, true))
            .unwrap()
            .expect("login should parse");

        assert_eq!(parsed.protocol, 975);
        assert_eq!(parsed.player_name, "Steve");
        assert_eq!(parsed.skin_id, "Standard_Steve");
    }
}
