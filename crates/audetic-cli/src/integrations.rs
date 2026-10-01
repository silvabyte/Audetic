use anyhow::{bail, Context, Result};
use audetic_core::url::{api_url, integration_key_path, paths};
use reqwest::{Client, Method, RequestBuilder};
use serde_json::{json, Value};

use crate::args::{IntegrationKeysCommand, IntegrationsCliArgs, IntegrationsCommand, PlaudCommand};
use crate::client::{json_or_error, CONNECT_HINT};

pub async fn handle_integrations_command(args: IntegrationsCliArgs) -> Result<()> {
    let client = Client::new();
    let request = match args.command {
        IntegrationsCommand::Status => client.get(api_url(paths::INTEGRATIONS)),
        IntegrationsCommand::Imports { limit } => client
            .get(api_url(paths::INTEGRATION_IMPORTS))
            .query(&[("limit", limit.min(100))]),
        IntegrationsCommand::Keys(args) => match args.command {
            IntegrationKeysCommand::Add { name, scope } => {
                if name.trim().is_empty() {
                    bail!("Access key name must not be empty");
                }
                client.post(api_url(paths::INTEGRATION_KEYS)).json(&json!({
                    "name": name,
                    "scope": scope.as_str(),
                }))
            }
            IntegrationKeysCommand::List => client.get(api_url(paths::INTEGRATION_KEYS)),
            IntegrationKeysCommand::Revoke { id } => key_request(&client, Method::DELETE, &id)?,
        },
        IntegrationsCommand::Plaud(args) => match args.command {
            PlaudCommand::Status => client.get(api_url(paths::INTEGRATION_PLAUD)),
            PlaudCommand::Enable { interval_minutes } => {
                if !(5..=1440).contains(&interval_minutes) {
                    bail!("--interval-minutes must be between 5 and 1440");
                }
                client.put(api_url(paths::INTEGRATION_PLAUD)).json(&json!({
                    "enabled": true,
                    "interval_minutes": interval_minutes,
                }))
            }
            PlaudCommand::Disable => {
                let current = send_json(
                    client.get(api_url(paths::INTEGRATION_PLAUD)),
                    "read Plaud settings",
                )
                .await?;
                let interval = current["interval_minutes"].as_i64().unwrap_or(15);
                client.put(api_url(paths::INTEGRATION_PLAUD)).json(&json!({
                    "enabled": false,
                    "interval_minutes": interval,
                }))
            }
            PlaudCommand::Sync => client.post(api_url(paths::INTEGRATION_PLAUD_SYNC)),
            PlaudCommand::Backfill => client.post(api_url(paths::INTEGRATION_PLAUD_BACKFILL)),
        },
    };

    let value = send_json(request, "external integrations").await?;
    if value.get("secret").is_some() {
        eprintln!("Save this access key now. Audetic will not display it again.");
    }
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn key_request(client: &Client, method: Method, id: &str) -> Result<RequestBuilder> {
    uuid::Uuid::parse_str(id).context("Access key ID must be a UUID")?;
    Ok(client.request(method, api_url(&integration_key_path(id))))
}

async fn send_json(request: RequestBuilder, operation: &str) -> Result<Value> {
    let response = request.send().await.context(CONNECT_HINT)?;
    json_or_error(response, operation).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::{Cli, CliCommand, IntegrationKeyScope};
    use clap::Parser;

    #[test]
    fn integration_commands_are_typed_and_validate_key_ids() {
        let cli = Cli::try_parse_from([
            "audetic",
            "integrations",
            "keys",
            "add",
            "--name",
            "Index ring",
            "--scope",
            "index",
        ])
        .unwrap();
        let Some(CliCommand::Integrations(args)) = cli.command else {
            panic!("wrong command")
        };
        let IntegrationsCommand::Keys(keys) = args.command else {
            panic!("wrong integration command")
        };
        let IntegrationKeysCommand::Add { name, scope } = keys.command else {
            panic!("wrong key command")
        };
        assert_eq!(name, "Index ring");
        assert!(matches!(scope, IntegrationKeyScope::Index));
        assert!(key_request(&Client::new(), Method::DELETE, "not-a-uuid").is_err());
    }
}
