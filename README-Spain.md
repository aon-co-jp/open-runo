# open-runo

**Plataforma de GraphQL Federation / framework web construido con Rust + Poem**
— Funciones del plan de pago de WunderGraph Cosmo, ahora como OSS.
Incluye IA de autoaprendizaje propia (sin contratos con LLM externos).

📖 Otros idiomas: [日本語](README-Japan.md) / [English](README-English.md) ·
Para integrarlo en otros proyectos, consulta **[PORTING.md](PORTING.md)**.

## ¿Qué es open-runo?

El crecimiento de microservicios multiplica las APIs REST (infierno BFF,
explosión de versiones `/v1 /v2`, gestión de endpoints descontrolada).
open-runo resuelve esto de raíz con **GraphQL Federation + VersionlessAPI**.
Las funciones que WunderGraph Cosmo (Go) solo ofrece en planes de pago
(Launch/Scale/Enterprise) están implementadas aquí **en Rust puro y gratis, como OSS**.

## Comparativa de funciones

| Función | Cosmo gratis | Cosmo de pago | **open-runo** |
|---|:---:|:---:|:---:|
| GraphQL Federation / Schema Registry | ✅ | ✅ | ✅ |
| Persisted Queries | — | ✅ | ✅ **gratis** |
| RBAC de grano fino | — | ✅ | ✅ **gratis** |
| SSO (OIDC / JWKS RS256) | — | ✅ | ✅ **gratis** |
| Aprovisionamiento SCIM 2.0 | — | ✅ | ✅ **gratis** |
| Registro de auditoría | — | ✅ | ✅ **gratis** |
| Límites de solicitudes/equipo/retención | Sí | Reducidos | **Ninguno** |

### Funciones exclusivas de open-runo

- 🧠 IA de autoaprendizaje (sin costes de LLM externo) — caché de HTML adaptativo, TTL dinámico
- 🔑 KeyGuardian — gestión totalmente automática de claves API (emisión/revocación vía SCIM)
- 🗄️ DUAL DATABASE — PostgreSQL + aruaru-db en espejo, verificación y reparación automática
- 📦 Migración y restauración sencillas — todos los datos + estado de IA en un JSON portátil
- 🔀 Conversión de motores e integración distribuida — MySQL→PostgreSQL→CockroachDB con un comando
- ⚡ VersionlessAPI — evita crear `/v1 /v2` mediante un motor de reglas de compatibilidad
- 🖥️ App de escritorio Tauri 2 (TypeScript + Bootstrap 5)

## Inicio rápido

```bash
git clone https://github.com/aon-co-jp/open-runo
cd open-runo
cargo test --workspace          # 192 tests
cargo run -p open-runo-gateway  # servidor REST + GraphQL
```

## Estructura del workspace (15 crates)

Compuesto por `open-runo-router` (gateway REST/auth/auditoría),
`open-runo-gateway` (endpoint GraphQL), `open-runo-federation` (composición de esquemas),
`open-runo-db` (abstracción multi-motor), etc. Ver [docs/architecture.md](docs/architecture.md).

## Licencia

Apache-2.0 OR MIT (a elección). Para contribuir, ver [CONTRIBUTING.md](CONTRIBUTING.md).
