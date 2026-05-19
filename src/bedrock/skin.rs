//! Skin data extraction and PNG file generation

use crate::Result;
use crc32fast::Hasher;
use flate2::{write::ZlibEncoder, Compression};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

/// Extracted skin information
#[derive(Debug, Clone)]
pub struct ExtractedSkin {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl ExtractedSkin {
    /// Create a new skin with RGBA data
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> Result<Self> {
        // Validate that data size matches width * height * 4 bytes (RGBA)
        if data.len() != (width as usize * height as usize * 4) {
            return Err(crate::Error::InvalidData(format!(
                "Skin data size mismatch: expected {}, got {}",
                width as usize * height as usize * 4,
                data.len()
            )));
        }
        Ok(Self {
            width,
            height,
            data,
        })
    }
}

/// Skin data extractor
pub struct SkinExtractor;

impl SkinExtractor {
    /// Extract skin from packed data
    pub fn extract_skin(data: &[u8]) -> Result<ExtractedSkin> {
        // Validate that data is in RGBA format and meets minimum skin size
        // Minimum accepted skin size: 64 x 32 x 4 (RGBA)
        let min_skin_bytes = 64usize * 32usize * 4usize;
        if data.len() < min_skin_bytes {
            return Err(crate::Error::InvalidData(format!(
                "Skin data too small: {} bytes (minimum {})",
                data.len(),
                min_skin_bytes
            )));
        }

        let pixels = data.len() / 4;

        // Detect dimensions based on standard Minecraft skin sizes
        let (width, height) = if pixels == 64 * 32 {
            (64u32, 32u32)
        } else if pixels == 64 * 64 {
            (64u32, 64u32)
        } else if pixels == 128 * 64 {
            (128u32, 64u32)
        } else if pixels == 128 * 128 {
            (128u32, 128u32)
        } else {
            // Try to infer from available data
            let width = 64u32;
            let mut height = (pixels as u32) / width;
            if height <= 0 {
                height = 64;
            }
            (width, height)
        };

        Ok(ExtractedSkin {
            width,
            height,
            data: data.to_vec(),
        })
    }

    /// Save skin to PNG file
    pub fn save_skin(skin: &ExtractedSkin, path: &str) -> Result<()> {
        // Create directory if it doesn't exist
        if let Some(parent) = Path::new(path).parent() {
            fs::create_dir_all(parent)?;
        }

        // Write PNG file
        Self::write_png_file(path, skin)?;

        tracing::info!(target: "success", "Skin saved to: {}", path);
        Ok(())
    }

    /// Save skin to PNG file (async wrapper)
    pub async fn save_skin_async(skin: &ExtractedSkin, path: &str) -> Result<()> {
        Self::save_skin(skin, path)
    }

    /// Write PNG file with RGBA data
    fn write_png_file(path: &str, skin: &ExtractedSkin) -> Result<()> {
        let png_bytes = encode_png_rgba(&skin.data, skin.width, skin.height)?;
        fs::write(path, png_bytes)?;
        Ok(())
    }
}

/// Save a parsed login capture using the upstream C++ filename rules.
pub fn save_login_capture(
    base_dir: &Path,
    player_name: &str,
    skin_id: &str,
    metadata: &crate::bedrock::login::LoginMetadata,
    skin: &ExtractedSkin,
    savemode: u8,
) -> Result<Option<PathBuf>> {
    if savemode == 0 {
        tracing::info!("Skin save skipped by config savemode=0");
        return Ok(None);
    }

    let skins_dir = base_dir.join("skins");
    fs::create_dir_all(&skins_dir)?;

    let safe_player = sanitize_component(player_name);
    let safe_skin = sanitize_component(skin_id);
    let base_name = format!("{}_{}", safe_player, safe_skin);

    let stem = match savemode {
        1 => base_name,
        _ => next_available_stem(&skins_dir, &base_name),
    };
    let target_path = skins_dir.join(format!("{}.png", stem));
    let metadata_path = skins_dir.join(format!("{}.json", stem));

    let png_bytes = encode_png_rgba(&skin.data, skin.width, skin.height)?;
    fs::write(&target_path, png_bytes)?;
    tracing::info!(target: "success", ">>> Saved PNG: {} ({}x{})", target_path.display(), skin.width, skin.height);
    tracing::info!("Skin saved: {}", target_path.display());

    let sidecar = serde_json::json!({
        "player_name": player_name,
        "skin_id": skin_id,
        "skin_width": skin.width,
        "skin_height": skin.height,
        "metadata": metadata,
    });
    if let Err(e) = fs::write(&metadata_path, serde_json::to_vec_pretty(&sidecar)?) {
        tracing::warn!(
            "Failed to write login metadata sidecar {}: {}",
            metadata_path.display(),
            e
        );
    } else {
        tracing::debug!("Saved login metadata: {}", metadata_path.display());
    }
    Ok(Some(target_path))
}

fn next_available_stem(skins_dir: &Path, base_name: &str) -> String {
    let base_png = skins_dir.join(format!("{}.png", base_name));
    let base_json = skins_dir.join(format!("{}.json", base_name));
    if !base_png.exists() && !base_json.exists() {
        return base_name.to_string();
    }

    let mut count = 1u32;
    loop {
        let stem = format!("{}_{}", base_name, count);
        let png_path = skins_dir.join(format!("{}.png", stem));
        let json_path = skins_dir.join(format!("{}.json", stem));
        if !png_path.exists() && !json_path.exists() {
            return stem;
        }
        count = count.saturating_add(1);
    }
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

fn encode_png_rgba(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    if rgba.len() != width as usize * height as usize * 4 {
        return Err(crate::Error::InvalidData(format!(
            "Skin data size mismatch: expected {}, got {}",
            width as usize * height as usize * 4,
            rgba.len()
        )));
    }

    let mut raw = Vec::with_capacity((width as usize * 4 + 1) * height as usize);
    let row_bytes = width as usize * 4;
    for row in 0..height as usize {
        raw.push(0);
        let start = row * row_bytes;
        raw.extend_from_slice(&rgba[start..start + row_bytes]);
    }

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(6));
    encoder.write_all(&raw)?;
    let compressed = encoder.finish()?;

    let mut png = Vec::new();
    png.extend_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]);

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.push(8);
    ihdr.push(6);
    ihdr.push(0);
    ihdr.push(0);
    ihdr.push(0);

    write_png_chunk(&mut png, b"IHDR", &ihdr);
    write_png_chunk(&mut png, b"IDAT", &compressed);
    write_png_chunk(&mut png, b"IEND", &[]);
    Ok(png)
}

