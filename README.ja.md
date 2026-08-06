<div align="center">

<img src="res/app.ico" width="80" alt="SkinGetBE Logo" />

# SkinGetBE - Rust Edition

**Minecraft Bedrock Edition プレイヤーのスキンを、RakNet + Bedrock プロトコルの自前実装で自動取得する高性能 Rust ツール**

[![Rust Edition](https://img.shields.io/badge/Edition-Rust-orange?style=flat-square&logo=rust)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow?style=flat-square)](LICENSE)
[![Async Runtime](https://img.shields.io/badge/Runtime-Tokio-blue?style=flat-square)](https://tokio.rs/)
[![Platform](https://img.shields.io/badge/Platform-Win%20%7C%20Linux%20%7C%20macOS-lightgrey?style=flat-square)](#ビルド方法)
[![Protocol](https://img.shields.io/badge/Bedrock%20Protocol-Dynamic-green?style=flat-square)](#設定)

[English](README.md) | **日本語**

</div>

---

## 概要

**SkinGetBE** は、Minecraft Bedrock Edition（BE）の通信スタックを一から実装しています。生の UDP ソケット → RakNet ハンドシェイク → Bedrock Login パケット解析までを処理し、接続してきたクライアントのスキン画像を PNG と JSON メタデータ（サイドカー）のペアで自動保存します。

Rust 版の特徴：
- **パフォーマンス**: Tokio の async/await に加え、バックグラウンドの再送・輻輳制御ワーカーが信頼性保証フレームの再送を処理
- **メモリ安全性**: Rust の所有権システムと型システムにより、バッファオーバーフローや use-after-free を根本的に排除
- **クロスプラットフォーム**: Windows・Linux・macOS を単一コードベースでビルド可能
- **保守性**: `bedrock`・`raknet`・`network`・`crypto`・`util` に分割されたモジュール設計

> ⚠️ **注意**: 本ツールは非公式かつ研究・技術検証目的のものです。完全なゲームサーバーではなく、商用利用・公開サーバーへの展開は推奨しません。利用は自己責任でお願いします。

---

## 機能

### ✅ 実装済み

| カテゴリ | 機能 |
|----------|------|
| **RakNet** | Unconnected Ping/Pong（バージョン文字列付き MOTD） |
| | Open Connection Request/Reply 1 & 2 |
| | Connection Request / Connection Request Accepted |
| | Connected Ping/Pong |
| | Frame Set パケット解析・信頼性保証フレームの分割 |
| | ACK/NAK 処理、再送・輻輳制御 |
| **Bedrock** | `Login` パケット受信・解析（複数フォーマット対応、新しめのプレビュー版プロトコルも対応） |
| | zlib raw deflate 解凍 |
| | JWT トークン解析（チェーンデータ抽出） |
| | Skin データ デコード（Base64 → RGBA） |
| | クライアントへの `NetworkSettings` 応答送信 |
| **スキン抽出** | JWT チェーンからプレイヤー名取得（xname / ThirdPartyName / displayName） |
| | Skin データ（Base64 RGBA）抽出 |
| | 画像サイズ自動判定（64×32, 64×64, 128×64, 128×128） |
| | `skins/` への PNG 生成・出力 |
| | JSON メタデータ サイドカー（`{player}_{skinId}.json`） |
| | 保存モード: 無効 / 上書き / 重複時の自動連番 |
| **インフラ** | Tokio 非同期ランタイムによる並行接続処理 |
| | クライアントごとのセッション状態管理 |
| | STUN による外部 IP 発見 |
| | C++ スタイルのカラー出力ログ（tracing フレームワーク） |
| | プロトコル→バージョン対応表＋未知/新規プロトコルの自動フォールバック |
| | `config.jsonc`（コメント対応 JSONC）による設定 |
| | clap ベース CLI、Windows 形式 `/flag` 引数対応 |
| **ビルド** | Cargo でのクロスプラットフォームビルド |
| | Windows アイコン埋め込み（`build.rs` + `winres`） |

### 🔲 未実装（予定）

| 機能 | 備考 |
|------|------|
| 暗号化通信対応 | Xbox Live オンラインセッションに必要 |
| Cape（マント）画像抽出 | 追加の Bedrock パケット処理が必要 |
| Geometry JSON 保存 | スキン形状・アニメーションデータ |
| 完全なゲームサーバー機能 | スキン抽出のみ。取得後はクライアントを切断 |

---

## 動作の仕組み

```
[BE クライアント 接続]
        │
        ▼
[RakNet ハンドシェイク]
  Unconnected Ping → Pong (MOTD + バージョン)
  OCR1 → OCReply1
  OCR2 → OCReply2
  ConnectionRequest → ConnectionRequestAccepted
  （信頼性保証フレーム、ACK/NAK + 再送）
        │
        ▼
[Bedrock レイヤー]
  NetworkSettings 要求 → NetworkSettings 応答（zlib）
  Login パケット受信（zlib 圧縮）
  Chain Data (JWT) → プレイヤー名 + スキンデータ
  Base64 RGBA → PNG → skins/{player}_{skinId}.png
  → skins/{player}_{skinId}.json（メタデータ サイドカー）
        │
        ▼
[接続クリーンアップ]
  クライアント切断 / セッション終了
```

---

## プロジェクト構成

```
src/
├── main.rs               # バイナリのエントリーポイント（CLI、設定、STUN、サーバーループ）
├── lib.rs                # ライブラリルート＆公開モジュール
├── cli.rs                # clap CLI 定義（--logs、--filter、--debug など）
├── error.rs              # エラー型＆Result エイリアス
├── bedrock/              # Minecraft Bedrock プロトコル
│   ├── mod.rs           # 再エクスポート＆最新バージョン/プロトコル参照
│   ├── version.rs       # プロトコル↔バージョン対応表＆未知プロトコルのフォールバック
│   ├── login.rs         # Login パケット解析＆JWT チェーン/スキン抽出
│   ├── skin.rs          # スキンデコード、PNG エンコード、保存処理（savemode）
│   ├── batch.rs         # バッチパケット処理
│   └── responses.rs     # 送信応答（NetworkSettings など）
├── raknet/               # RakNet プロトコル実装
│   ├── mod.rs           # RakNet 構造体＆設定
│   ├── constants.rs     # パケット ID＆定数
│   ├── protocol.rs      # フレーム/データグラム型
│   └── server/          # RakNet サーバー
│       ├── mod.rs       # サーバー状態、パケットルーティング、再送ワーカー
│       ├── session.rs   # クライアントごとのセッション状態
│       ├── handshake.rs # Unconnected PING/PONG、OCR1/2 応答
│       ├── frames.rs    # フレームセットの符号化/解析、信頼性、分割
│       └── bedrock.rs   # Bedrock パケット処理（login → スキン保存）
├── crypto/               # 暗号化ユーティリティ
│   ├── mod.rs
│   └── jwt.rs           # JWT 解析＆Base64 デコード
├── network/              # ネットワーク抽象化
│   ├── mod.rs           # ネットワーク設定＆ラッパー
│   └── udp.rs           # UDP ソケット（async Tokio）
└── util/                 # ユーティリティ
    ├── mod.rs
    ├── buffer.rs        # バイナリバッファ（リトル/ビッグエンディアン）
    ├── config.rs        # config.jsonc の読み書き＆JSONC コメント除去
    ├── logger.rs        # C++ スタイルのカラー出力フォーマッタ
    └── stun.rs          # STUN 外部 IP 発見

build.rs                 # Windows アイコン埋め込み
res/
└── app.ico              # Windows アプリケーションアイコン
```

---

## ビルド方法

### 必要なもの

- **Rust**: 1.70 以上（[rustup.rs](https://rustup.rs) からインストール）
- **Cargo**: Rust に付属

### Windows / Linux / macOS

```bash
cargo build --release
```

- Windows: `target/release/skingetbe.exe`（アイコン埋め込み済み）
- Linux/macOS: `target/release/skingetbe`

`windows` と `winres` の依存は **Windows のみ**で使用されます。他のプラットフォームではビルドに含まれません。

### ビルドオプション

```bash
# デバッグビルド（コンパイル高速、実行は遅い）
cargo build

# リリースビルド（最適化、バイナリ小サイズ）
cargo build --release
```

---

## 使い方

```
Usage: skingetbe [OPTIONS]

Options:
  -c, --config          config.jsonc からバージョン/プロトコルを読み込む
                        （デフォルト。CLI 互換用に保持）
      --filter <NAME>   プレイヤー名でフィルタ（部分一致）
      --logs [<level>]  ログレベル: 0=error, 1=warn, 2=info, 3=debug, 4=trace。
                        値なしの場合は debug（3）になる
  -d, --debug           --logs 3 の後方互換エイリアス
  -h, --help            ヘルプを表示
  -V, --version         バージョンを表示
```

Windows 形式の `/flag` 引数も使用できます（例: `/help`、`/logs 3`）。

### サーバー起動

```bash
# デフォルト設定で起動（info レベルのログ）
./skingetbe
# または Windows
skingetbe.exe

# デバッグ出力
./skingetbe --logs
./skingetbe --logs 3
./skingetbe --debug        # --logs 3 と同じ

# フルトレース出力
./skingetbe --logs 4

# エラーのみ出力
./skingetbe --logs 0
```

デフォルトのログレベルは `info`（2）です。

### 初回起動時

初回実行時に `config.jsonc` が自動生成されます（**バイナリと同じディレクトリに生成**）：

```jsonc
{
  // サーバーリストに表示される Minecraft Bedrock のバージョン文字列。
  "version": "1.26.21",
  // Bedrock のプロトコル番号。クライアントのバージョンに合わせる。
  "protocol": 975,
  // UDP リスンポート。
  "port": 19132,
  // バインドアドレス。0.0.0.0 は全ネットワークインターフェースで待ち受け。
  "bind_addr": "0.0.0.0",
  // スキン保存モード: 0 = 保存しない, 1 = 上書き, 2 = 連番ファイルとして保存。
  "savemode": 2
}
```

**生成先の例**:
- Windows: `C:\path\to\skingetbe.exe` → `C:\path\to\config.jsonc`
- Linux: `/usr/local/bin/skingetbe` → `/usr/local/bin/config.jsonc`

### 設定

| 設定項目 | 型 | デフォルト | 用途 |
|----------|-----|-----------|------|
| `port` | int | 19132 | UDP リスンポート（到達可能である必要あり） |
| `bind_addr` | string | "0.0.0.0" | バインドアドレス（0.0.0.0 = 全インターフェース） |
| `protocol` | int | 975 | Bedrock プロトコル番号 |
| `version` | string | "1.26.21" | サーバーリスト / MOTD に表示するバージョン文字列 |
| `savemode` | int | 2 | `0`=保存しない、`1`=上書き、`2`=重複時は連番 |

JSONC コメントに対応しています。`protocol`/`version` が欠落・不正な場合は、自動的に最新対応バージョンで補完・正規化されます。

### Minecraft クライアント側の操作

1. BE クライアントを起動し、**Play → サーバータブ**（または **LAN**）を開く
2. `127.0.0.1`（または SkinGetBE を動かしているマシンの IP）に接続
3. 接続すると自動的にスキンが取得・保存され、クライアントは切断される
4. `skins/` ディレクトリに PNG ファイルが保存されている

### 出力結果

スキンは `skins/` ディレクトリに PNG + JSON メタデータのペアで保存されます：

```
skins/
├── Steve_Standard_Steve.png
├── Steve_Standard_Steve.json     ← メタデータ サイドカー
├── Alex_CustomSkinId.png
└── Player_AnotherSkin_1.png      ← 重複時は連番（savemode 2）
```

### ロギング

カラー出力のログ形式は元の C++ ツールと同じです：`[HH:MM:SS] [LEVEL] message`

| レベル | `--logs` 値 | 出力内容 |
|--------|-------------|----------|
| error | 0 | エラーのみ |
| warn | 1 | + 警告（未知プロトコルのフォールバックなど） |
| info | 2（デフォルト） | 起動状態、スキン保存メッセージ |
| debug | 3 | パケットレベルの詳細 |
| trace | 4 | フレーム全体・低レベルトレース |

---

## 設定

### Bedrock バージョンとプロトコル番号

`config.jsonc` の `protocol` はクライアントのバージョンに合わせてください。完全な対応表は `src/bedrock/version.rs` にあり、0.14.3（プロトコル 70）から最新版まで網羅しています：

| Bedrock バージョン | プロトコル | 備考 |
|-------------------|------------|------|
| 1.20.0–1.21.0    | 589–685    | 1.20 系 |
| 1.21.2–1.21.50   | 686–766    | 1.21 系 |
| 1.21.60–1.21.124 | 776–860    | 1.21 系 |
| 1.21.130         | 898        | |
| 1.26.0           | 924        | |
| 1.26.10          | 944        | |
| 1.26.21          | 975        | デフォルト / 最新既知 |

対応表にない、または新しいプロトコル（プレビュー版など）は拒否されません。警告（`Unknown Bedrock protocol <N> - falling back to ...`）をログに出力し、最新既知のバージョンで処理を継続します。

---

## ネットワークの注意点

- UDP で待ち受けます（デフォルトはポート **19132**）。ファイアウォールでポートを開放し、インターネット越しに接続する場合はルーターで転送設定を行ってください。
- 起動時に **STUN** リクエストで外部 IP を発見し、RakNet ハンドシェイクに含めます。
- 暗号化はネゴシエーションしないため、**オフラインモード**のクライアントのみ接続できます。正規のオンライン Bedrock サーバーとしては機能しません。研究・検証目的のみで使用してください。

---

## 依存ライブラリ

| Crate | 用途 |
|-------|------|
| **tokio** | 非同期ランタイム |
| **clap** | CLI 解析（derive） |
| **serde** / **serde_json** | 設定・メタデータのシリアライズ |
| **base64** | スキンデータのデコード |
| **flate2** | zlib 圧縮 / 解凍 |
| **crc32fast** | PNG チャンク CRC（PNG エンコードは自前実装） |
| **tracing** / **tracing-subscriber** | 構造化・カラー出力ログ |
| **chrono** | ログフォーマッタのタイムスタンプ |
| **once_cell** | Lazy 静的変数（プロトコル対応表） |
| **rand** | GUID 生成 |
| **thiserror** / **anyhow** | エラーハンドリング |
| **windows** | コンソール仮想ターミナル対応（Windows のみ） |
| **winres** | ビルド時のアイコン埋め込み（Windows のみ） |

---

## トラブルシューティング

### ポートがすでに使用中

**エラー**: `Address already in use`

**解決策**:
1. `config.jsonc` の `port` を変更する
2. または競合するプロセスを停止：
   ```bash
   # Windows PowerShell
   Get-NetTCPConnection -LocalPort 19132

   # Linux
   lsof -i :19132
   netstat -tlnp | grep 19132
   ```

### クライアントが接続できない

**確認事項**:
1. config の `port` が正しく、サーバーが待ち受け状態であること
2. ファイアウォールが該当 UDP ポートへの受信を許可しているか
3. まず同一マシンからの接続（`127.0.0.1`）をテスト
4. `--logs 3` でハンドシェイクパケットを確認

### "Unknown Bedrock protocol" の警告

クライアントのプロトコル番号が対応表にありません。サーバーは自動的に最新既知バージョン（1.26.21 / プロトコル 975）へフォールバックして処理を継続します。別のバージョンを対象にしたい場合は `config.jsonc` の `protocol` を変更してください。

### スキンが保存されない

`config.jsonc` の `savemode` を確認：
- `0` は保存を完全に無効化
- `1` は既存ファイルを上書き
- `2` は連番ファイルを作成（デフォルト）

また、クライアントが実際に Login 段階まで到達しているか、`--logs 3` の出力を確認してください。

---

## 開発

```bash
# デバッグモード + フルロギングで実行
cargo run -- --logs 3

# テスト実行（ライブラリ + CLI）
cargo test

# コード整形
cargo fmt

# 静的解析
cargo clippy
```

---

## 参考資料

- [Mojang/bedrock-protocol-docs](https://github.com/Mojang/bedrock-protocol-docs)
- [PrismarineJS/bedrock-protocol](https://github.com/PrismarineJS/bedrock-protocol)
- [Sandertv/go-raknet](https://github.com/Sandertv/go-raknet)
- [RakNet Documentation](https://github.com/facebookarchive/RakNet)
- [wiki.vg/Bedrock Protocol](https://minecraft.wiki/w/Bedrock_Edition_protocol)
- [Tokio Guide](https://tokio.rs/)

---

## ライセンス

[MIT License](LICENSE)

---

## 貢献

貢献歓迎です。検討中の機能：

- テストスイート拡張（パケットレベルのフィクスチャ）
- 新 Bedrock リリースへのプロトコル対応表更新
- Cape / Geometry 抽出
- 暗号化通信対応
- パフォーマンス最適化

PR の際は説明と、可能であればベンチマーク比較をお願いします。
