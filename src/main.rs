use skingetbe::{
    network::Network,
    raknet::{RakNetConfig, RakNetServer},
    util::{ConfigManager, Logger},
};
use std::sync::Arc;

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
    let cli = match parse_args(std::env::args().skip(1).collect()) {
        Ok(cli) => cli,
        Err(message) => {
            eprintln!("{}", message);
            print_help();
            return Ok(());
        }
    };

    // Initialize logging from the unified --logs level.
    skingetbe::init_with_verbosity(cli.log_level);
    // Print banner as a colored info block (no timestamp/label prefix)
    Logger::info_block(LOGO);

    Logger::info("SkinGetBE starting up...");

    // Get working directory
    let work_dir = std::env::current_dir()?;
    Logger::status("Working Directory", work_dir.display());

    // Determine config file path (same directory as binary)
    let config_path = {
        let exe_path = std::env::current_exe()?;
        let bin_dir = exe_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Could not determine binary directory"))?;
        bin_dir.join("config.jsonc")
    };

    // Load configuration
    let config_mgr = ConfigManager::new(&config_path);
    let config = match config_mgr.load().await {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::error!("Failed to load config: {}", e);
            return Err(e.into());
        }
    };

    Logger::status("Version", &config.version);
    Logger::status("Protocol", config.protocol);
    Logger::status("Port", config.port);
    Logger::status("Bind Address", &config.bind_addr);
    Logger::status("Save Mode", config.savemode);

    // Initialize network
    let network_config = skingetbe::network::NetworkConfig {
        bind_addr: config.bind_addr.clone(),
        bind_port: config.port,
        max_packet_size: 65535,
        timeout_ms: 5000,
    };

    let mut network = Network::new(network_config);
    network.bind().await?;

    Logger::success(format!("UDP Server listening on port {}", config.port));

    Logger::info("Attempting NAT discovery (STUN)...");
    let server_socket = network.get_socket();
    let ext_ip = discover_external_ip(&server_socket).await;
    Logger::success(format!("External IP detected: {}", ext_ip));
    Logger::info(format!(
        "Note: Ensure UDP Port {} is open on your router if not reachable.",
        config.port
    ));

    run_server(network, config, cli.filter_name, ext_ip).await?;

    Ok(())
}

async fn run_server(
    network: Network,
    config: skingetbe::util::Config,
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

async fn discover_external_ip(socket: &skingetbe::network::UdpSocket) -> String {
    let client = skingetbe::util::stun::StunClient::new(5000);
    client.discover_external_ip(socket).await
}

/// Print help message
fn print_help() {
    println!("Usage: SkinGetBE.exe [options]");
    println!("Options:");
    println!("  -h, --help          Show this help message");
    println!("  --config            Enable loading version/protocol from config.jsonc");
    println!("  --filter <name>     Filter displayed players by name (substring match)");
    println!("  --logs [level]      Set log level: 0=error, 1=warn, 2=info, 3=debug, 4=trace");
    println!("Example: SkinGetBE.exe --filter Steve --logs 3");
}

#[derive(Debug)]
struct CliOptions {
    log_level: u8,
    _config: bool,
    filter_name: Option<String>,
}

fn parse_args(args: Vec<String>) -> Result<CliOptions, String> {
    let mut log_level = 2u8;
    let mut config = true;
    let mut filter_name = None;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "-h" | "--help" | "/?" | "/help" => {
                print_help();
                std::process::exit(0);
            }
            "-d" | "--debug" | "/debug" => {
                // Backward-compatible alias for the old debug flag.
                log_level = 3;
                index += 1;
            }
            "--config" | "-c" | "/config" => {
                config = true;
                index += 1;
            }
            "--logs" => {
                match args.get(index + 1) {
                    Some(value) if !value.starts_with('-') && !value.starts_with('/') => {
                        log_level = parse_log_level(value)?;
                        index += 2;
                    }
                    _ => {
                        // `--logs` alone means the old verbose/debug behavior.
                        log_level = 3;
                        index += 1;
                    }
                }
            }
            "--filter" => {
                let value = args.get(index + 1).ok_or_else(|| {
                    "Missing value for --filter. Expected a player name substring.".to_string()
                })?;
                filter_name = Some(value.clone());
                index += 2;
            }
            other => {
                return Err(format!("Unknown argument: {}", other));
            }
        }
    }

    Ok(CliOptions {
        log_level,
        _config: config,
        filter_name,
    })
}

fn parse_log_level(value: &str) -> Result<u8, String> {
    let parsed = value.parse::<u8>().map_err(|_| {
        format!(
            "Invalid --logs level: {}. Expected 0, 1, 2, 3, or 4.",
            value
        )
    })?;

    if parsed > 4 {
        return Err(format!(
            "Invalid --logs level: {}. Expected 0, 1, 2, 3, or 4.",
            value
        ));
    }

    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::parse_args;

    #[test]
    fn parses_logs_level_and_filter_arguments() {
        let cli = parse_args(vec![
            "--logs".into(),
            "3".into(),
            "--filter".into(),
            "Steve".into(),
        ])
        .expect("parse should succeed");

        assert_eq!(cli.log_level, 3);
        assert_eq!(cli.filter_name.as_deref(), Some("Steve"));
    }

    #[test]
    fn logs_without_level_enables_debug_verbosity() {
        let cli = parse_args(vec!["--logs".into()]).expect("parse should succeed");

        assert_eq!(cli.log_level, 3);
    }

    #[test]
    fn debug_alias_maps_to_debug_verbosity() {
        let cli = parse_args(vec!["--debug".into()]).expect("parse should succeed");

        assert_eq!(cli.log_level, 3);
    }

    #[test]
    fn rejects_invalid_logs_level() {
        let error = parse_args(vec!["--logs".into(), "9".into()]).expect_err("parse should fail");

        assert!(error.contains("Invalid --logs level"));
    }

    #[test]
    fn rejects_missing_filter_value() {
        let error = parse_args(vec!["--filter".into()]).expect_err("parse should fail");

        assert!(error.contains("Missing value for --filter"));
    }
}
