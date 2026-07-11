# Poem 機能パリティ調査(2026-07-11)

> hyper_compat(`crates/open-runo-router/src/hyper_compat.rs`)への移行後、
> 本家 Poem フレームワークが持つ機能に対して漏れがないかを調査した記録。
> 調査対象:
> - <https://docs.rs/poem/latest/poem/>(公式ドキュメント)
> - <https://github.com/poem-web/poem>(本体リポジトリ、周辺クレート一覧)
> - <https://zenn.dev/ouvill/articles/introduce_rust_poem_framework>(日本語入門記事)

## 1. Poem本体の主な機能

| カテゴリ | 内容 |
|---------|------|
| ルーティング | `Route`(パス)/`RouteDomain`(ドメイン)/`RouteMethod`(HTTPメソッド)/`RouteScheme`(スキーム) |
| エクストラクタ | `Query<T>`/`Path<T>`/`Json<T>`/`RemoteAddr`/`Method`/`Uri`、`FromRequest`でカスタム可 |
| レスポンス | `IntoResponse`トレイトで`String`/`StatusCode`/`Result`等を柔軟に変換 |
| エラーハンドリング | `catch_error()`、`NotFoundError`等の専用エラー型 |
| ミドルウェア | `with()`で適用、組み込み`Tracing`、`Middleware`トレイトでカスタム可 |
| WebSocket / SSE | プロトコルサポートあり(feature flag) |
| セキュリティ | Cookie・CSRF・TLS(native-tls/openssl/rustls)・ACME |
| データ処理 | Multipart・圧縮(gzip/br)・XML・YAML |
| セッション | Redisバックエンドセッション管理 |
| 監視 | OpenTelemetry・Prometheus・tokio-metrics |
| その他 | 静的ファイル埋め込み、テストユーティリティ(`TestClient`)、tower互換アダプタ、i18n(fluent) |
| 周辺クレート | `poem-openapi`(OpenAPI 3.0自動生成)・`poem-grpc`(gRPC)・`poem-lambda`(AWS Lambda)・`poem-mcpserver`(MCP Server)・`poem-worker` |

## 2. hyper_compat実装との対応表

| Poem機能 | hyper_compat対応 | 状態 |
|---------|------------------|------|
| ルーティング(パス+メソッド) | `hyper_compat::Router`(method+path、`:param`対応) | ✅ 実装済み |
| JSON抽出/レスポンス | `read_json_body`/`json_response` | ✅ 実装済み |
| クエリパラメータ | `hyper_compat::query_params` | ✅ 実装済み |
| ミドルウェア(CORS/RateLimit/Tracing) | `middleware_hyper::{with_cors, with_shared_rate_limit, with_tracing}` | ✅ 実装済み(関数コンビネータ方式) |
| SSE | `hyper_compat::sse_response` + `SseEvent` | ✅ 実装済み |
| 静的ファイル配信 | `hyper_compat::static_file_handler` | ✅ 実装済み(WASMフロントエンド配信用) |
| テストユーティリティ | `hyper_compat::serve` + `reqwest`(実HTTP経由) | ✅ 実装済み(TestClientの代替) |
| OpenAPI 3.0仕様生成 | `crates/open-runo-router/src/openapi.rs`(手書き静的JSON、`GET /api/openapi.json`) | ✅ 実装済み(2026-07-11追加。macro自動生成ではなく手書き) |
| ~~WebSocket(汎用)~~ | `hyper_compat::websocket_handler`(RFC 6455手書き実装) | ✅ 完了(2026-07-11)。`GET /api/ws-echo`(エコー)・`GET /api/ws-events`(state.eventsのWS版、認証必須)の2ルートで実証。GraphQL Subscriptions(poem版`graphql_route`)は引き続き別経路 |
| ~~gzip/br圧縮~~ | `middleware_hyper::with_compression`(gzip、`flate2`使用) | ✅ 完了(2026-07-11、gzipのみ・brは見送り。理由は3節参照) |
| Multipart(ファイルアップロード) | ― | ❌ 未実装(現状ファイルアップロードを要する機能なし) |
| Cookie/セッション管理 | ― | ❌ 未実装(X-Api-Keyヘッダのみ、Cookie不使用の設計方針) |
| CSRF保護 | ― | N/A(ブラウザセッションCookieを使わないAPI設計のため該当なし) |
| TLS/ACME | ― | N/A(リバースプロキシでのTLS終端を前提、アプリ側では未実装) |
| gRPC(poem-grpc相当) | ― | ❌ 未実装 |
| MCP Server(poem-mcpserver相当) | ― | ❌ 未実装 |

## 3. 優先度付きギャップ一覧

| 項目 | 優先度 | 理由 |
|------|--------|------|
| ~~gzip/br圧縮ミドルウェア~~ | ★★☆ | ✅ 完了(2026-07-11)。本番運用のパフォーマンス向上に直結。`GET /api/openapi.json`で実測10265→2115バイト(約79%削減)を確認 |
| ~~汎用WebSocket対応~~ | ★★☆ | ✅ 完了(2026-07-11)。RFC 6455ハンドシェイク・フレーミングを`sha1`のみでhyper_compatに手書き実装、実バイナリ+実WSクライアント(Node.js `WebSocket`)でエコーの往復を確認 |
| Multipart/ファイルアップロード | ★☆☆ | 現状のAPI設計では不要(スキーマはSDL文字列、バックアップはJSON)。将来ファイル添付機能が必要になれば実装 |
| Cookie/セッション | ★☆☆ | API-Key/JWT/OIDCベースの認証方針と方向性が異なるため、意図的に見送り |
| gRPC / MCP Server対応 | ★☆☆ | `docs/cosmo-parity.md`のCosmo側ギャップ(gRPC/MCP)と重複。将来必要になれば両方をまとめて検討 |

## 4. 結論

hyper_compatはPoemの**コア機能(ルーティング・エクストラクタ・レスポンス・
ミドルウェア・SSE・静的配信・テスト)を実用上必要十分にカバー**している。
未実装は主に「Poemのfeature flagでオプトインする周辺機能」であり、現状の
REST/GraphQL API提供という用途では致命的な欠落はない。gzip圧縮・汎用
WebSocket対応はいずれも2026-07-11に実装完了。残るギャップ
(Multipart/Cookie・セッション/gRPC・MCP Server)はいずれも★☆☆で、
現状のAPI設計・認証方針とは意図的に方向性が異なるため見送り。
