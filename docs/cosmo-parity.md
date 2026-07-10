# Cosmo 互換ロードマップ — open-runo を GraphQL ファーストへ

> **仕様変更日**: 2026-07-03
> **方針**: WunderGraph Cosmo（Apache 2.0 / Go 製）の無償版 + **有料版（Launch / Scale / Enterprise）機能**を
> Pure Rust + Poem で互換実装し、GitHub 上のオープンソース・フレームワーク
> （GraphQL Federation プラットフォーム）として提供する。
> 自前サーバー / AWS / VPS / クラウドいずれでも同一バイナリで動作すること。

参考: <https://wundergraph.com/cosmo> / <https://cosmo-docs.wundergraph.com/overview> / <https://graphql.org/>

---

## 1. 基本方針の変更点

| 項目 | 旧方針 | 新方針 |
|------|--------|--------|
| API の主軸 | REST (VersionlessAPI) が主、GraphQL は追加 | **GraphQL (Federation) が主軸**。REST は管理・互換 API として維持 |
| 互換ターゲット | Cosmo OSS 相当 | **Cosmo 有料版（Launch/Scale/Enterprise）機能まで互換** |
| 提供形態 | 未定 | GitHub で全機能 OSS 公開（機能制限なし・Apache 2.0 OR MIT） |
| 収益版との差 | — | open-runo では**全機能を無償開放**（リクエスト数制限・チーム人数制限・データ保持制限を設けない） |

Cosmo が有料プランでのみ解放している機能を、open-runo はすべて OSS で提供する。
これが open-runo の最大の差別化ポイントとなる。

---

## 2. Cosmo 機能 → open-runo クレート対応表

### 2.1 無償版 (Developer/OSS) 相当 — 既存実装

| Cosmo 機能 | open-runo 対応 | 状態 |
|-----------|----------------|------|
| Cosmo Router（クエリエンジン） | `open-runo-router` + `open-runo-gateway` (POST /graphql) | ✅ 実装済み |
| Schema Registry | `open-runo-schema-registry` (+ REST /api/schemas) | ✅ 実装済み |
| Federation 合成・破壊的変更検出 | `open-runo-federation` | ✅ 実装済み（SDL 完全対応は今後） |
| Cosmo Studio（ダッシュボード） | `apps/desktop-wasm` (Rust→WebAssembly) → 将来 aruaru-web 版 | ✅ 基本 5 ページ |
| OTEL テレメトリ | `open-runo-observability` | 🔶 tracing のみ。OTLP export 拡張予定 |

### 2.2 有料版 (Launch / Scale) 相当 — 次の実装ターゲット

| Cosmo 有料機能 | open-runo 実装先 | 優先度 | 内容 |
|---------------|-----------------|--------|------|
| **Persisted Queries / Trusted Documents** | `open-runo-persisted-queries` + gateway 統合 | ✅ 実装済み | SHA-256 登録・APQ 互換。`/graphql` が `OPEN_RUNO_PQ_MODE=disabled\|allow\|enforce` で動作。REST: `/api/persisted-queries` |
| **細粒度レートリミット** | `open-runo-security::TokenBucketLimiter` | ✅ 実装済み | per-key トークンバケット + `with_override`。ルート単位のミドルウェア化は今後 |
| **キャッシュ制御 (Response / Entity Cache)** | `open-runo-cache` + gateway 統合 | ✅ 実装済み | `OPEN_RUNO_CACHE=on` で operation 単位の TTL レスポンスキャッシュ。`redis-backend` feature で Redis 共有。per-field `@cacheControl` は今後 |
| **月間リクエスト数の計測**（制限はしない） | `open-runo-observability` + ClickHouse | ★★☆ | 課金目的ではなく運用メトリクスとして計測のみ |
| **Analytics / Tracing (Studio 相当)** | ClickHouse :8123 + aruaru-web ダッシュボード | ★★☆ | オペレーション別レイテンシ・エラー率 |

### 2.3 Enterprise 相当 — 中期実装ターゲット

| Cosmo Enterprise 機能 | open-runo 実装先 | 優先度 | 内容 |
|----------------------|-----------------|--------|------|
| **SSO (OIDC)** | `open-runo-security::oidc` + `ApiKeyAuth::with_oidc` | ✅ 実装済み | JWKS/RS256 検証（kid・iss・aud・exp）。env: `OPEN_RUNO_OIDC_ISSUER` / `OPEN_RUNO_OIDC_JWKS_FILE`。Discovery 自動フェッチは今後 |
| **厳密な RBAC** | `open-runo-security::rbac` + `ApiKeyAuth::with_rbac` | ✅ 実装済み | `OPEN_RUNO_RBAC=enforce` で JWT roles をルート単位に認可（403）。admin/developer/viewer 組み込み |
| **SCIM (ユーザープロビジョニング)** | `open-runo-scim` + `/scim/v2/Users` `/scim/v2/Groups` | ✅ 実装済み | RFC 7643/7644 Users + Groups CRUD、`OPEN_RUNO_SCIM_TOKEN`（IdP 用 Bearer） |
| **監査ログ** | `open-runo-router::audit` + `audit_log` テーブル | ✅ 実装済み | schema / db / persisted-query / SCIM の全変更を actor 付きで aruaru-db (Git-on-SQL) に記録 |
| **マルチグラフ / namespace** | `SchemaRegistry::register_in` ほか + REST `?namespace=` | ✅ 実装済み | namespace ごとに独立したグラフ。既存 API は `default` namespace で完全互換 |
| **SOC2/HIPAA 対応基盤** | docs/security.md 拡充 | ★☆☆ | 暗号化・保持ポリシー・アクセス制御の文書化（認証取得は範囲外） |
| GraphQL Subscriptions (WS/SSE) | `SubscriptionRoot::schema_events` + `GET /graphql/ws` | ✅ 実装済み | graphql-ws プロトコル。broadcast ブローカーでスキーマ変更を配信 |

---

## 3. 実装フェーズ

1. **Phase A（次セッション）**: `open-runo-persisted-queries` クレート新設、
   `open-runo-security` のレートリミット拡張（per-key トークンバケット）、RBAC マトリクス設計。
2. **Phase B**: OIDC SSO（JWKS 検証）、監査ログ配線、`@cacheControl` + Redis キャッシュ。
3. **Phase C**: SCIM 2.0、マルチグラフ/namespace、GraphQL Subscriptions、
   ClickHouse アナリティクス + aruaru-web ダッシュボード統合。

すべて feature フラグで無効化可能にし、最小構成（シングルバイナリ + SQLite +
`DualBackend::single`）でも VPS 1 台で動くことを維持する。

---

## 4. デプロイ形態（すべて同一バイナリ）

| 形態 | 構成 |
|------|------|
| 自前サーバー / VPS (ConoHa 等) | `open-runo-gateway` バイナリ + SQLite or PostgreSQL、systemd |
| AWS / クラウド | 同バイナリ + RDS/ElastiCache、Docker (`Dockerfile` 済) |
| フルスタック | DUAL DATABASE (PostgreSQL + aruaru-db) + Redis + ClickHouse (`--features full`) |

Cosmo と異なり「マネージド版でしか使えない機能」を作らない。
