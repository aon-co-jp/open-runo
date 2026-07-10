# open-runo

**Платформа GraphQL Federation / веб-фреймворк на Rust + Poem**
— платные функции WunderGraph Cosmo в виде открытого ПО.
Встроен собственный самообучающийся ИИ (без контрактов с внешними LLM).

📖 Другие языки: [日本語](README-Japan.md) / [English](README-English.md) ·
Для интеграции в другие проекты см. **[PORTING.md](PORTING.md)**.

## Что такое open-runo?

Рост числа микросервисов приводит к разрастанию REST API (ад BFF,
взрыв версий `/v1 /v2`, неуправляемое количество эндпоинтов).
open-runo решает эту проблему в корне с помощью **GraphQL Federation + VersionlessAPI**.
Функции, которые WunderGraph Cosmo (на Go) предлагает только в платных
тарифах (Launch/Scale/Enterprise), здесь **полностью реализованы на чистом Rust,
бесплатно, как OSS**.

## Сравнение функций

| Функция | Cosmo Free | Cosmo Paid | **open-runo** |
|---|:---:|:---:|:---:|
| GraphQL Federation / Schema Registry | ✅ | ✅ | ✅ |
| Persisted Queries | — | ✅ | ✅ **бесплатно** |
| Гранулярный RBAC | — | ✅ | ✅ **бесплатно** |
| SSO (OIDC / JWKS RS256) | — | ✅ | ✅ **бесплатно** |
| SCIM 2.0 провижининг | — | ✅ | ✅ **бесплатно** |
| Аудит-лог | — | ✅ | ✅ **бесплатно** |
| Лимиты запросов/команды/хранения | Есть | Смягчены | **Отсутствуют** |

### Только в open-runo

- 🧠 Самообучающийся ИИ (без затрат на внешние LLM) — адаптивный HTML-кэш, динамический TTL
- 🔑 KeyGuardian — полностью автоматическое управление API-ключами (выпуск/отзыв через SCIM)
- 🗄️ DUAL DATABASE — зеркалирование PostgreSQL + aruaru-db, автопроверка и автовосстановление
- 📦 Простой перенос/восстановление — все данные + состояние ИИ в одном переносимом JSON
- 🔀 Конвертация движков и распределённая интеграция — MySQL→PostgreSQL→CockroachDB одной командой
- ⚡ VersionlessAPI — не нужно создавать `/v1 /v2` благодаря движку правил совместимости
- 🖥️ Десктопное приложение на Tauri 2 (TypeScript + Bootstrap 5)

## Быстрый старт

```bash
git clone https://github.com/aon-co-jp/open-runo
cd open-runo
cargo test --workspace          # 192 тестов
cargo run -p open-runo-gateway  # сервер REST + GraphQL
```

## Структура workspace (15 крейтов)

Состоит из `open-runo-router` (REST-шлюз/аутентификация/аудит),
`open-runo-gateway` (GraphQL-эндпоинт), `open-runo-federation` (композиция схем),
`open-runo-db` (абстракция над несколькими движками БД) и др. Подробнее: [docs/architecture.md](docs/architecture.md).

## Лицензия

Apache-2.0 OR MIT (на выбор). Как внести вклад — см. [CONTRIBUTING.md](CONTRIBUTING.md).
