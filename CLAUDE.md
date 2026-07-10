# 技術スタック・開発ルール(open-runo)

**このリポジトリは廃止されていません。** 2026-07-10にユーザー指示により、
[`poem-cosmo-tauri`](https://github.com/aon-co-jp/poem-cosmo-tauri) と
**同時並行で開発**する方針に変更されました。両リポジトリとも
**Tauri・Poem を含めない**構成で進めます(open-runoはさらに厳密に
Tauri/Poemを一切含まない方針。poem-cosmo-tauri側は互換性維持のため
名称にPoem/Tauriを残しつつ実体はTauri/Poem非依存)。共通点: どちらも
WunderGraph Cosmo(有料版含む)をパッケージとして直接依存させず、
Rust標準ライブラリ + tokio/hyperで機能を自前実装する。
実装作業(例: crates/open-runo-routerのPoem→tokio/hyper移行)は
poem-cosmo-tauri側で先行し、動作確認が取れたファイルをこちらにも
ミラーしていく運用とする。

このリポジトリ、および関連プロジェクト(`open-web-server`/`aruaru-db`/
`aruaru-web`/`open-raid-z`)で開発・保守を行う際は、以下を基本方針とする。
作業ドライブは `F:\open-runo`(E:ドライブは2026-07-10に消失、以後Fが実体)。
この節は [`open-raid-z`](https://github.com/aon-co-jp/open-raid-z) の
`CLAUDE.md` を正本とし、各プロジェクトへコピーして同期する。

## フロントエンド(2026-07-10、方針更新)

- Tauriパッケージには直接依存しない。ただしTauriのデスクトップUI体験・
  `invoke()`的なコマンド呼び出しインターフェースとは互換性を保つ。
- **HTML5/CSS3・TypeScript・Bootstrap・Node.jsのスタックは廃止**。
  Rustをメイン言語としてフロントエンドとバックエンドを統合し、
  **WebAssembly (WASM)** に置き換える(コンパイル対象はRust →
  `wasm32-unknown-unknown`)。DOM操作・`invoke()`相当の呼び出しは
  Rust製WASMモジュール側で行い、TypeScript/Node.jsのビルドチェーンには
  依存しない。https://webassembly.org/ | https://rustwasm.github.io/
- Tauri機能パリティ調査(参照: https://v2.tauri.app/ |
  https://github.com/tauri-apps/tauri)の結果は`docs/tauri-parity.md`
  を正とする。

## バックエンド・コア

- **Rust**(メイン言語、標準ライブラリ中心): https://www.rust-lang.org/ja/ | https://github.com/rust-lang/rust
- **tokio** + **hyper**(Webフレームワークなしで直接HTTPサーバを自前実装):
  https://tokio.rs/ | https://docs.rs/hyper/latest/hyper/
- Poemパッケージには依存しないが、Poemのルーティング/ハンドラAPI形状とは
  互換性のあるインターフェースを維持しながらtokio/hyper直接実装へ移行する
  (既存ハンドラのシグネチャ・レスポンス形式を極力変えない。移行状況は
  下記HANDOFF参照)。参考資料: https://docs.rs/poem/latest/poem/ |
  https://github.com/poem-web/poem |
  https://zenn.dev/ouvill/articles/introduce_rust_poem_framework。
  機能差分の調査結果は`docs/poem-parity.md`を正とする。

## API設計思想(参考・概念のみ)

- **VersionLess API**という考え方を参考にする(WunderGraphのブログ/podcast参照)。
- **WunderGraph Cosmo**: あくまで**参考・着想元としてのみ**参照する。
  **有料版を含め実装には絶対に使用しない**。https://github.com/wundergraph/cosmo
- 「REST APIを不要にする」仕様はCosmo有料版と同一方針であることを2026-07-11に
  確認済み。機能対応表・未実装ギャップは`docs/cosmo-parity.md`(4a節)を正とする。
- **「APIキー不要」の正確な意味(2026-07-11、ユーザー確認済み)**: 認証を
  廃止するのではなく、**人間がAPIキーを意識・管理する必要をゼロにする**
  という意味。`KeyGuardian`のauto-issue/auto-revoke/auto-clean/
  auto-defendに加え、`POST /api/keys/self-issue`(認証不要でdeveloper
  role・24時間有効のキーを自動発行)とWASMフロントエンドの透過的な
  取得・キャッシュ・401時自動再発行で実現済み。

## 関連プロジェクト

- **poem-cosmo-tauri**(姉妹リポジトリ・同時並行開発。GraphQL Federation /
  API Gateway / AI-native routing platform。実装の先行地点):
  https://github.com/aon-co-jp/poem-cosmo-tauri
- **open-runo**(このリポジトリ。2026-07-10付けで開発再開・poem-cosmo-tauri
  と同時並行で開発): https://github.com/aon-co-jp/open-runo
- **open-web-server**: https://github.com/aon-co-jp/open-web-server
- **aruaru-db**: https://github.com/aon-co-jp/aruaru-db
- **aruaru-web**: https://github.com/aon-co-jp/aruaru-web
- **open-raid-z**(開発ルールの正本): https://github.com/aon-co-jp/open-raid-z
- **rs-to-readme**: https://github.com/aon-co-jp/rs-to-readme

## 運用ルール

- **開発中はこの`CLAUDE.md`を、コード変更のコミット/pushと必ず一緒に
  push する**(内容を更新した場合はもちろん、変更が無い場合も他の変更と
  一緒にコミット対象へ含めておくこと)。
- 実装で迷った場合や、API仕様の詳細確認が必要な場合は、学習データからの
  推測より公式ドキュメント(上記URL)を優先して参照する。
- 作業ドライブが変わった場合は、この節を更新し、関連プロジェクトの
  引き継ぎ資料にも変更の経緯を記録すること。

## 現状(このリポジトリ固有)

- `cargo check --workspace` / `cargo test --workspace --no-run` は成功する
  (17クレート構成 — 2026-07-11に`open-runo-cli`・`open-runo-api-types`を
  追加、テストコンパイル済み)。todo!()/unimplemented!()マーカーなし。
- README多言語版は `README-<言語>.md` 形式で日本語・英語・中国語簡体字・
  韓国語・スペイン語・フランス語・ドイツ語・イタリア語・ロシア語・
  アラビア語の10言語が揃っている。

## HANDOFF(直近の自動実行パス)

- **2026-07-11 Mirror open-runo-api-types(router/CLI/WASM共有型)を
  poem-cosmo-tauriから**: 新規`crates/open-runo-api-types`
  (17クレート目、`serde`のみ依存・I/Oなし・wasm32対応)に
  `SchemaVersion`・`RegisterSchemaRequest`・`SchemaHistoryResponse`・
  `FederationStatusResponse`を集約 — 同じ"スキーマバージョン"形状が
  router(`handlers_hyper.rs`)・`apps/desktop-wasm/src/api.rs`・
  `open-runo-cli`の3箇所で独立再定義されdriftしていた問題(登録
  レスポンスがsdl欠落、フロントエンドのhistory用structがnamespace+sdl
  欠落)を解消。3箇所すべてをこの共有crateに向けて書き換え、
  `apps/desktop-wasm`(独立workspace)には相対パス依存を追加。
  副次効果として`POST /api/schemas`のレスポンスに`sdl`が追加
  (後方互換)、Schema HistoryページにもnamespaceがUI表示されるように
  なった。詳細・実CLI+実ブラウザでの統合動作確認(CLIで登録した
  スキーマが同一UUID/タイムスタンプでブラウザのSchema Historyページに
  実際に見えることを確認)結果はpoem-cosmo-tauriの同日CLAUDE.md
  HANDOFFエントリを正とする(実装・検証はそちらで先行、このリポジトリ
  へは`crates/open-runo-api-types/` / `Cargo.toml` / `Cargo.lock` /
  `apps/desktop-wasm/{Cargo.toml,Cargo.lock,src/api.rs,src/pages.rs}` /
  `crates/open-runo-cli/{Cargo.toml,src/main.rs}` /
  `crates/open-runo-router/{Cargo.toml,src/handlers_hyper.rs}`を
  そのままコピーしてミラー)。`cargo check --workspace` /
  `cargo test --workspace`(全33テストバイナリ)ともgreenを確認。
  次回パスがすべきこと: (1) 同種のdrift問題が他のエンドポイント
  (DB CRUD・SCIM・Persisted Queries・Cache)にも無いか棚卸しし、価値が
  あれば同様に共有crateへ集約(poem-cosmo-tauriで先行実装した上で
  こちらへミラー)、(2) `docs/cosmo-parity.md`4a節の残りのギャップ
  (EDFS/Kafka連携、gRPC Connect対応、Feature Flags、MCP Server統合)
  から次の実用性向上タスクを選ぶ(ユーザー指示により確認不要で
  自動継続)。

- **2026-07-11 Mirror open-runo-cli(wgc相当CLI)をpoem-cosmo-tauriから**:
  新規`crates/open-runo-cli`(16クレート目、バイナリ`open-runo-cli`)を
  追加。`schema register/get/history`・`federation status`・
  `openapi`・`login`サブコマンド、`--api-key`省略時は自動self-issue。
  `clap`(derive+env)・`reqwest`をworkspace.dependenciesに追加。
  詳細・実バイナリでの動作確認結果はpoem-cosmo-tauriの同日CLAUDE.md
  HANDOFFエントリを正とする(実装・検証はそちらで先行、このリポジトリへ
  は`crates/open-runo-cli/` / `Cargo.toml` / `Cargo.lock` /
  `DEVELOPMENT.md` / `docs/cosmo-parity.md`をそのままコピーして
  ミラー)。`cargo check --workspace` / `cargo test --workspace`
  (全32テストバイナリ、open-runo-cli分含む)ともgreenを確認。
  次回パスがすべきこと: `docs/cosmo-parity.md`4a節の残りのギャップ
  (EDFS/Kafka連携、gRPC Connect対応、Feature Flags、MCP Server統合)
  から次の実用性向上タスクを選び、poem-cosmo-tauriで先行実装した上で
  こちらへミラーを継続(ユーザー指示により確認不要で自動継続)。

- **2026-07-11 Mirror OTLP分散トレーシングexportをpoem-cosmo-tauriから**:
  `crates/open-runo-observability`にOTLP(OpenTelemetry Protocol) HTTP
  トレースエクスポータを追加(`init_tracing_with_otlp`、
  `OPEN_RUNO_OTLP_ENDPOINT`環境変数で有効化、未設定時は従来通り
  console-onlyのJSON tracing)。ワークスペースが宣言のみで未使用だった
  `opentelemetry`/`opentelemetry-jaeger`(0.22)を実際に使う
  `opentelemetry`/`opentelemetry_sdk`/`opentelemetry-otlp`(0.32)・
  `tracing-opentelemetry`(0.33)に置き換え。`open-runo-core::Config`に
  `otlp_endpoint`フィールド追加、`open-runo-router`/`open-runo-gateway`
  両`main.rs`を新関数呼び出しに切替。ついでに`.env.example`の
  `OPENRUNO_*`(アンダースコアなし、実コードと不一致で無効だった)を
  `OPEN_RUNO_*`に修正、`open-runo-core`のテストに並列実行時の
  env変数レースコンディションがあったのを`Mutex`で修正。詳細は
  `poem-cosmo-tauri`の同日CLAUDE.md HANDOFFエントリを正とする(実装は
  そちらで先行・動作確認済み、このリポジトリへは`.env.example` /
  `Cargo.toml` / `Cargo.lock` / `DEVELOPMENT.md` /
  `crates/open-runo-core/src/lib.rs` /
  `crates/open-runo-gateway/src/main.rs` /
  `crates/open-runo-observability/{Cargo.toml,src/lib.rs}` /
  `crates/open-runo-router/src/main.rs` / `docs/cosmo-parity.md`
  をそのままコピーしてミラー)。`cargo check --workspace` /
  `cargo test --workspace`(全32テストバイナリ)ともgreenを確認。
  次回パスがすべきこと: `docs/cosmo-parity.md`4a節の残りのギャップから
  次の実用性向上タスクを選び、poem-cosmo-tauriで先行実装した上でこちらへ
  ミラーを継続(ユーザー指示により確認不要で自動継続)。

- **2026-07-10**: 定時の自律メンテナンスパス。`cargo check --workspace` /
  `cargo test --workspace --no-run` は変更前から成功済みを確認(ビルド破損なし)。
  `todo!()`/`unimplemented!()`/フェイクデータを返すスタブ関数は見つからず
  (実装は本当に完了している)。README-Japan.md と README-English.md が
  Phase A 以前の古い「ビジョン文書」のまま放置されており、実際の実装
  (15クレート・176テスト・自己学習AI・KeyGuardian・DUAL DATABASE・
  VersionlessAPI 等)と矛盾していた(例: 英語版は「設計・開発初期段階」
  「License TBD」「外部LLMプロバイダへのルーティング」と記載)ため、
  ルートの `README.md`(正しい最新情報)を基準に両ファイルを修正した:
  README-Japan.md はルート README.md の内容をそのまま反映、
  README-English.md は他8言語版と同じ構成(機能比較表・open-runo限定機能・
  クイックスタート・15クレート構成)の正確な英語版に書き換えた。
  他8言語版(中/韓/西/仏/独/伊/露/アラビア語)は内容確認済みで正確、変更不要。
  次回パスへの引き継ぎ: 特に緊急の課題は残っていない。次点候補は
  `docs/HANDOFF.md` の「次セッション候補」(Google Drive API 直接統合、
  FederatedBackend の TOML 設定化など)。
