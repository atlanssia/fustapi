//! `FustAPI` CLI entry point.
//!
//! Bootstrap parameters come from CLI flags + environment variables.
//! All runtime data (providers, routes) lives in `SQLite`.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// Local-first, high-performance LLM API aggregation gateway.
#[derive(Parser)]
#[command(name = "fustapi", version, about = "Local-first LLM API gateway")]
struct Cli {
    /// Data directory for `SQLite` storage.
    #[arg(long, global = true, env = "FUSTAPI_DATA_DIR")]
    data_dir: Option<PathBuf>,

    /// Subcommand to execute.
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the gateway server.
    Serve {
        /// Host to bind to.
        #[arg(long, env = "FUSTAPI_HOST", default_value = fustapi::config::DEFAULT_HOST)]
        host: String,

        /// Port to listen on.
        #[arg(long, env = "FUSTAPI_PORT", default_value_t = fustapi::config::DEFAULT_PORT)]
        port: u16,
    },

    /// Manage providers.
    Providers {
        #[command(subcommand)]
        command: ProvidersCommand,
    },

    /// Manage routes.
    Routes {
        #[command(subcommand)]
        command: RoutesCommand,
    },
}

#[derive(Subcommand)]
enum ProvidersCommand {
    /// List configured providers.
    List,
    /// Add a new provider.
    Add {
        /// Provider name (unique identifier).
        name: String,
        /// Provider type (omlx, lmstudio, sglang, openai, openai-compatible, deepseek, glm, z.ai).
        #[arg(long, rename_all = "lower")]
        r#type: String,
        /// Provider endpoint URL (include version path, e.g. /v1). Defaults to the provider's well-known base URL if omitted.
        #[arg(long)]
        endpoint: Option<String>,
        /// API key (for cloud providers).
        #[arg(long)]
        api_key: Option<String>,
        /// Optional upstream model name override.
        #[arg(long)]
        upstream_model: Option<String>,
    },
}

#[derive(Subcommand)]
enum RoutesCommand {
    /// List model routes.
    List,
    /// Add or update a model route.
    Add {
        /// Model name.
        model: String,
        /// Comma-separated provider names in priority order.
        #[arg(long, value_delimiter = ',')]
        providers: Vec<String>,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    let data_dir = cli.data_dir.unwrap_or_else(|| {
        dirs::home_dir()
            .expect("could not determine home directory")
            .join(".fustapi")
    });

    match cli.command {
        Commands::Serve { host, port } => {
            let bootstrap = fustapi::config::BootstrapConfig {
                host,
                port,
                data_dir,
            };

            let config = fustapi::config::load_from_db(&bootstrap.db_path()).unwrap_or_else(|e| {
                eprintln!("Warning: Could not load config ({e}). Using defaults.");
                fustapi::config::default_config()
            });

            let addr: SocketAddr = format!("{}:{}", bootstrap.host, bootstrap.port)
                .parse()
                .expect("invalid host:port combination");

            let router = fustapi::router::RealRouter::from_config(&config);

            let server_config = fustapi::server::ServerConfig {
                addr,
                router: std::sync::Arc::new(router),
                db_path: bootstrap.db_path(),
            };

            if let Err(e) = fustapi::server::run(server_config).await {
                eprintln!("Server error: {e}");
                std::process::exit(1);
            }
        }
        Commands::Providers { command } => {
            let bootstrap = fustapi::config::BootstrapConfig {
                host: String::new(),
                port: 0,
                data_dir,
            };
            handle_providers(command, &bootstrap);
        }
        Commands::Routes { command } => {
            let bootstrap = fustapi::config::BootstrapConfig {
                host: String::new(),
                port: 0,
                data_dir,
            };
            handle_routes(command, &bootstrap);
        }
    }
}

fn handle_providers(command: ProvidersCommand, bootstrap: &fustapi::config::BootstrapConfig) {
    let db_path = bootstrap.db_path();
    match command {
        ProvidersCommand::List => {
            let config = match fustapi::config::load_from_db(&db_path) {
                Ok(cfg) => cfg,
                Err(e) => {
                    eprintln!("Could not load config: {e}");
                    std::process::exit(1);
                }
            };
            if config.providers.is_empty() {
                println!("No providers configured.");
                println!("Add one: fustapi providers add <name> --type <type> --endpoint <url>");
                return;
            }
            println!("{:<20} {:<10} {:<45} API Key", "Name", "Type", "Endpoint");
            println!("{}", "─".repeat(80));
            for (name, provider) in &config.providers {
                let has_key = provider.api_key.as_ref().map_or("no", |_| "yes");
                println!(
                    "{:<20} {:<10} {:<45} {}",
                    name, provider.r#type, provider.endpoint, has_key
                );
            }
        }
        ProvidersCommand::Add {
            name,
            r#type,
            endpoint,
            api_key,
            upstream_model,
        } => {
            let valid_types = [
                "omlx",
                "lmstudio",
                "sglang",
                "openai",
                "openai-compatible",
                "deepseek",
                "glm",
                "z.ai",
            ];
            if !valid_types.contains(&r#type.as_str()) {
                eprintln!(
                    "Unknown provider type '{}'. Valid types: {}",
                    r#type,
                    valid_types.join(", ")
                );
                std::process::exit(1);
            }
            let endpoint = endpoint.unwrap_or_else(|| {
                fustapi::config::default_endpoint(&r#type)
                    .map(String::from)
                    .unwrap_or_default()
            });
            if endpoint.is_empty() {
                eprintln!("No default endpoint for type '{type}'. Provide --endpoint.");
                std::process::exit(1);
            }
            let mut config = fustapi::config::load_from_db(&db_path)
                .unwrap_or_else(|_| fustapi::config::default_config());
            if config.providers.contains_key(&name) {
                eprintln!("Provider '{name}' already exists. Use the Web UI to update.");
                std::process::exit(1);
            }
            config.providers.insert(
                name.clone(),
                fustapi::config::ProviderConfig {
                    endpoint,
                    api_key,
                    model: upstream_model,
                    r#type,
                },
            );
            if let Err(e) = fustapi::config::save_to_db(&config, &db_path) {
                eprintln!("Failed to save: {e}");
                std::process::exit(1);
            }
            println!("Provider '{name}' added.");
        }
    }
}

fn handle_routes(command: RoutesCommand, bootstrap: &fustapi::config::BootstrapConfig) {
    let db_path = bootstrap.db_path();
    match command {
        RoutesCommand::List => {
            let config = match fustapi::config::load_from_db(&db_path) {
                Ok(cfg) => cfg,
                Err(e) => {
                    eprintln!("Could not load config: {e}");
                    std::process::exit(1);
                }
            };
            if config.router.is_empty() {
                println!("No routes configured.");
                println!("Add one: fustapi routes add <model> --providers <p1,p2>");
                return;
            }
            println!("Model routing:");
            for (model, route_cfg) in &config.router {
                println!("  {} → {}", model, route_cfg.provider_ids.join(" → "));
            }
        }
        RoutesCommand::Add { model, providers } => {
            if providers.is_empty() {
                eprintln!("At least one provider is required.");
                std::process::exit(1);
            }
            let mut config = fustapi::config::load_from_db(&db_path)
                .unwrap_or_else(|_| fustapi::config::default_config());
            config.router.insert(model.clone(), fustapi::config::RouteConfig {
                provider_ids: providers,
                upstream_model: None,
            });
            if let Err(e) = fustapi::config::save_to_db(&config, &db_path) {
                eprintln!("Failed to save: {e}");
                std::process::exit(1);
            }
            println!("Route '{model}' saved.");
        }
    }
}
