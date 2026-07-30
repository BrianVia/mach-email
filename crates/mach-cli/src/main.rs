use anyhow::Result;
use clap::{Parser, Subcommand};

mod commands;
mod runtime;

/// `mach` — a fast, vim-flavored personal email client.
///
/// Run with no arguments to launch the TUI. All subcommands map onto the
/// same `Action` set the TUI dispatches, so anything you can do interactively
/// you can also do from a shell pipe or an MCP-connected agent.
#[derive(Parser, Debug)]
#[command(name = "mach", version, about, long_about = None)]
struct Cli {
    /// Limit reads and actions to one Gmail account. Omit for the unified inbox.
    #[arg(long, global = true)]
    account: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Dispatch a single Action as JSON; emit the outcome as JSON. Designed
    /// for shell scripts and LLM agents calling out via tool use.
    Do {
        /// Action JSON, e.g. `{"kind":"search","query":"is:unread","limit":10}`.
        /// Pass `-` to read from stdin.
        action: String,
    },
    /// Launch the MCP server over stdio. Used by Claude Desktop and friends.
    Mcp,
    /// OAuth flows.
    #[command(subcommand)]
    Auth(AuthCommand),
    /// Run a sync pass once and exit.
    Sync {
        /// Force a full bootstrap instead of incremental history sync.
        #[arg(long)]
        bootstrap: bool,
    },
    /// Diagnostics. Use to verify environment and exercise rare code paths.
    Doctor {
        /// Wipe the local sync cursor to force gap recovery on next sync.
        #[arg(long)]
        simulate_gap: bool,
    },
}

#[derive(Subcommand, Debug)]
enum AuthCommand {
    /// Run the installed-app OAuth flow and store credentials in the app data directory.
    Login,
    /// Print stored accounts and token expiry.
    Status,
    /// Forget stored credentials.
    Logout {
        /// Remove every stored Gmail login.
        #[arg(long)]
        all: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Walks up from CWD looking for `.env`. Silent if not found —
    // env vars set in the shell still win.
    let _ = dotenvy::dotenv();

    if let Err(e) = color_eyre::install() {
        eprintln!("warning: failed to install color_eyre panic hook: {e}");
    }
    let cli = Cli::parse();
    init_tracing(cli.command.is_none());
    let account = cli.account.as_deref();
    match cli.command {
        None => commands::tui::run(account).await,
        Some(Command::Do { action }) => commands::do_cmd::run(&action, account).await,
        Some(Command::Mcp) => commands::mcp::run(account).await,
        Some(Command::Auth(AuthCommand::Login)) => commands::auth::login().await,
        Some(Command::Auth(AuthCommand::Status)) => commands::auth::status(account).await,
        Some(Command::Auth(AuthCommand::Logout { all })) => {
            commands::auth::logout(account, all).await
        }
        Some(Command::Sync { bootstrap }) => commands::sync::run(bootstrap, account).await,
        Some(Command::Doctor { simulate_gap }) => {
            commands::doctor::run(simulate_gap, account).await
        }
    }
}

fn init_tracing(tui_mode: bool) {
    use std::fs::{self, OpenOptions};
    use std::sync::Mutex;

    use directories::ProjectDirs;
    use tracing_subscriber::{fmt, EnvFilter};

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("warn,mach=info,refinery=warn"));
    if tui_mode {
        let log_file = ProjectDirs::from("com", "via", "mach").and_then(|dirs| {
            let directory = dirs.data_dir();
            fs::create_dir_all(directory).ok()?;
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(directory.join("mach.log"))
                .ok()
        });
        if let Some(log_file) = log_file {
            fmt()
                .with_env_filter(filter)
                .with_target(false)
                .with_ansi(false)
                .with_writer(Mutex::new(log_file))
                .init();
        } else {
            fmt()
                .with_env_filter(filter)
                .with_target(false)
                .with_writer(std::io::sink)
                .init();
        }
    } else {
        fmt()
            .with_env_filter(filter)
            .with_target(false)
            .with_writer(std::io::stderr)
            .init();
    }
}
