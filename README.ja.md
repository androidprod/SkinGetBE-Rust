<div align="center">

<img src="res/app.ico" width="80" alt="SkinGetBE ロゴ" />

# SkinGetBE - Rust Edition

**NetherNet（WebRTC）経由で Minecraft Bedrock Edition のスキンを取得するツール**

[![Rust Edition](https://img.shields.io/badge/Edition-Rust-orange?style=flat-square&logo=rust)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow?style=flat-square)](LICENSE)
[![Async Runtime](https://img.shields.io/badge/Runtime-Tokio-blue?style=flat-square)](https://tokio.rs/)

[English](README.md) | **日本語**

</div>

## 概要

SkinGetBE は Minecraft Bedrock クライアントからの NetherNet 接続を受け、Login パケットからプレイヤーのスキンを取り出して PNG と JSON メタデータに保存します。**ゲームをプレイできるサーバーではありません**。Login を解析した後は PlayStatus と Disconnect を送信し、接続を閉じます。

実行バイナリの通信処理には [`nethernet-tokio`](https://github.com/bedrock-crustaceans/nethernet) を使用します。以前の RakNet 実装は `src/raknet/` に残っていますが、`src/main.rs` では起動しません。

> **研究・検証用:** 認証済みの公開サーバーとして運用しないでください。JWT のペイロードは読み取りますが、Login チェーンの署名検証やプレイヤー本人確認は行いません。WebRTC の通信暗号化と Login 認証は別の仕組みです。

## 機能と制限

- HTTP シグナリングは TCP `port`（デフォルト **19132**）、LAN 検出は UDP **7551**。
- クライアントごとの WebRTC 接続（reliable/unreliable チャネル）、NetworkSettings 応答、Login 解析、raw DEFLATE 圧縮バッチの処理。
- Login の JWT ペイロードから名前・RGBA スキンを抽出し、PNG と JSON サイドカーを保存。
- 保存モードは `0`（無効）、`1`（上書き）、`2`（重複時に連番）。
- カラー出力ログ、JSONC 設定、**26.60 / プロトコル 2225** まで登録済みのバージョン対応表。
- 完全なゲームサーバー機能、マント画像やジオメトリの個別ファイル保存は未実装。
- Xbox Live セッションの登録や Microsoft 認証済みサーバーとしての運用は未実装。シグナリング用サーバー ID は起動ごとに生成され、保存されません。

### 処理の流れ

```text
HTTP シグナリング（TCP port）または LAN 検出（UDP 7551）
  → NetherNet WebRTC 接続（ICE / DTLS / SCTP）
  → RequestNetworkSettings → 非圧縮の NetworkSettings
  → 圧縮された Bedrock Login → JWT ペイロード → RGBA スキン
  → skins/<player>_<skinId>.png と .json
  → PlayStatus / Disconnect → セッション終了
```

NetworkSettings 自体は非圧縮です。圧縮のネゴシエーション後は、Bedrock バッチの先頭に圧縮方式バイトを付けます（`0` は raw DEFLATE、受信側は非圧縮を示す `0xff` にも対応）。NetherNet への送信では RakNet の `0xfe` ヘッダーを使いません。

## ビルド

新しい安定版 Rust と Cargo をインストールして、次を実行します。

```bash
cargo build --release
cargo test --locked
```

実行ファイルは Windows では `target/release/skingetbe.exe`、Linux/macOS では `target/release/skingetbe` です。初回ビルド時に Cargo が NetherNet の依存コードを GitHub から取得します。ローカルの `vendor/` は不要です。Windows では `build.rs` でアイコンを埋め込みます。

## 使い方

```bash
cargo run -- --logs 3
# またはリリース版の実行ファイルを起動
./target/release/skingetbe --logs 3
```

Windows では `target\release\skingetbe.exe` を使います。`--logs` の値は `0` エラー、`1` 警告、`2` 情報（初期値）、`3` デバッグ、`4` トレースです。値を省略した `--logs` は `3`、`--debug` も `3` です。Windows 形式の `/flag` も使用できます。`--config` は互換性のため残っていますが、設定は指定なしでも読み込みます。**`--filter` は引数として受け付けますが、現在のスキン取得処理には反映されません。**

初回起動時、実行ファイルと**同じディレクトリ**に `config.jsonc` を生成します（`cargo run` の場合は `target/debug/`）。

```jsonc
{
  "version": "1.26.21",
  "protocol": 975,
  "port": 19132,
  "bind_addr": "0.0.0.0",
  "savemode": 2
}
```

| 項目 | 用途 |
|------|------|
| `port` | HTTP シグナリングの TCP 待受ポート（初期値 `19132`） |
| `bind_addr` | シグナリングと LAN 検出のバインドアドレス |
| `protocol`, `version` | 設定上の Bedrock プロトコルとバージョン（初期値 `975` / `1.26.21`） |
| `savemode` | `0` 保存しない、`1` 上書き、`2` 重複時に連番 |

JSONC コメントに対応しています。**プロトコル値が `0` の場合**と**バージョンが空の場合**のみ、対応表の最新値で補完します。キーを省略した場合は組み込みの初期値が使われます。値が入っていれば自動で最新版に書き換えません。**現在の LAN/HTTP サーバー広告には、設定値ではなく `src/bedrock/version.rs` の最新値が使われます。** このため、初期設定の起動ログは `975 / 1.26.21` ですが、広告は `2225 / 26.60` です。クライアントの版に合わせて `config.jsonc` を編集しても、この広告値の選び方は変わりません。

Minecraft Bedrock からは LAN 一覧、またはサーバーのアドレスとシグナリングポートを指定して接続します。Login の解析に成功すると、実行ファイル横の `skins/` に `Steve_Standard_Steve.png` と `Steve_Standard_Steve.json` のようなファイルが作られます。`savemode=2` の重複ファイルには `_1` などが付きます。取得後はクライアントを切断します。

## トラブルシューティング

- **ポートが使用中:** `port` は **TCP** の待受、UDP **7551** は LAN 検出用です。どちらも空いている必要があります。Windows では `Get-NetTCPConnection -LocalPort 19132` と `Get-NetUDPEndpoint -LocalPort 7551` で確認できます。
- **外部から接続できない:** 設定した TCP ポート、および LAN 検出が必要なら UDP 7551 の受信を許可してください。WebRTC の通信本体には NetherNet ライブラリが別途割り当てる UDP ソケットも使われるため、TCP 19132 だけを転送してもインターネット越しの接続は保証されません。まず LAN で確認してください。
- **`Sending NetworkSettings` の後に保存されない:** このログは通信層への送信処理を示すだけです。`--logs 3` または `--logs 4` でバッチ展開の警告、Login 解析、`Detected Player` の有無を確認してください。接続成立だけではスキン保存は完了していません。
- **`Unknown Bedrock protocol`:** `src/bedrock/version.rs` は表示用の対応表で、バージョン別の完全なパケット実装ではありません。未知の番号でも表示名にフォールバックしますが、Login の互換性は保証しません。
- **ファイルがない:** `savemode` を確認し、Login 成功ログと、作業ディレクトリではなく**実行ファイル横**の `skins/` を確認してください。

## 主な構成

| パス | 役割 |
|------|------|
| `src/main.rs` | CLI・設定の起動処理、NetherNet リスナー、Bedrock セッション処理 |
| `src/bedrock/` | バッチ圧縮、Login 解析、応答、PNG/JSON 保存、バージョン表 |
| `src/crypto/jwt.rs` | JWT ペイロードの読み取り（署名検証なし） |
| `src/util/` | 設定、ログ、バイナリバッファ、旧 STUN ヘルパー |
| `src/raknet/`, `src/network/` | 残存する旧 RakNet/UDP 実装。現行バイナリでは不使用 |

## 参考資料

- [NetherNet Rust 実装](https://github.com/bedrock-crustaceans/nethernet)
- [NetherNet 仕様](https://github.com/df-mc/nethernet-spec)
- [Mojang Bedrock プロトコル資料](https://github.com/Mojang/bedrock-protocol-docs)

## ライセンス

[MIT License](LICENSE)
