use anyhow::Result;
use clap::{Parser, Subcommand};
use rmcp::ServiceExt;
use soudan::{app::App, install::install};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "Consult AI agents through a shared, pluggable MCP server"
)]
struct Cli {
    #[arg(long, global = true, default_value = ".")]
    workspace: PathBuf,
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    /// Discover, connect, and send messages to already-open terminal chats.
    Live {
        #[command(subcommand)]
        command: LiveCommands,
    },
    /// Serve MCP over stdio (stdout is reserved for protocol messages).
    Serve,
    /// Configure project-scoped MCP connections, preserving other servers.
    Install {
        #[arg(long, default_value = "all")]
        client: String,
    },
    /// Check which agent executables are on PATH (does not check login).
    Doctor,
    /// Start a consultation; add --wait to wait for its response.
    Consult {
        #[arg(long)]
        agent: String,
        #[arg(long)]
        room: Option<String>,
        #[arg(long, default_value_t = 180)]
        timeout: u64,
        #[arg(long)]
        wait: bool,
        prompt: String,
    },
    /// Read a consultation job, including any error.
    Result { job_id: String },
    /// Post a message to a shared room.
    Post {
        #[arg(long)]
        room: String,
        #[arg(long)]
        sender: String,
        text: String,
    },
    /// Read up to 100 messages; pass the last ID as --after for the next page.
    History {
        #[arg(long)]
        room: String,
        #[arg(long, default_value_t = 0)]
        after: i64,
    },
    #[command(hide = true)]
    Worker { job_id: String },
}
#[derive(Subcommand)]
enum LiveCommands {
    /// Read a saved delivery status without resending.
    Delivery {
        request_id: String,
    },
    /// Stop the bridge and remove its shortcut; leave chats open.
    Disconnect,
    List,
    Setup {
        #[arg(long)]
        via_pid: u32,
    },
    Read {
        target: String,
    },
    Send {
        target: String,
        #[arg(long)]
        request_id: String,
        text: String,
    },
    #[command(hide = true)]
    Bridge,
}
#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Commands::Install { client } = &cli.command {
        let paths = install(
            &cli.workspace.canonicalize()?,
            &std::env::current_exe()?,
            client,
            cli.config.as_deref(),
        )?;
        println!("{}", serde_json::to_string_pretty(&paths)?);
        return Ok(());
    }
    let app = App::open(&cli.workspace, cli.config.as_deref())?;
    let value = match cli.command {
        Commands::Serve => {
            soudan::mcp::Mcp { app }
                .serve(rmcp::transport::stdio())
                .await?
                .waiting()
                .await?;
            return Ok(());
        }
        Commands::Worker { job_id } => {
            app.worker(&job_id).await?;
            return Ok(());
        }
        Commands::Live { command } => match command {
            LiveCommands::Delivery { request_id } => {
                soudan::live::delivery(&app.workspace, &request_id)?
            }
            LiveCommands::Disconnect => soudan::live::disconnect(&app.workspace).await?,
            LiveCommands::List => serde_json::to_value(soudan::live::discover(&app.workspace)?)?,
            LiveCommands::Setup { via_pid } => soudan::live::setup(&app.workspace, via_pid).await?,
            LiveCommands::Read { target } => soudan::live::read(&app.workspace, &target).await?,
            LiveCommands::Send {
                target,
                request_id,
                text,
            } => soudan::live::send(&app.workspace, &target, &text, &request_id).await?,
            LiveCommands::Bridge => {
                soudan::live::bridge(&app.workspace).await?;
                return Ok(());
            }
        },
        Commands::Doctor => app.agents(),
        Commands::Consult {
            agent,
            room,
            timeout,
            wait,
            prompt,
        } => {
            let started = app.start(&agent, &prompt, room.as_deref(), timeout, None)?;
            if wait {
                let id = started["job_id"].as_str().unwrap();
                loop {
                    let job = app.store.job(id)?;
                    if job.status == "completed" {
                        break serde_json::to_value(job)?;
                    }
                    if job.status == "failed" {
                        anyhow::bail!(
                            "Consultation {id} failed: {}",
                            job.error.unwrap_or_default()
                        );
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                }
            } else {
                started
            }
        }
        Commands::Result { job_id } => serde_json::to_value(app.store.job(&job_id)?)?,
        Commands::Post { room, sender, text } => {
            serde_json::json!({"id":app.store.post(&room,&sender,&text)?})
        }
        Commands::History { room, after } => {
            serde_json::to_value(app.store.history(&room, after)?)?
        }
        Commands::Install { .. } => unreachable!(),
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
