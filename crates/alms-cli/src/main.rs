use clap::{Parser, Subcommand};
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
        /// Bind address
        #[arg(short, long, default_value = "127.0.0.1:8080")]
        bind: String,
        /// OpenRouter/OpenAI API key (overrides OPENROUTER_API_KEY env var)
        #[arg(long, env = "OPENROUTER_API_KEY")]
        api_key: Option<String>,
    },
    /// Check system health
    Health {
        /// Gateway URL to check
        #[arg(short, long, default_value = "http://127.0.0.1:8080")]
        url: String,
    },
    /// Manage sessions
    Sessions,
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
                    // Dump any key-like env vars to help diagnose missing key issues.
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
                // Ensure the key is in the env so AlmsConfig::load() picks it up.
                unsafe {
                    std::env::set_var("OPENROUTER_API_KEY", key);
                }
            }
            alms_gateway::serve(&bind).await?;
        }
        Commands::Health { url } => {
            let health_url = format!("{}/health", url.trim_end_matches('/'));
            match reqwest::get(&health_url).await {
                Ok(resp) if resp.status().is_success() => {
                    let body: serde_json::Value = resp.json().await?;
                    println!("ALMS Gateway is healthy");
                    if let Some(version) = body.get("version").and_then(|v| v.as_str()) {
                        println!("  version: {}", version);
                    }
                }
                Ok(resp) => {
                    eprintln!("Health check failed: HTTP {}", resp.status());
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("Cannot reach gateway at {}: {}", health_url, e);
                    std::process::exit(1);
                }
            }
        }
        Commands::Sessions => {
            println!("Session management not yet implemented");
        }
    }

    Ok(())
}
