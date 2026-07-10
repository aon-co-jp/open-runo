//! `open-runo-cli` -- a small command-line client over `open-runo-router`'s
//! REST API: schema registry register/get/history, federation status, and
//! an OpenAPI spec dump. This is open-runo's answer to Cosmo's `wgc` CLI
//! (see `docs/cosmo-parity.md` 4a, "Powerful CLI (`wgc`相当)").
//!
//! If no `--api-key` is given, the CLI transparently self-issues a
//! short-lived developer key via `POST /api/keys/self-issue` -- the same
//! "a human never has to manage an API key" flow the WASM frontend uses
//! (see `crates/open-runo-router/src/handlers_hyper.rs::self_issue_key_handler`).

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "open-runo-cli",
    version,
    about = "open-runo command-line client (wgc-equivalent developer tooling)"
)]
struct Cli {
    /// Base URL of the open-runo-router (or open-runo-gateway) instance to talk to.
    #[arg(long, global = true, default_value = "http://localhost:8080", env = "OPEN_RUNO_CLI_BASE_URL")]
    base_url: String,

    /// API key to send as `X-Api-Key`. If omitted, a short-lived developer
    /// key is self-issued automatically (see module docs).
    #[arg(long, global = true, env = "OPEN_RUNO_CLI_API_KEY")]
    api_key: Option<String>,

    /// Print raw JSON responses instead of a human-readable summary.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Schema Registry operations.
    Schema {
        #[command(subcommand)]
        action: SchemaCommand,
    },
    /// Federation status.
    Federation {
        #[command(subcommand)]
        action: FederationCommand,
    },
    /// Fetch the OpenAPI 3.0 spec (`GET /api/openapi.json`).
    Openapi,
    /// Self-issue a fresh developer API key and print it (no other action).
    Login,
}

#[derive(Subcommand)]
enum SchemaCommand {
    /// Register (or update) a schema.
    Register {
        #[arg(long)]
        service: String,
        /// Path to a file containing the GraphQL SDL to register.
        #[arg(long)]
        sdl_file: PathBuf,
        #[arg(long, default_value = "local")]
        stage: String,
    },
    /// Fetch the latest version of a schema for a service.
    Get {
        #[arg(long)]
        service: String,
        #[arg(long, default_value = "local")]
        stage: String,
        #[arg(long)]
        namespace: Option<String>,
    },
    /// List every registered version for a service (composition-check /
    /// search equivalent -- see what's already there before registering).
    History {
        #[arg(long)]
        service: String,
        #[arg(long)]
        namespace: Option<String>,
    },
}

#[derive(Subcommand)]
enum FederationCommand {
    /// Show composed federation status (contributing services, type/field counts).
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = reqwest::Client::new();

    let api_key = match &cli.api_key {
        Some(key) => key.clone(),
        None => self_issue_key(&client, &cli.base_url).await?,
    };

    let body = match &cli.command {
        Command::Login => {
            // Already self-issued above; just confirm it to the user.
            serde_json::json!({ "api_key": api_key })
        }
        Command::Openapi => get(&client, &cli.base_url, &api_key, "/api/openapi.json").await?,
        Command::Federation { action } => match action {
            FederationCommand::Status => get(&client, &cli.base_url, &api_key, "/api/federation/status").await?,
        },
        Command::Schema { action } => match action {
            SchemaCommand::Register { service, sdl_file, stage } => {
                let sdl = std::fs::read_to_string(sdl_file)
                    .with_context(|| format!("reading SDL file {}", sdl_file.display()))?;
                let payload = serde_json::json!({
                    "service_name": service,
                    "sdl": sdl,
                    "stage": stage,
                });
                post(&client, &cli.base_url, &api_key, "/api/schemas", &payload).await?
            }
            SchemaCommand::Get { service, stage, namespace } => {
                let path = with_query(
                    &format!("/api/schemas/{service}"),
                    &[("stage", Some(stage.as_str())), ("namespace", namespace.as_deref())],
                );
                get(&client, &cli.base_url, &api_key, &path).await?
            }
            SchemaCommand::History { service, namespace } => {
                let path = with_query(
                    &format!("/api/schemas/{service}/history"),
                    &[("namespace", namespace.as_deref())],
                );
                get(&client, &cli.base_url, &api_key, &path).await?
            }
        },
    };

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&body)?);
    } else {
        print_human(&cli.command, &body);
    }

    Ok(())
}

