# Tauri 機能パリティ調査(2026-07-11)

> `apps/desktop-wasm`(Rust→WebAssembly)がTauriに依存せず提供している/
> 提供していない機能を、本家Tauriの公開ドキュメント・リポジトリと照合した記録。
> 調査対象:
> - <https://v2.tauri.app/>(公式サイト)
> - <https://github.com/tauri-apps/tauri>(本体リポジトリ)

## 1. Tauriの主な機能

| カテゴリ | 内容 |
|---------|------|
| IPC | フロントエンド(JS)↔バックエンド(Rust)間の`invoke()`コマンド呼び出し |
| アプリバンドラー | `.app`/`.dmg`/`.deb`/`.rpm`/`.AppImage`/`.exe`(NSIS)/`.msi`(WiX)へのパッケージング |
| 自動アップデーター | 組み込みの自己更新機構 |
| システムトレイ | ネイティブトレイアイコン |
| ネイティブ通知 | OSのネイティブ通知システム連携 |
| ネイティブWebView | localhostサーバーではなくOS標準WebView経由でコンテンツ配信 |
| クロスプラットフォーム | Windows/macOS/Linux/iOS/Android を単一コードベースで |
| 小さいバイナリ | OS標準WebView活用で600KB程度から |
| セキュリティ | Capabilities/Permissionsモデル |

## 2. apps/desktop-wasmとの対応表

| Tauri機能 | apps/desktop-wasm対応 | 状態 |
|-----------|----------------------|------|
| IPC (`invoke()`) | `api.rs`の`fetch()`ベースの素の非同期関数 | ✅ 代替実装済み(同一オリジンHTTP、IPCブリッジ不要) |
| クロスプラットフォーム | ブラウザで動くWebAssembly(OS非依存) | ✅ 実質的に全OS対応(ブラウザがあれば動く) |
| 小さいバイナリ | `opt-level = "s"` + `lto = true`でリリースビルド最適化 | 🔶 部分対応(ネイティブバイナリではなくWASMなので単純比較不可) |
| **アプリバンドラー(インストーラー)** | PWA manifest(`manifest.json`)+`apps/desktop-tray/installer`(Inno Setup、実.exeインストーラー) | ✅ 完了(2026-07-12)。PWAインストールに加え、`apps/desktop-tray`向けの真のネイティブWindowsインストーラー(`open-runo-tray-setup.exe`)を追加。実機で`/VERYSILENT`インストール→`%LOCALAPPDATA%\Programs\open-runo-tray\`への配置→`HKCU`アンインストールエントリ登録(名前/バージョン/発行者/アンインストール文字列すべて正しい)→アンインストーラーでの完全削除まで確認済み |
| **システムトレイ** | `apps/desktop-tray`(別バイナリ、`tray-icon`+`tao`、tauriパッケージ非依存) | ✅ 完了(2026-07-12)。実Windows環境でトレイアイコン表示(手書き32x32 RGBAアイコン)・左クリックで既定ブラウザが正しいURLで起動(`firefox.exe -osint -url http://localhost:8080/`をプロセス一覧で確認)・右クリックメニュー(Open/Quit)表示・Quitでプロセスが正常終了、をすべて実機検証済み |
| **ネイティブ通知** | `apps/desktop-wasm/src/notifications.rs`(Web Notifications API)+ `apps/desktop-tray`(`notify-rust`、起動時に真のOSネイティブ通知) | ✅ 完了(2026-07-12)。ブラウザ内(Web Notifications API)とトレイ常駐プロセス(`notify-rust`、Windows toast/macOS Notification Center/Linux desktop通知)の二重対応。バックアップ完了・キャッシュ全パージ完了・整合性チェック完了(成功/失敗いずれも)でOSネイティブ通知を発火。権限未許可時は既存のページ内ステータス表示のみにフォールバックし失敗しない |
| **自動アップデーター** | サーバー側バイナリを更新すれば`GET /`で常に最新UIが配信される | ✅ 実質的に自動(クライアント側の更新操作が不要という点でTauriより単純) |
| セキュリティ(Capabilities/Permissions) | サーバー側の認証(KeyGuardian、自動発行/失効)+ブラウザのオリジン分離 | 🔶 異なるモデルだが同等の目的を達成 |

## 3. 優先度付きギャップ一覧

| 項目 | 優先度 | 理由 |
|------|--------|------|
| ~~Web Notifications API連携~~ | ★☆☆ | ✅ 完了(2026-07-12) |
| ~~システムトレイ相当~~ | ★★☆ | ✅ 完了(2026-07-12、ユーザー指示により方針転換——「対応不可」を理由に見送らず、`tauri`パッケージには依存しない別のネイティブ常駐バイナリ`apps/desktop-tray`で実現) |
| ~~ネイティブインストーラー(.exe)~~ | ★★☆ | ✅ 完了(2026-07-12、Windows)。WiX Toolset v7+は商用「Open Source Maintenance Fee」EULAへの同意が必要なため採用せず、無償のInno Setupを使用(`apps/desktop-tray/installer/`)。macOS(.dmg)/Linux(.deb/.AppImage)は未着手(3節参照) |
| macOS(.dmg)/Linux(.deb/.AppImage)インストーラー | ★☆☆ | ❌ 未着手。`apps/desktop-tray`はクロスプラットフォーム対応のクレート(`tray-icon`/`tao`/`notify-rust`)で書かれているためコード自体はビルド可能なはずだが、この開発環境はWindowsのみのため他OSでの実機ビルド・パッケージング検証ができない。次回以降、該当OS環境で着手・検証すること |

## 4. 結論(2026-07-12 更新)

TauriのIPC・クロスプラットフォーム性・「単一コードベースでアプリらしく使える」
という核心価値は、fetch()ベースの直接通信 + PWA manifestによる
インストール可能性で**実用上ほぼ同等の体験**を実現している。ネイティブ通知・
システムトレイ・(Windows向け)ネイティブインストーラーはいずれも
2026-07-12に実装完了。「ブラウザ実行という制約で対応不可」という従来の
結論を撤回し(2026-07-12、ユーザー指示)、`tauri`パッケージには依存しない
軽量なネイティブ常駐バイナリ(`apps/desktop-tray`、`tray-icon`+`tao`+
`notify-rust`)を追加することで実現した。WASM本体(ブラウザ実行)はそのまま
メインUIとして維持し、`apps/desktop-tray`はトレイアイコン・ネイティブ通知・
「ブラウザでUIを開く」ショートカット・実インストーラーのみを提供する薄い
補助プロセスである。実Windows環境での実機検証(トレイアイコン表示・
クリックでのブラウザ起動・メニュー・インストール/アンインストール)は
すべて完了。残るのはmacOS/Linux向けパッケージングのみ(このWindows専用
開発環境では検証不可、次回以降の課題)。
