mod cli;

use clap::Parser;
use skingetbe::{
    network::Network,
    raknet::{RakNetConfig, RakNetServer},
    util::{Config, ConfigManager, Logger, StunClient},
};
use std::sync::Arc;

use cli::{normalize_args, Cli};

const LOGO: &str = r#"
   _____ _    _      _____      _   ____  ______
  / ____| |  (_)    / ____|    | | |  _ \|  ____|
  | (___ | | ___ _ _| |  __  ___| |_| |_) | |__
   \___ \| |/ / | '_ \ | |_ |/ _ \ __|  _ <|  __|
   ____) |   <| | | | | |__| |  __/ |_| |_) | |____
  |_____/|_|\_\_|_| |_|\_____|\___|\__|____/|______|
"#;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // `parse_from` treats the first element as the program name, so keep a
    // placeholder in position 0 (we only transform `/flag` -> `--flag` args).
    let mut args = normalize_args(std::env::args().skip(1));
    args.insert(0, "skingetbe".to_string());
    let cli = Cli::parse_from(args);

    // Initialize logging from the unified --logs level.
    skingetbe::init_with_verbosity(cli.log_level());
    // Print banner as a colored info block (no timestamp/label prefix)
    Logger::info_block(LOGO);

    Logger::info("SkinGetBE starting up...");

    let work_dir = std::env::current_dir()?;
    Logger::status("Working Directory", work_dir.display());

    // Config file path is next to the binary.
    let config_path = {
        let exe_path = std::env::current_exe()?;
        let bin_dir = exe_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Could not determine binary directory"))?;
        bin_dir.join("config.jsonc")
    };

    // Load configuration
    let config_mgr = ConfigManager::new(&config_path);
    let config = config_mgr.load()?;

    Logger::status("Version", &config.version);
    Logger::status("Protocol", config.protocol);
    Logger::status("Port", config.port);
    Logger::status("Bind Address", &config.bind_addr);
    Logger::status("Save Mode", config.savemode);

    // Initialize network
    let network_config = skingetbe::network::NetworkConfig {
        bind_addr: config.bind_addr.clone(),
        bind_port: config.port,
    };

    let mut network = Network::new(network_config);
    network.bind().await?;

    Logger::success(format!("UDP Server listening on port {}", config.port));

    Logger::info("Attempting NAT discovery (STUN)...");
    let server_socket = network.get_socket();
    let ext_ip = StunClient::new(5000)
        .discover_external_ip(&server_socket)
        .await;
    Logger::success(format!("External IP detected: {}", ext_ip));
    Logger::info(format!(
        "Note: Ensure UDP Port {} is open on your router if not reachable.",
        config.port
    ));

    run_server(network, config, cli.filter, ext_ip).await?;

    Ok(())
}

async fn run_server(
    network: Network,
    config: Config,
    filter_name: Option<String>,
    external_ip: String,
) -> anyhow::Result<()> {
    if let Some(filter_name) = filter_name.as_deref() {
        Logger::info(format!("Player name filter applied: {}", filter_name));
    }

    // Create RakNetServer with config values
    let raknet_config = RakNetConfig {
        server_guid: 0x1234567812345678,
        protocol_version: config.protocol,
        mtu_size: 1492,
        server_port: config.port,
        version: config.version.clone(),
        external_addr: Some(external_ip),
        savemode: config.savemode,
    };

    let socket = network.get_socket();
    let raknet_server = Arc::new(RakNetServer::new(Arc::new(socket), raknet_config, None));

    // Main server loop - receive UDP packets
    let mut buffer = vec![0u8; 65535];

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                Logger::info("Shutdown requested (Ctrl+C).".to_string());
                return Ok(());
            }
            res = network.recv_from(&mut buffer) => {
                match res {
                    Ok((size, addr)) => {
                        // Copy packet data for processing
                        let packet_data = buffer[..size].to_vec();

                        // Log incoming packet
                        tracing::debug!("Received {} bytes from {}", size, addr);

                        // Synchronously route to RakNetServer (performance: avoid tokio::spawn overhead)
                        if let Err(e) = raknet_server.handle_packet(&packet_data, addr).await {
                            tracing::warn!("Error handling packet from {}: {}", addr, e);
                        }
                    }
                    Err(e) => {
                        tracing::error!("Error receiving packet: {}", e);
                        // Continue listening despite errors
                        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    }
                }
            }
        }
    }
}
