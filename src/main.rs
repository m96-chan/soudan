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
    /// Open a read-only dashboard on 127.0.0.1 only. No default port.
    Web {
        #[arg(long)]
        port: u16,
    },
    #[command(hide = true)]
    WebObserver { query: String },
    /// Wait for room messages without consuming them (exit 0 event, 124 timeout, 1 error).
    Wait {
        #[arg(long, required_unless_present = "reply_to")]
        room: Option<String>,
        #[arg(long, required_unless_present = "reply_to")]
        after: Option<i64>,
        #[arg(long)]
        reply_to: Option<String>,
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
        #[arg(long)]
        in_reply_to: Option<String>,
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
    List,
    Read {
        target: String,
    },
    Send {
        target: String,
        #[arg(long)]
        sender: Option<String>,
        #[arg(long)]
        request_id: String,
        text: String,
    },
}
#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Commands::Web { port } = &cli.command {
        return soudan::web::serve(&cli.workspace, *port).await;
    }
    if let Commands::WebObserver { query } = &cli.command {
        let result = serde_json::from_str(query)
            .map_err(anyhow::Error::from)
            .and_then(|q| soudan::web::snapshot(&cli.workspace, q));
        let value =
            result.unwrap_or_else(|e| serde_json::json!({"observer_error":format!("{e:#}")}));
        println!("{}", serde_json::to_string(&value)?);
        return Ok(());
    }
    // Waits must bypass App::open: it creates state and enables WAL.
    let waiting = match &cli.command {
        Commands::Wait {
            room,
            after,
            reply_to,
            timeout,
        } => Some((
            if let Some(request_id) = reply_to {
                soudan::wait::Watch::Reply {
                    request_id: request_id.clone(),
                    room: room.clone(),
                    after: after.unwrap_or(0),
                }
            } else {
                soudan::wait::Watch::Room {
                    room: room.clone().expect("required by clap"),
                    after: after.expect("required by clap"),
                }
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
        Commands::Web { .. }
        | Commands::WebObserver { .. }
        | Commands::Wait { .. }
        | Commands::WaitObserver { .. } => unreachable!(),
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
            LiveCommands::List => serde_json::to_value(soudan::live::discover(&app.workspace)?)?,
            LiveCommands::Read { target } => soudan::live::read(&app.workspace, &target).await?,
            LiveCommands::Send {
                sender,
                target,
                request_id,
                text,
            } => {
                soudan::live::send_as(
                    &app.workspace,
                    &target,
                    &text,
                    &request_id,
                    sender.as_deref(),
                )
                .await?
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
        Commands::Post {
            room,
            sender,
            text,
            in_reply_to,
        } => {
            serde_json::json!({"id":app.store.post_reply(&room,&sender,&text,None,in_reply_to.as_deref())?})
        }
        Commands::History { room, after } => {
            serde_json::to_value(app.store.history(&room, after)?)?
        }
        Commands::Install { .. } => unreachable!(),
    };
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
