//! Poem-free auth check, extracted from `auth.rs`'s `ApiKeyAuth` (a
//! `poem::Middleware` implementation) into a plain function so
//! `hyper_compat::Handler`s can call it directly without depending on
//! poem's `Endpoint`/`Middleware` traits.
//!
//! Scope for this first pass: **`X-Api-Key` only**, mirroring the
//! `ApiKeyAuth` code path used by nearly all existing tests (KeyGuardian
//! verify → RegistryEmpty/Ok pass, everything else 401). JWT bearer, OIDC,
//! SCIM static token, and RBAC are intentionally deferred — see CLAUDE.md
//! HANDOFF for the follow-up plan; routes that need them stay on the poem
//! stack until those are ported too.

use crate::keyring::{KeyDecision, KeyGuardian};
use hyper::{HeaderMap, StatusCode};
use std::sync::Arc;

/// Checks the `X-Api-Key` header against `guardian`. Returns `Ok(())` when
/// the request may proceed, `Err(status)` otherwise (401 for missing/bad
/// key, matching `ApiKeyAuthEndpoint::call`'s behavior for the API-key path).
pub async fn check_api_key(
    headers: &HeaderMap,
    guardian: &Arc<KeyGuardian>,
) -> Result<(), StatusCode> {
    let api_key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim();

    if api_key.is_empty() {
        return Err(StatusCode::UNAUTHORIZED);
    }

    match guardian.verify(api_key, chrono::Utc::now()).await {
        KeyDecision::RegistryEmpty | KeyDecision::Ok { .. } => Ok(()),
        KeyDecision::Rejected | KeyDecision::Suspended => Err(StatusCode::UNAUTHORIZED),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyring::GuardianConfig;
    use crate::state::AppState;
    use hyper::header::HeaderValue;

    fn guardian() -> Arc<KeyGuardian> {
        let state = AppState::new();
        Arc::new(KeyGuardian::new(Arc::clone(&state.db), GuardianConfig::from_env()))
    }

    #[tokio::test]
    async fn missing_header_is_rejected() {
        let headers = HeaderMap::new();
        let result = check_api_key(&headers, &guardian()).await;
        assert_eq!(result, Err(StatusCode::UNAUTHORIZED));
    }

    #[tokio::test]
    async fn empty_header_is_rejected() {
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static(""));
        let result = check_api_key(&headers, &guardian()).await;
        assert_eq!(result, Err(StatusCode::UNAUTHORIZED));
    }

    #[tokio::test]
    async fn any_nonempty_key_passes_when_registry_is_empty() {
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_static("anything-goes"));
        let result = check_api_key(&headers, &guardian()).await;
        assert_eq!(result, Ok(()));
    }
}