fn write_png_chunk(out: &mut Vec<u8>, chunk_type: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(chunk_type);
    out.extend_from_slice(data);

    let mut hasher = Hasher::new();
    hasher.update(chunk_type);
    hasher.update(data);
    out.extend_from_slice(&hasher.finalize().to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn test_extract_skin_64x64() {
        let data = vec![0u8; 64 * 64 * 4];
        let skin = SkinExtractor::extract_skin(&data).unwrap();
        assert_eq!(skin.width, 64);
        assert_eq!(skin.height, 64);
    }

    #[test]
    fn test_extract_skin_128x64() {
        let data = vec![0u8; 128 * 64 * 4];
        let skin = SkinExtractor::extract_skin(&data).unwrap();
        assert_eq!(skin.width, 128);
        assert_eq!(skin.height, 64);
    }

    #[test]
    fn test_invalid_data_size() {
        let data = vec![0u8; 100]; // Not divisible by 4
        assert!(SkinExtractor::extract_skin(&data).is_err());
    }

    #[test]
    fn test_save_login_capture_numbered_png_and_json_share_suffix() {
        let base_dir = unique_test_dir("numbered");
        let skin = ExtractedSkin::new(64, 64, vec![0u8; 64 * 64 * 4]).unwrap();
        let metadata = crate::bedrock::login::LoginMetadata::default();

        let first = save_login_capture(&base_dir, "Steve", "Skin", &metadata, &skin, 2)
            .unwrap()
            .unwrap();
        let second = save_login_capture(&base_dir, "Steve", "Skin", &metadata, &skin, 2)
            .unwrap()
            .unwrap();

        assert_eq!(
            first.file_name().and_then(|v| v.to_str()),
            Some("Steve_Skin.png")
        );
        assert_eq!(
            second.file_name().and_then(|v| v.to_str()),
            Some("Steve_Skin_1.png")
        );
        assert!(base_dir.join("skins").join("Steve_Skin_1.json").exists());

        let _ = fs::remove_dir_all(base_dir);
    }

    #[test]
    fn test_save_login_capture_disabled_writes_nothing() {
        let base_dir = unique_test_dir("disabled");
        let skin = ExtractedSkin::new(64, 64, vec![0u8; 64 * 64 * 4]).unwrap();
        let metadata = crate::bedrock::login::LoginMetadata::default();

        let saved = save_login_capture(&base_dir, "Steve", "Skin", &metadata, &skin, 0).unwrap();

        assert!(saved.is_none());
        assert!(!base_dir.join("skins").exists());

        let _ = fs::remove_dir_all(base_dir);
    }

    #[test]
    fn test_save_login_capture_overwrites_base_files() {
        let base_dir = unique_test_dir("overwrite");
        let skin = ExtractedSkin::new(64, 64, vec![0u8; 64 * 64 * 4]).unwrap();
        let metadata = crate::bedrock::login::LoginMetadata::default();

        let first = save_login_capture(&base_dir, "Steve", "Skin", &metadata, &skin, 1)
            .unwrap()
            .unwrap();
        let second = save_login_capture(&base_dir, "Steve", "Skin", &metadata, &skin, 1)
            .unwrap()
            .unwrap();

        assert_eq!(first, second);
        assert!(!base_dir.join("skins").join("Steve_Skin_1.png").exists());
        assert!(base_dir.join("skins").join("Steve_Skin.json").exists());

        let _ = fs::remove_dir_all(base_dir);
    }

    fn unique_test_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "skingetbe_{}_{}_{}",
            name,
            std::process::id(),
            nanos
        ))
    }
}
