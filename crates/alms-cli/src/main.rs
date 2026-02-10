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
    Health,
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
        Commands::Health => {
            println!("ALMS Gateway is healthy");
        }
        Commands::Sessions => {
            println!("Session management not yet implemented");
        }
    }
    
    Ok(())
}