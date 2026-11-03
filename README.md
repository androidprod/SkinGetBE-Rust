<div align="center">

<img src="res/app.ico" width="80" alt="SkinGetBE Logo" />

# SkinGetBE - Rust Edition

**Minecraft Bedrock Edition skin capture over NetherNet (WebRTC)**

[![Rust Edition](https://img.shields.io/badge/Edition-Rust-orange?style=flat-square&logo=rust)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow?style=flat-square)](LICENSE)
[![Async Runtime](https://img.shields.io/badge/Runtime-Tokio-blue?style=flat-square)](https://tokio.rs/)

**English** | [日本語](README.ja.md)

</div>

## Overview

SkinGetBE accepts NetherNet connections from Minecraft Bedrock clients, reads the Bedrock Login packet, and saves the player's skin as a PNG alongside JSON metadata. It is a skin capture tool, **not a playable game server**: after a Login packet is parsed, it sends PlayStatus and Disconnect, then closes the connection.

The running binary uses [`nethernet-tokio`](https://github.com/bedrock-crustaceans/nethernet) for WebRTC transport, HTTP signaling, and LAN discovery. The older RakNet implementation remains in `src/raknet/` but is not started by `src/main.rs`.

> **Research tool:** Do not deploy as an authenticated public server. The Bedrock JWT code decodes claims but does not verify the Login chain's signatures or authenticate a player's identity. WebRTC transport encryption is separate from Login authentication.

## Features and limitations

- HTTP signaling on TCP `port` (default **19132**) and LAN discovery on UDP **7551**.
- WebRTC reliable/unreliable data channels handled per connection; Bedrock NetworkSettings, Login parsing, raw-DEFLATE batch handling, and response encoding.
- Player name and skin data extraction from Login JWT payloads; RGBA skin saved as PNG with a JSON sidecar.
- Save modes: disabled (`0`), overwrite (`1`), or numbered duplicates (`2`).
- Colored logs, JSONC configuration, and a protocol-to-version lookup table (currently through **26.60 / 2225**).
- Not a full game server; cape images and geometry are not saved as separate files.
- Xbox Live session registration / Microsoft-authenticated server operation is not implemented. The server identity used for signaling is generated on each start rather than persisted.

### Data flow

```text
HTTP signaling (TCP port) or LAN discovery (UDP 7551)
  -> NetherNet WebRTC connection (ICE / DTLS / SCTP)
  -> RequestNetworkSettings -> uncompressed NetworkSettings
  -> compressed Bedrock Login -> JWT payloads -> RGBA skin
  -> skins/<player>_<skinId>.png + .json
  -> PlayStatus / Disconnect -> session closed
```

`NetworkSettings` is uncompressed. Once compression is negotiated, Bedrock batches use an algorithm byte (`0` for raw DEFLATE; the reader also accepts `0xff` for uncompressed payloads). NetherNet messages do not use a RakNet `0xfe` header when sent.

## Build

Install a recent stable Rust toolchain and Cargo, then run:

```bash
cargo build --release
cargo test --locked
```

The executable is `target/release/skingetbe.exe` on Windows or `target/release/skingetbe` on Linux/macOS. Cargo fetches the NetherNet dependency from GitHub on the first build; a local `vendor/` checkout is **not** needed. The Windows icon is embedded by `build.rs` on Windows.

## Run

```bash
cargo run -- --logs 3
# or run the release executable directly
./target/release/skingetbe --logs 3
```

On Windows, use `target\release\skingetbe.exe`. `--logs` sets the level: `0` error, `1` warn, `2` info (default), `3` debug, `4` trace. Without a number, `--logs` selects `3`; `--debug` is an alias. Windows-style `/flag` arguments are also accepted. `--config` is retained for compatibility; configuration is loaded regardless. **`--filter` is parsed but not currently applied to captures.**

On first run, `config.jsonc` is created **next to the executable** (for `cargo run`, under `target/debug/`):

```jsonc
{
  "version": "1.26.21",
  "protocol": 975,
  "port": 19132,
  "bind_addr": "0.0.0.0",
  "savemode": 2
}
```

| Key | Purpose |
|-----|---------|
| `port` | HTTP signaling TCP listen port (default `19132`) |
| `bind_addr` | Local bind address for signaling and LAN discovery |
| `protocol`, `version` | Configured Bedrock protocol/version; defaults `975` / `1.26.21` |
| `savemode` | `0` no files, `1` overwrite, `2` save numbered duplicates |

JSONC comments are supported. Only a **protocol value of `0`** or an **empty version** is filled from the latest mapping; omitting a key uses the built-in default instead. Other nonempty values are not automatically replaced. **Currently, discovery advertises the latest version from `src/bedrock/version.rs` rather than these config values.** With the defaults above, the startup log shows `975 / 1.26.21`, but discovery advertises `2225 / 26.60`. Edit `config.jsonc` to make the displayed settings match your client; this does not change the current discovery-advertisement behavior.

To connect, select the LAN entry or add the server by its address and signaling port in Bedrock. If Login is successfully parsed, files appear in `skins/` next to the executable, for example `skins/Steve_Standard_Steve.png` and `skins/Steve_Standard_Steve.json`. In mode `2`, duplicates receive suffixes such as `_1`. The client is disconnected after capture.

## Troubleshooting

- **Port in use:** `port` is a **TCP** listener, while UDP `7551` is the LAN discovery listener. Both must be free. On Windows, check `Get-NetTCPConnection -LocalPort 19132` and `Get-NetUDPEndpoint -LocalPort 7551`.
- **Remote clients cannot connect:** allow TCP on the configured signaling port and UDP for LAN discovery (if needed). WebRTC media uses additional UDP sockets allocated by the NetherNet library; forwarding only TCP `19132` is not a guarantee of internet connectivity. LAN connections are the simplest first test.
- **`Sending NetworkSettings` but no saved skin:** this log only confirms the response was submitted to the transport. Run with `--logs 3` or `--logs 4` and check for batch decompression warnings, Login parsing, and `Detected Player`. Transport connection alone does not mean a skin has been captured.
- **`Unknown Bedrock protocol`:** `src/bedrock/version.rs` contains a display-name lookup, not a complete version-specific packet implementation. Unknown versions fall back to a display name; this does not ensure Login compatibility.
- **No files:** check `savemode`, confirm a successful Login, and check the `skins/` directory beside the executable rather than the shell's working directory.

## Project layout

| Path | Role |
|------|------|
| `src/main.rs` | CLI/config startup, NetherNet listeners, Bedrock session handling |
| `src/bedrock/` | Batch compression, Login parsing, response packets, PNG/JSON saving, version lookup |
| `src/crypto/jwt.rs` | JWT payload decoding (not signature verification) |
| `src/util/` | Config, logger, binary buffer and legacy STUN helper |
| `src/raknet/`, `src/network/` | Retained legacy RakNet/UDP code; not used by the current binary |

## References

- [NetherNet Rust implementation](https://github.com/bedrock-crustaceans/nethernet)
- [NetherNet specification](https://github.com/df-mc/nethernet-spec)
- [Mojang Bedrock protocol documentation](https://github.com/Mojang/bedrock-protocol-docs)

## License

[MIT License](LICENSE)
