mod cmd_agent;
mod cmd_auth;
mod cmd_job;
mod cmd_run;
mod cmd_session;
mod helpers;

use cmd_agent::AgentCommands;
use cmd_auth::AuthCommands;
use cmd_job::JobCommands;
use cmd_run::RunCommands;
use cmd_session::SessionCommands;
use helpers::{api_client, open_db};

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{Shell, generate};
use tracing::{error, info, warn};

#[derive(Parser, Debug)]
#[command(name = "alms")]
#[command(about = "ALMS - Agent Loop Management System")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Start the gateway server
    Gateway {
        /// Bind address (defaults to config value or 127.0.0.1:8080)
        #[arg(short, long)]
        bind: Option<String>,
        /// OpenRouter/OpenAI API key (overrides OPENROUTER_API_KEY env var)
        #[arg(long, env = "OPENROUTER_API_KEY")]
        api_key: Option<String>,
    },
    /// Check system health
    Health {
        /// Gateway URL to check
        #[arg(
            short,
            long,
            default_value = "http://127.0.0.1:8080",
            env = "ALMS_GATEWAY_URL"
        )]
        url: String,
        /// Output as JSON instead of human-readable text
        #[arg(long)]
        json: bool,
    },
    /// Open the ALMS web UI in your browser
    Dashboard {
        /// Gateway URL to open
        #[arg(
            short,
            long,
            default_value = "http://127.0.0.1:8080",
            env = "ALMS_GATEWAY_URL"
        )]
        url: String,
    },
    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: Shell,
    },
    /// Manage sessions
    Session {
        #[command(subcommand)]
        cmd: SessionCommands,
        /// Output as JSON instead of human-readable text
        #[arg(long, global = true)]
        json: bool,
    },
    /// Manage API keys for LLM providers
    Auth {
        #[command(subcommand)]
        cmd: AuthCommands,
        /// Output as JSON instead of human-readable text
        #[arg(long, global = true)]
        json: bool,
    },
    /// Manage agents
    Agent {
        #[command(subcommand)]
        cmd: AgentCommands,
        /// Output as JSON instead of human-readable text
        #[arg(long, global = true)]
        json: bool,
    },
    /// Manage runs (requires running gateway)
    Run {
        #[command(subcommand)]
        cmd: RunCommands,
        /// Output as JSON instead of human-readable text
        #[arg(long, global = true)]
        json: bool,
        /// Gateway URL
        #[arg(
            long,
            global = true,
            default_value = "http://127.0.0.1:8080",
            env = "ALMS_GATEWAY_URL"
        )]
        url: String,
    },
    /// Manage scheduled jobs
    Job {
        #[command(subcommand)]
        cmd: JobCommands,
        /// Output as JSON instead of human-readable text
        #[arg(long, global = true)]
        json: bool,
        /// Gateway URL (used by create/cancel)
        #[arg(
            long,
            global = true,
            default_value = "http://127.0.0.1:8080",
            env = "ALMS_GATEWAY_URL"
        )]
        url: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Gateway { bind, api_key } => {
            info!("Starting ALMS Gateway...");
            match &api_key {
                Some(k) => info!("API key provided ({} chars)", k.len()),
                None => {
                    let found: Vec<String> = std::env::vars()
                        .filter(|(k, _)| k.contains("API_KEY") || k.contains("OPENROUTER"))
                        .map(|(k, v)| format!("{}=({} chars)", k, v.len()))
                        .collect();
                    if found.is_empty() {
                        error!(
                            "No API key found. Pass --api-key sk-or-... or set OPENROUTER_API_KEY."
                        );
                    } else {
                        warn!("API key env vars visible to process: {}", found.join(", "));
                    }
                }
            }
            if let Some(key) = api_key {
                unsafe {
                    std::env::set_var("OPENROUTER_API_KEY", key);
                }
            }
            // Load config once — used for bind address and passed to the gateway.
            let config = alms_core::AlmsConfig::load().unwrap_or_else(|e| {
                warn!("Failed to load config, using defaults: {}", e);
                alms_core::AlmsConfig::default()
            });
            // Resolve bind address: --bind flag > config (alms.toml / ALMS_BIND) > default
            let bind = bind.unwrap_or(config.server.bind.clone());
            alms_gateway::serve_with_config(&bind, &config).await?;
        }
        Commands::Health { url, json } => {
            let client = api_client()?;
            let health_url = format!("{}/health", url.trim_end_matches('/'));
            match client.get(&health_url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    let body: serde_json::Value = resp.json().await?;
                    if json {
                        let mut envelope = body;
                        if let Some(obj) = envelope.as_object_mut() {
                            obj.insert("ok".into(), serde_json::json!(true));
                        }
                        println!("{}", serde_json::to_string_pretty(&envelope)?);
                    } else {
                        println!("ALMS Gateway is healthy");
                        if let Some(version) = body.get("version").and_then(|v| v.as_str()) {
                            println!("  version: {}", version);
                        }
                    }
                }
                Ok(resp) => {
                    let status = resp.status();
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({ "ok": false, "error": format!("HTTP {status}") })
                        );
                    }
                    anyhow::bail!("Health check failed: HTTP {}", status);
                }
                Err(e) => {
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({ "ok": false, "error": format!("Cannot reach gateway: {e}") })
                        );
                    }
                    anyhow::bail!("Cannot reach gateway at {}: {}", health_url, e);
                }
            }
        }
        Commands::Dashboard { url } => {
            let url = url.trim_end_matches('/').to_string();
            println!("Opening {url} ...");
            match open::that(&url) {
                Ok(()) => {
                    // open::that() can silently succeed on headless systems
                    // (no browser available). Always print the URL as a fallback.
                    println!("If the browser did not open, visit: {url}");
                }
                Err(e) => {
                    eprintln!("Open manually: {url}");
                    anyhow::bail!("Failed to open browser: {e}");
                }
            }
        }
        Commands::Completions { shell } => {
            let mut cmd = Cli::command();
            generate(shell, &mut cmd, "alms", &mut std::io::stdout());
        }
        Commands::Session { cmd, json } => {
            let store = open_db()?;
            match cmd {
                SessionCommands::List { agent } => {
                    cmd_session::session_list(&store, agent, json)?;
                }
                SessionCommands::Show { session_id } => {
                    cmd_session::session_show(&store, &session_id, json)?;
                }
                SessionCommands::Delete { session_id } => {
                    cmd_session::session_delete(&store, &session_id, json)?;
                }
            }
        }
        Commands::Auth { cmd, json } => {
            let config = alms_core::AlmsConfig::load().unwrap_or_default();
            let data_dir: std::path::PathBuf = config.server.data_dir.into();
            match cmd {
                AuthCommands::Set { provider, key } => {
                    cmd_auth::auth_set(&data_dir, &provider, key, json)?;
                }
                AuthCommands::List => {
                    cmd_auth::auth_list(&data_dir, json)?;
                }
                AuthCommands::Remove { provider } => {
                    cmd_auth::auth_remove(&data_dir, &provider, json)?;
                }
            }
        }
        Commands::Agent { cmd, json } => {
            let (store, config) = helpers::open_db_with_config()?;
            match cmd {
                AgentCommands::List => cmd_agent::agent_list(&store, json)?,
                AgentCommands::Create {
                    name,
                    description,
                    model,
                    posture,
                    provider,
                    default,
                } => {
                    let workspace_dir = config.server.workspace_dir();
                    cmd_agent::agent_create(
                        &store,
                        name,
                        description,
                        model,
                        posture,
                        provider,
                        default,
                        json,
                        Some(&workspace_dir),
                    )?;
                }
                AgentCommands::Show { name_or_id } => {
                    cmd_agent::agent_show(&store, &name_or_id, json)?;
                }
                AgentCommands::Delete { name_or_id, force } => {
                    cmd_agent::agent_delete(&store, &name_or_id, force, json)?;
                }
                AgentCommands::SetDefault { name_or_id } => {
                    cmd_agent::agent_set_default(&store, &name_or_id, json)?;
                }
                AgentCommands::Config {
                    name_or_id,
                    model,
                    posture,
                    provider,
                    description,
                } => {
                    cmd_agent::agent_config(
                        &store,
                        &name_or_id,
                        model,
                        posture,
                        provider,
                        description,
                        json,
                    )?;
                }
            }
        }
        Commands::Run { cmd, json, url } => {
            let client = api_client()?;
            match cmd {
                RunCommands::Create {
                    session,
                    input,
                    model,
                    max_tokens,
                    posture,
                } => {
                    cmd_run::run_create(
                        &client, &url, &session, &input, model, max_tokens, posture, json,
                    )
                    .await?;
                }
                RunCommands::List { session, limit } => {
                    cmd_run::run_list(&client, &url, &session, limit, json).await?;
                }
                RunCommands::Show { run_id } => {
                    cmd_run::run_show(&client, &url, &run_id, json).await?;
                }
            }
        }
        Commands::Job { cmd, json, url } => match cmd {
            JobCommands::List { agent } => {
                let store = open_db()?;
                cmd_job::job_list(&store, agent, json)?;
            }
            JobCommands::Show { job_id } => {
                let store = open_db()?;
                cmd_job::job_show(&store, &job_id, json)?;
            }
            JobCommands::Create {
                agent,
                prompt,
                schedule,
            } => {
                let client = api_client()?;
                cmd_job::job_create(&client, &url, &agent, &prompt, &schedule, json).await?;
            }
            JobCommands::Cancel { job_id } => {
                let client = api_client()?;
                cmd_job::job_cancel(&client, &url, &job_id, json).await?;
            }
        },
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_completions_bash() {
        let mut cmd = Cli::command();
        let mut buf = Vec::new();
        generate(Shell::Bash, &mut cmd, "alms", &mut buf);
        assert!(!buf.is_empty());
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("alms"));
    }

    #[test]
    fn test_completions_powershell() {
        let mut cmd = Cli::command();
        let mut buf = Vec::new();
        generate(Shell::PowerShell, &mut cmd, "alms", &mut buf);
        assert!(!buf.is_empty());
    }
}
