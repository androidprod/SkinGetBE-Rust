//! Configuration management for SkinGetBE

use crate::{bedrock, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tracing::info;

/// Server configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Server version string
    pub version: String,

    /// Bedrock protocol version
    pub protocol: u16,

    /// Server port
    pub port: u16,

    /// Bind address
    pub bind_addr: String,

    /// Skin save mode: 0 = disabled, 1 = overwrite, 2 = create numbered files
    pub savemode: u8,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: "1.26.21".to_string(),
            protocol: 975,
            port: 19132,
            bind_addr: "0.0.0.0".to_string(),
            savemode: 2,
        }
    }
}

/// Configuration manager
pub struct ConfigManager {
    config_path: PathBuf,
}

impl ConfigManager {
    /// Create a new configuration manager
    pub fn new(config_path: impl AsRef<Path>) -> Self {
        Self {
            config_path: config_path.as_ref().to_path_buf(),
        }
    }

    /// Load configuration from file (or create default)
    pub fn load(&self) -> Result<Config> {
        // Create parent directory if needed
        if let Some(parent) = self.config_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Check if config file exists
        if self.config_path.exists() {
            info!("Loading config from: {}", self.config_path.display());
            let content = fs::read_to_string(&self.config_path)?;
            let stripped = strip_json_comments(&content);
            let mut config: Config = serde_json::from_str(&stripped)?;
            let normalized = Self::normalize_supported_version(&config);
            if normalized != config {
                self.save(&normalized)?;
                config = normalized;
            }
            Ok(config)
        } else {
            // Create default config
            info!("Creating default config at: {}", self.config_path.display());
            let config = Config::default();
            self.save(&config)?;
            Ok(config)
        }
    }

    /// Save configuration to file
    pub fn save(&self, config: &Config) -> Result<()> {
        // Create parent directory if needed
        if let Some(parent) = self.config_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let content = format_config_with_comments(config);
        fs::write(&self.config_path, content)?;
        info!("Config saved to: {}", self.config_path.display());
        Ok(())
    }

    fn normalize_supported_version(config: &Config) -> Config {
        let latest_protocol = bedrock::get_latest_protocol();
        let latest_version = bedrock::get_latest_version();

        let protocol_missing = config.protocol == 0;
        let version_empty = config.version.trim().is_empty();

        if protocol_missing || version_empty {
            info!("Config missing version/protocol, filling defaults from latest supported");
            return Config {
                version: if version_empty {
                    latest_version
                } else {
                    config.version.clone()
                },
                protocol: if protocol_missing {
                    latest_protocol
                } else {
                    config.protocol
                },
                port: config.port,
                bind_addr: config.bind_addr.clone(),
                savemode: config.savemode.min(2),
            };
        }

        let mut normalized = config.clone();
        if normalized.savemode > 2 {
            info!("Config savemode out of range, using numbered-save mode (2)");
            normalized.savemode = 2;
        }
        normalized
    }
}

fn format_config_with_comments(config: &Config) -> String {
    format!(
        concat!(
            "{{\n",
            "  // Minecraft Bedrock version string shown in the server list.\n",
            "  \"version\": \"{}\",\n",
            "  // Bedrock protocol number. Keep this aligned with the client version.\n",
            "  \"protocol\": {},\n",
            "  // UDP port to listen on.\n",
            "  \"port\": {},\n",
            "  // Local bind address. 0.0.0.0 listens on all network interfaces.\n",
            "  \"bind_addr\": \"{}\",\n",
            "  // Skin save mode: 0 = do not save, 1 = overwrite, 2 = save as numbered new files.\n",
            "  \"savemode\": {}\n",
            "}}\n"
        ),
        escape_json_string(&config.version),
        config.protocol,
        config.port,
        escape_json_string(&config.bind_addr),
        config.savemode.min(2)
    )
}

fn escape_json_string(value: &str) -> String {
    value
        .chars()
        .flat_map(|c| match c {
            '"' => "\\\"".chars().collect::<Vec<_>>(),
            '\\' => "\\\\".chars().collect::<Vec<_>>(),
            '\n' => "\\n".chars().collect::<Vec<_>>(),
            '\r' => "\\r".chars().collect::<Vec<_>>(),
            '\t' => "\\t".chars().collect::<Vec<_>>(),
            _ => vec![c],
        })
        .collect()
}

fn strip_json_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;

    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }

        if c == '"' {
            in_string = true;
            out.push(c);
            continue;
        }

        if c == '/' {
            match chars.peek().copied() {
                Some('/') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if next == '\n' {
                            out.push('\n');
                            break;
                        }
                    }
                    continue;
                }
                Some('*') => {
                    chars.next();
                    let mut previous = '\0';
                    for next in chars.by_ref() {
                        if previous == '*' && next == '/' {
                            break;
                        }
                        previous = next;
                    }
                    continue;
                }
                _ => {}
            }
        }

        out.push(c);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.port, 19132);
        assert_eq!(config.protocol, 975);
        assert_eq!(config.version, "1.26.21");
    }

    #[tokio::test]
    async fn test_config_serialization() {
        let config = Config::default();
        let json = serde_json::to_string(&config).unwrap();
        let loaded: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(config.port, loaded.port);
    }

    #[test]
    fn test_normalize_supported_version_matches_protocol() {
        let config = Config {
            version: "old-version".to_string(),
            protocol: 944,
            port: 19132,
            bind_addr: "0.0.0.0".to_string(),
            savemode: 2,
        };

        let normalized = ConfigManager::normalize_supported_version(&config);
        assert_eq!(normalized.protocol, 944);
        assert_eq!(normalized.version, "old-version");
    }

    #[test]
    fn test_commented_config_deserializes() {
        let raw = r#"{
            // Keep latest supported client here.
            "version": "1.26.21",
            "protocol": 975,
            "port": 19132,
            "bind_addr": "0.0.0.0",
            "savemode": 1
        }"#;

        let stripped = strip_json_comments(raw);
        let config: Config = serde_json::from_str(&stripped).unwrap();
        assert_eq!(config.savemode, 1);
    }
}
