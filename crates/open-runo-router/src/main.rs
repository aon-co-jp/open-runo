//! open-runo Gateway Router — binary entrypoint.
//!
//! Runs on the poem-free `hyper_compat` stack (see CLAUDE.md HANDOFF for
//! the migration history). REST-only binary. For REST + GraphQL in one
//! process, run the `open-runo-gateway` binary instead.

use open_runo_core::Config;
use open_runo_router::{build_hyper_app, hyper_compat, state::AppState};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env().map_err(|e| format!("config error: {e}"))?;

    open_runo_observability::init_tracing_with_otlp(
        &config.log_level,
        config.otlp_endpoint.as_deref(),
        "open-runo-router",
    );

    tracing::info!(
        bind_addr = %config.bind_addr,
        env = %config.environment,
        "starting open-runo-router"
    );

    let state = Arc::new(AppState::new());
    let app = build_hyper_app(
        state,
        config.rate_limit_max_requests,
        config.rate_limit_window_secs as i64,
    );

    let addr = config
        .bind_addr
        .parse()
        .map_err(|e| format!("invalid bind_addr {:?}: {e}", config.bind_addr))?;
    let (bound, handle) = hyper_compat::serve(app, addr).await?;
    tracing::info!(%bound, "open-runo-router listening");
    handle.await?;

    Ok(())
}
