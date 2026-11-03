mod cli;

use clap::Parser;
use nethernet_tokio::signaling::http::{HttpServerConfig, HttpSignalingServer};
use nethernet_tokio::signaling::lan::LanSignaling;
use nethernet_tokio::ServerIdentity;
use nethernet_tokio::{AcceptedSession, ConnectionConfig, NetherServer, ServerData};
use skingetbe::util::{Config, ConfigManager, Logger};
use std::sync::Arc;
use std::time::SystemTime;

use cli::{normalize_args, Cli};

const LOGO: &str = r#"
   _____ _    _      _____      _   ____  ______
  / ____| |  (_)    / ____|    | | |  _ \|  ____|
  | (___ | | ___ _ _| |  __  ___| |_| |_) | |__
   \___ \| |/ / | '_ \ | |_ |/ _ \ __|  _ <|  __|
   ____) |   <| | | | | |__| |  __/ |_| |_) | |____
  |_____/|_|\_\_|_| |_|\_____|\___|\__|____/|______
"#;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = normalize_args(std::env::args().skip(1));
    args.insert(0, "skingetbe".to_string());
    let cli = Cli::parse_from(args);

    skingetbe::init_with_verbosity(cli.log_level());
    Logger::info_block(LOGO);

    Logger::status_text("SkinGetBE starting up...");

    let work_dir = std::env::current_dir()?;
    Logger::status("Working Directory", work_dir.display());

    let config_path = {
        let exe_path = std::env::current_exe()?;
        let bin_dir = exe_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Could not determine binary directory"))?;
        bin_dir.join("config.jsonc")
    };

    let config_mgr = ConfigManager::new(&config_path);
    let config = config_mgr.load()?;

    Logger::status("Version", &config.version);
    Logger::status("Protocol", config.protocol);
    Logger::status("Save Mode", config.savemode);

    let network_id = rand::random::<u64>();
    let network_id_text = network_id.to_string();
    let bind_addr = format!("{}:{}", config.bind_addr, config.port).parse()?;
    let signaling = HttpSignalingServer::bind(
        bind_addr,
        HttpServerConfig {
            network_id: network_id_text.clone(),
            ..Default::default()
        },
    )
    .await?;
    let lan_bind_addr = format!("{}:7551", config.bind_addr).parse()?;
    let lan_signaling = LanSignaling::new(network_id, lan_bind_addr).await?;
    let advertised_protocol = skingetbe::bedrock::get_latest_protocol() as u32;
    let advertised_version = skingetbe::bedrock::get_latest_version();
    let mut server_data = ServerData::new(
        "SkinGetBE NetherNet Server".to_string(),
        "Skin Capture".to_string(),
    );
    server_data.protocol_version = advertised_protocol;
    server_data.game_version = advertised_version;
    server_data.player_count = 0;
    server_data.max_player_count = 1;
    signaling.set_server_data(server_data.clone());
    lan_signaling.set_server_data(server_data);

    let identity = ServerIdentity::generate("SkinGetBE", SystemTime::now())?;
    let connection_config = ConnectionConfig {
        identity: Some(Arc::new(identity)),
        ..Default::default()
    };

    Logger::status_text(format!(
        "NetherNet HTTP signaling listening on {}",
        bind_addr
    ));
    Logger::status_text(format!(
        "NetherNet LAN discovery listening on {}",
        lan_bind_addr
    ));
    Logger::status_text(format!("Network ID: {}", network_id_text));

    let mut http_listener = NetherServer::bind_with(signaling, connection_config.clone()).await?;
    let mut lan_listener = NetherServer::bind_with(lan_signaling, connection_config).await?;
    Logger::success("NetherNet server ready");

    let config = Arc::new(config);
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                Logger::status_text("Shutdown requested (Ctrl+C).".to_string());
                return Ok(());
            }
            res = http_listener.accept() => {
                match res {
                    Ok(accepted) => {
                        let config = config.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_session(accepted, config).await {
                                tracing::warn!("HTTP NetherNet session error: {}", e);
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!("HTTP NetherNet accept error: {}", e);
                    }
                }
            }
            res = lan_listener.accept() => {
                match res {
                    Ok(accepted) => {
                        let config = config.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_session(accepted, config).await {
                                tracing::warn!("LAN NetherNet session error: {}", e);
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!("LAN NetherNet accept error: {}", e);
                    }
                }
            }
        }
    }
}

