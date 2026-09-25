//! `kiln-agent` command-line entry point.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};
use kiln_agent::config::AgentConfig;
use kiln_agent::security::{generate_token, hash_token, load_or_create_admin_token};
use kiln_agent::{StartOptions, logging};

#[derive(Debug, Parser)]
#[command(name = "kiln-agent", version, about = "Kiln Print local print agent")]
struct Cli {
    /// Configuration file (default: <config dir>/KilnPrint/agent.toml if present).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the agent (default).
    Run {
        /// Also register simulated printers (development and demos).
        #[arg(long)]
        mock: bool,
        /// Do not use the OS spooler provider.
        #[arg(long)]
        no_os_printers: bool,
    },
    /// List printers the agent can see, then exit.
    Printers {
        #[arg(long)]
        mock: bool,
    },
    /// Show the local admin token (creating it if needed) and where it is stored.
    Token,
    /// Generate a token for a new client and print the configuration entry to add.
    NewClient {
        /// Client id, e.g. `lab-app`.
        #[arg(long)]
        id: String,
        /// Display name, e.g. "Lab Application".
        #[arg(long)]
        name: String,
        /// Allowed browser origin (repeatable). Omit for native clients.
        #[arg(long = "origin")]
        origins: Vec<String>,
    },
    /// Print the effective configuration as TOML.
    Config,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let mut config = AgentConfig::load(cli.config.as_deref())?;
    match cli.command.unwrap_or(Command::Run {
        mock: false,
        no_os_printers: false,
    }) {
        Command::Run {
            mock,
            no_os_printers,
        } => {
            config.providers.mock |= mock;
            config.providers.windows &= !no_os_printers;
            run(config)
        }
        Command::Printers { mock } => {
            config.providers.mock |= mock;
            list_printers(config)
        }
        Command::Token => {
            let dir = config.data_dir()?;
            let path = config
                .security
                .admin_token_file
                .clone()
                .unwrap_or_else(|| dir.join("admin.token"));
            let token = load_or_create_admin_token(&path)?;
            println!("admin token file: {}", path.display());
            println!("{token}");
            Ok(())
        }
        Command::NewClient { id, name, origins } => {
            let token = generate_token()?;
            let entry = kiln_agent::config::ClientConfig {
                id,
                name,
                token_sha256: hex::encode(hash_token(&token)),
                origins,
                permissions: vec![
                    kiln_agent::security::Permission::PrintersRead,
                    kiln_agent::security::Permission::Print,
                    kiln_agent::security::Permission::JobsRead,
                    kiln_agent::security::Permission::JobsCancel,
                    kiln_agent::security::Permission::QueueRead,
                ],
                printers: vec!["*".into()],
            };
            let mut probe = config.clone();
            probe.security.clients.push(entry.clone());
            probe.validate()?;
            #[derive(serde::Serialize)]
            struct Wrapper {
                clients: Vec<kiln_agent::config::ClientConfig>,
            }
            let snippet = toml::to_string(&Wrapper {
                clients: vec![entry],
            })?;
            println!("Client token (shown once; give it to the application):\n\n  {token}\n");
            println!("Add this to the [security] section of agent.toml and restart the agent:\n");
            println!("{}", snippet.replace("[[clients]]", "[[security.clients]]"));
            Ok(())
        }
        Command::Config => {
            print!("{}", toml::to_string_pretty(&config)?);
            Ok(())
        }
    }
}

fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    let workers = std::thread::available_parallelism().map_or(2, |n| n.get().clamp(2, 4));
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        // Provider calls run on the blocking pool; the engine bounds them with semaphores,
        // this caps the pool itself so thread count can never run away.
        .max_blocking_threads(32)
        .thread_name("kiln-agent")
        .enable_all()
        .build()
        .context("starting the async runtime")
}

fn run(config: AgentConfig) -> anyhow::Result<()> {
    let data_dir = config.data_dir()?;
    std::fs::create_dir_all(&data_dir)?;
    let _log_guard = logging::init(&config.logging, &data_dir)?;
    runtime()?.block_on(async move {
        let agent = kiln_agent::start(config, StartOptions::default()).await?;
        println!("Kiln agent listening on ws://{}/v1/ws", agent.local_addr);
        let stop = agent.shutdown_token();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => tracing::info!(target: "kiln::agent", "shutdown requested"),
            _ = stop.cancelled() => {}
        }
        agent.shutdown().await;
        Ok(())
    })
}

fn list_printers(config: AgentConfig) -> anyhow::Result<()> {
    runtime()?.block_on(async move {
        let repo = Arc::new(kiln_core::repository::InMemoryJobRepository::new());
        let engine = kiln_agent::build_engine(&config, repo, Vec::new())?;
        let printers = engine.refresh_printers().await?;
        if printers.is_empty() {
            println!("No printers found.");
        }
        for p in printers {
            println!(
                "{}{}\n    id: {}\n    type: {:?}  status: {:?}  online: {}\n    driver: {}\n    port: {}",
                p.name,
                if p.default { "  (default)" } else { "" },
                p.id,
                p.connection,
                p.status,
                p.online,
                p.driver.as_deref().unwrap_or("-"),
                p.port.as_deref().unwrap_or("-"),
            );
            if let Ok(caps) = engine.capabilities(&p.id).await {
                println!("    raw: {:?}  document types: {:?}", caps.raw, caps.document_types);
            }
        }
        Ok(())
    })
}
