//! FustAPI CLI entry point.
//!
//! Parses command-line arguments with clap and dispatches to subcommand handlers.

use std::net::SocketAddr;

use clap::{Parser, Subcommand};
use tracing::info;

/// Local-first, high-performance LLM API aggregation gateway.
#[derive(Parser)]
#[command(name = "fustapi", about = "Local-first LLM API gateway")]
struct Cli {
    /// Subcommand to execute.
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the gateway server.
    Serve {
        /// Host to bind to.
        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        /// Port to listen on.
        #[arg(long, default_value_t = 8080)]
        port: u16,
    },

    /// Configuration management.
    Config {
        /// Config subcommand to execute.
        #[command(subcommand)]
        command: ConfigSubcommand,
    },

    /// List configured providers.
    Providers,
}

#[derive(Subcommand)]
enum ConfigSubcommand {
    /// Initialize default configuration file.
    Init,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Serve { host, port } => {
            let addr = SocketAddr::from((
                host.parse::<std::net::IpAddr>().expect("invalid host"),
                port,
            ));
            let config = fustapi::server::ServerConfig { addr };
            if let Err(e) = fustapi::server::run(config).await {
                eprintln!("Server error: {e}");
                std::process::exit(1);
            }
        }
        Commands::Config { command } => match command {
            ConfigSubcommand::Init => {
                info!("Initializing default configuration...");
                config_init().await;
            }
        },
        Commands::Providers => {
            info!("Listing providers...");
            providers_list();
        }
    }
}

/// Stub: initialize default configuration file.
async fn config_init() {
    println!("todo: write default config file");
}

/// Stub: list configured providers.
fn providers_list() {
    println!("todo: list providers");
}
