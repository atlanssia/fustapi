//! FustAPI CLI entry point.
//!
//! Parses command-line arguments with clap and dispatches to subcommand handlers.

use std::net::SocketAddr;

use clap::{Parser, Subcommand};

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
        /// Host to bind to (overrides config).
        #[arg(long)]
        host: Option<String>,

        /// Port to listen on (overrides config).
        #[arg(long)]
        port: Option<u16>,
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

#[allow(deprecated)]
#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Serve { host, port } => {
            let config = load_server_config(host, port);
            if let Err(e) = fustapi::server::run(config).await {
                eprintln!("Server error: {e}");
                std::process::exit(1);
            }
        }
        Commands::Config { command } => match command {
            ConfigSubcommand::Init => {
                if let Err(e) = fustapi::config::init_config() {
                    eprintln!("Failed to initialize config: {e}");
                    std::process::exit(1);
                }
            }
        },
        Commands::Providers => {
            providers_list();
        }
    }
}

/// Load server configuration, merging CLI overrides with config file / defaults.
fn load_server_config(
    cli_host: Option<String>,
    cli_port: Option<u16>,
) -> fustapi::server::ServerConfig {
    #[allow(deprecated)]
    let config = fustapi::config::load_merged(&fustapi::config::db_path()).unwrap_or_else(|e| {
        eprintln!("Warning: Could not load config ({e}). Using defaults.");
        fustapi::config::default_config()
    });

    let host = cli_host.unwrap_or(config.server.host);
    let port = cli_port.unwrap_or(config.server.port);

    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .expect("invalid host:port combination");

    fustapi::server::ServerConfig { addr }
}

/// List configured providers from the config file.
#[allow(deprecated)]
fn providers_list() {
    let config = match fustapi::config::load_merged(&fustapi::config::db_path()) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("Warning: Could not load config ({e}).");
            println!("Run `fustapi config init` to create a configuration file.");
            return;
        }
    };

    if config.providers.is_empty() && config.router.is_empty() {
        println!("No providers or model routing configured.");
        println!("Run `fustapi config init` to create a configuration file.");
        return;
    }

    if !config.providers.is_empty() {
        println!("Configured providers:");
        println!("{:<20} {:<45} API Key", "Name", "Endpoint");
        println!("{}", "─".repeat(70));

        for (name, provider) in &config.providers {
            let has_key = provider.api_key.as_ref().map_or("no", |_| "yes");
            println!("{:<20} {:<45} {}", name, provider.endpoint, has_key);
        }

        println!();
    }

    if !config.router.is_empty() {
        println!("Model routing:");
        for (model, providers) in &config.router {
            println!("  {} → {}", model, providers.join(" → "));
        }
    }
}