async fn handle_session(accepted: AcceptedSession, config: Arc<Config>) -> anyhow::Result<()> {
    let AcceptedSession {
        session,
        mut reliable,
        mut unreliable,
        ..
    } = accepted;

    let mut compression_enabled = false;
    let protocol = config.protocol as u32;
    let mut reliable_open = true;
    let mut unreliable_open = true;

    while reliable_open || unreliable_open {
        let data = tokio::select! {
            result = reliable.recv(), if reliable_open => {
                match result? {
                    Some(data) => Some(data),
                    None => {
                        reliable_open = false;
                        None
                    }
                }
            }
            result = unreliable.recv(), if unreliable_open => {
                match result? {
                    Some(data) => Some(data),
                    None => {
                        unreliable_open = false;
                        None
                    }
                }
            }
        };

        let Some(data) = data else {
            continue;
        };
        let bytes = data.to_vec();
        if bytes.is_empty() {
            continue;
        }
        // NetherNet has no RakNet batch header. Accept legacy-prefixed data for
        // compatibility, but never add that prefix to outgoing messages.
        let payload = if bytes.first() == Some(&0xfe) {
            &bytes[1..]
        } else {
            &bytes[..]
        };

        let decompressed = match skingetbe::bedrock::decompress_batch(payload, compression_enabled)
        {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("Invalid Bedrock batch ({} bytes): {}", payload.len(), e);
                continue;
            }
        };

        let packets = skingetbe::bedrock::parse_batch(&decompressed).unwrap_or_default();
        for pkt in packets {
            if pkt.is_empty() {
                continue;
            }
            // read packet id as varint
            let mut buf = skingetbe::util::Buffer::from(&pkt);
            let packet_id = buf.read_var_int().unwrap_or(pkt[0] as u32);

            match packet_id {
                0xc1 => {
                    let proto = buf.read_u32().unwrap_or(protocol);
                    let ver = skingetbe::bedrock::resolve_version(proto);
                    tracing::info!("RequestNetworkSettings proto={} ({})", proto, ver);
                    compression_enabled = true;
                    // NetworkSettings itself is always uncompressed.
                    let ns = skingetbe::bedrock::build_network_settings();
                    let resp = build_single_batch(&ns, false);
                    tracing::info!("Sending NetworkSettings {} bytes", resp.len());
                    if let Err(e) = session.send(resp.into()).await {
                        tracing::error!("send failed: {}", e);
                    }
                }
                0x01 => {
                    let remaining = buf.read_remaining();
                    match skingetbe::bedrock::login::parse_login_packet(&remaining) {
                        Ok(Some(parsed)) => {
                            tracing::info!("Detected Player: {}", parsed.player_name);
                            let base_dir = std::env::current_exe()
                                .ok()
                                .and_then(|p| p.parent().map(|x| x.to_path_buf()))
                                .or_else(|| std::env::current_dir().ok())
                                .unwrap_or_else(|| std::path::PathBuf::from("."));
                            match skingetbe::bedrock::skin::ExtractedSkin::new(
                                parsed.skin_width,
                                parsed.skin_height,
                                parsed.skin_data.clone(),
                            ) {
                                Ok(skin) => match skingetbe::bedrock::skin::save_login_capture(
                                    &base_dir,
                                    &parsed.player_name,
                                    &parsed.skin_id,
                                    &parsed.metadata,
                                    &skin,
                                    config.savemode,
                                ) {
                                    Ok(_) => {
                                        tracing::info!(target: "success", "Skin saved for {}", parsed.player_name);
                                    }
                                    Err(e) => tracing::warn!("save failed: {}", e),
                                },
                                Err(e) => tracing::warn!("skin validation failed: {}", e),
                            }
                            // send play status + disconnect bundle
                            let play_status = skingetbe::bedrock::build_play_status(0);
                            let disconnect =
                                skingetbe::bedrock::build_disconnect(0, "Skin captured");
                            let bundle =
                                build_batch(&[play_status, disconnect], compression_enabled);
                            let _ = session.send(bundle.into()).await;
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                            let _ = session.close().await;
                            return Ok(());
                        }
                        _ => {}
                    }
                }
                _ => {
                    tracing::debug!("Unhandled bedrock packet 0x{:02X}", packet_id);
                }
            }
        }
    }
    Ok(())
}

fn build_single_batch(packet: &[u8], compression_enabled: bool) -> Vec<u8> {
    build_batch(&[packet.to_vec()], compression_enabled)
}

fn build_batch(packets: &[Vec<u8>], compression_enabled: bool) -> Vec<u8> {
    use skingetbe::util::Buffer;
    let mut combined = Buffer::new();
    for p in packets {
        combined.write_var_int(p.len() as u32).unwrap();
        combined.write_bytes(p).unwrap();
    }
    let combined = combined.to_vec();

    if compression_enabled {
        skingetbe::bedrock::compress_batch(&combined)
            .expect("compressing a Bedrock response into memory cannot fail")
    } else {
        combined
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_settings_is_length_prefixed_without_raknet_batch_marker() {
        let packet = skingetbe::bedrock::build_network_settings();
        let encoded = build_single_batch(&packet, false);
        assert_eq!(encoded[0] as usize, packet.len());
        assert_eq!(&encoded[1..], packet.as_slice());
        assert_eq!(encoded[1], 0x8f); // VarInt(0x8f), not 0xfe
        assert_eq!(encoded.len(), 1 + packet.len());
    }

    #[test]
    fn compressed_responses_have_algorithm_byte_but_no_batch_marker() {
        let packet = skingetbe::bedrock::build_play_status(0);
        let encoded = build_single_batch(&packet, true);
        assert_eq!(encoded[0], 0); // zlib algorithm
        let decoded = skingetbe::bedrock::decompress_batch(&encoded, true).unwrap();
        assert_eq!(
            skingetbe::bedrock::parse_batch(&decoded).unwrap(),
            vec![packet]
        );
    }
}
