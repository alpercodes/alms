use clap::{Parser, Subcommand};
use tracing::info;

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
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Gateway { bind } => {
            info!("Starting ALMS Gateway...");
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