fn with_query(path: &str, params: &[(&str, Option<&str>)]) -> String {
    let query: Vec<String> = params
        .iter()
        .filter_map(|(k, v)| v.map(|v| format!("{k}={}", urlencode(v))))
        .collect();
    if query.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{}", query.join("&"))
    }
}

/// Minimal percent-encoding for query values -- these are always simple
/// identifiers (service/namespace/stage names) in practice, but escaping
/// keeps the CLI from breaking if one contains `&`, `=`, or a space.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_query_omits_none_params() {
        assert_eq!(
            with_query("/api/schemas/users/history", &[("namespace", None)]),
            "/api/schemas/users/history"
        );
    }

    #[test]
    fn with_query_joins_present_params() {
        assert_eq!(
            with_query("/api/schemas/users", &[("stage", Some("local")), ("namespace", Some("default"))]),
            "/api/schemas/users?stage=local&namespace=default"
        );
    }

    #[test]
    fn urlencode_escapes_reserved_characters() {
        assert_eq!(urlencode("a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(urlencode("plain-service_name.v1~"), "plain-service_name.v1~");
    }

    #[test]
    fn cli_parses_schema_register_args() {
        let cli = Cli::parse_from([
            "open-runo-cli",
            "schema",
            "register",
            "--service",
            "users",
            "--sdl-file",
            "schema.graphql",
        ]);
        match cli.command {
            Command::Schema { action: SchemaCommand::Register { service, stage, .. } } => {
                assert_eq!(service, "users");
                assert_eq!(stage, "local");
            }
            _ => panic!("expected Schema::Register"),
        }
    }
}

async fn self_issue_key(client: &reqwest::Client, base_url: &str) -> Result<String> {
    let resp = client
        .post(format!("{base_url}/api/keys/self-issue"))
        .send()
        .await
        .with_context(|| format!("connecting to {base_url}"))?;
    let status = resp.status();
    let body: Value = resp.json().await.context("decoding self-issue response as JSON")?;
    if !status.is_success() {
        bail!("self-issuing an API key failed ({status}): {body}");
    }
    body.get("api_key")
        .and_then(Value::as_str)
        .map(str::to_string)
        .context("self-issue response had no api_key field")
}

async fn get(client: &reqwest::Client, base_url: &str, api_key: &str, path: &str) -> Result<Value> {
    let resp = client
        .get(format!("{base_url}{path}"))
        .header("X-Api-Key", api_key)
        .send()
        .await
        .with_context(|| format!("GET {base_url}{path}"))?;
    decode(resp).await
}

async fn post(client: &reqwest::Client, base_url: &str, api_key: &str, path: &str, payload: &Value) -> Result<Value> {
    let resp = client
        .post(format!("{base_url}{path}"))
        .header("X-Api-Key", api_key)
        .json(payload)
        .send()
        .await
        .with_context(|| format!("POST {base_url}{path}"))?;
    decode(resp).await
}

async fn decode(resp: reqwest::Response) -> Result<Value> {
    let status = resp.status();
    let body: Value = resp.json().await.context("decoding response as JSON")?;
    if !status.is_success() {
        bail!("request failed ({status}): {body}");
    }
    Ok(body)
}

fn print_human(command: &Command, body: &Value) {
    match command {
        Command::Login => {
            println!("api_key: {}", body.get("api_key").and_then(Value::as_str).unwrap_or(""));
        }
        Command::Openapi => {
            println!("{}", serde_json::to_string_pretty(body).unwrap_or_default());
        }
        Command::Federation { .. } => {
            let services = body
                .get("contributing_services")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            println!("contributing services: [{services}]");
            println!("types: {}", body.get("type_count").and_then(Value::as_u64).unwrap_or(0));
            println!("fields: {}", body.get("field_count").and_then(Value::as_u64).unwrap_or(0));
        }
        Command::Schema { action } => match action {
            SchemaCommand::History { .. } => {
                let versions = body
                    .get("versions")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                println!("{} version(s):", versions.len());
                for v in versions {
                    print_schema_version(&v);
                }
            }
            SchemaCommand::Register { .. } | SchemaCommand::Get { .. } => {
                print_schema_version(body);
            }
        },
    }
}

fn print_schema_version(v: &Value) {
    println!(
        "  {} @{} [{}] id={} created_at={}",
        v.get("service_name").and_then(Value::as_str).unwrap_or("?"),
        v.get("stage").and_then(Value::as_str).unwrap_or("?"),
        v.get("namespace").and_then(Value::as_str).unwrap_or("?"),
        v.get("id").and_then(Value::as_str).unwrap_or("?"),
        v.get("created_at").and_then(Value::as_str).unwrap_or("?"),
    );
}
