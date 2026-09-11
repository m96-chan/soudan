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
    /// Wait for room messages without consuming them (exit 0 event, 124 timeout, 1 error).
    Wait {
        #[arg(long)]
        room: String,
        #[arg(long)]
        after: i64,
        #[arg(long, default_value_t = soudan::wait::DEFAULT_TIMEOUT)]
        timeout: u64,
    },
    #[command(hide = true)]
    WaitObserver { watch: String, timeout: u64 },
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
    /// Wait until receipt is no longer waiting; unknown is inconclusive, not success.
    Wait {
        #[arg(long)]
        request_id: String,
        #[arg(long, default_value_t = soudan::wait::DEFAULT_TIMEOUT)]
        timeout: u64,
    },
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
    // Waits must bypass App::open: it creates state and enables WAL.
    let waiting = match &cli.command {
        Commands::Wait {
            room,
            after,
            timeout,
        } => Some((
            soudan::wait::Watch::Room {
                room: room.clone(),
                after: *after,
            },
            *timeout,
        )),
        Commands::Live {
            command:
                LiveCommands::Wait {
                    request_id,
                    timeout,
                },
        } => Some((
            soudan::wait::Watch::Delivery {
                request_id: request_id.clone(),
            },
            *timeout,
        )),
        _ => None,
    };
    if let Some((watch, timeout)) = waiting {
        let value = soudan::wait::supervise(&cli.workspace, &watch, timeout).await;
        println!("{}", serde_json::to_string(&value)?);
        std::process::exit(soudan::wait::exit_code(&value));
    }
    if let Commands::WaitObserver { watch, timeout } = &cli.command {
        let result = async {
            let watch = serde_json::from_str(watch)?;
            soudan::wait::observe(&cli.workspace, &watch, *timeout).await
        }
        .await;
        let value = result.unwrap_or_else(|e| soudan::wait::error(format!("{e:#}")));
        println!("{}", serde_json::to_string(&value)?);
        return Ok(());
    }
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
        Commands::Wait { .. } | Commands::WaitObserver { .. } => unreachable!(),
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
            LiveCommands::Wait { .. } => unreachable!(),
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
