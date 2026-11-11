//! Astrum: a persistent terminal session manager with tabs, mouse support
//! and a built-in terminal agent. Runs inside any terminal emulator.

mod agent;
mod client;
mod config;
mod input;
mod keychain;
mod osc;
mod pane;
mod paths;
mod procinfo;
mod proto;
mod server;
mod snapshot;
mod theme;
mod ui;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "astrum", version, about = "A persistent terminal with tabs, mouse support and a built-in AI agent", long_about = None)]
struct Cli {
    /// Session name (each session has its own tabs and keeps running in the background)
    #[arg(short, long, global = true, default_value = "default")]
    session: String,

    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Attach to the session, starting it if needed (the default)
    Attach,
    /// List running sessions
    Ls,
    /// Stop a session and close its shells
    Kill {
        /// Stop every session
        #[arg(long)]
        all: bool,
    },
    /// Show the config file location; --init writes a commented starter file
    Config {
        #[arg(long)]
        init: bool,
    },
    /// List the built-in color themes
    Themes,
    /// Have a running session re-read its providers from the config file,
    /// the keychain and this shell's environment, without restarting
    Reload,
    #[command(name = "__server", hide = true)]
    Server,
    /// Drive a session without a terminal and print the screen (for testing)
    #[command(name = "debug-snapshot", hide = true)]
    Snapshot {
        #[arg(long, default_value_t = 100)]
        cols: u16,
        #[arg(long, default_value_t = 30)]
        rows: u16,
        /// Actions: text:…, line:…, key:ctrl+b, click:C,R, wait:MS, until:TEXT, snap:LABEL, …
        #[arg(long = "do")]
        actions: Vec<String>,
        #[arg(long, default_value_t = 400)]
        wait_ms: u64,
        /// Also write the final screen as an HTML page
        #[arg(long)]
        html: Option<String>,
    },
}

fn valid_session(name: &str) -> bool {
    !name.is_empty() && name.len() <= 40 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn main() {
    let cli = Cli::parse();
    if !valid_session(&cli.session) {
        eprintln!("astrum: session names may only contain letters, digits, '-' and '_'");
        std::process::exit(2);
    }
    let result: anyhow::Result<()> = match cli.command.unwrap_or(Cmd::Attach) {
        Cmd::Attach => client::attach(&cli.session).map(|msg| println!("{msg}")),
        Cmd::Server => tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(anyhow::Error::from)
            .and_then(|rt| rt.block_on(server::serve(cli.session.clone()))),
        Cmd::Ls => {
            let mut any = false;
            for name in client::session_names() {
                match client::info(&name) {
                    Some(info) => {
                        any = true;
                        let age = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs().saturating_sub(info.started_unix))
                            .unwrap_or(0);
                        let age = if age >= 3600 { format!("{}h{}m", age / 3600, age % 3600 / 60) } else { format!("{}m", age / 60) };
                        println!(
                            "{:<16} {} tab{}  {}  up {}  [{}]",
                            info.session,
                            info.tabs.len(),
                            if info.tabs.len() == 1 { "" } else { "s" },
                            if info.clients > 0 { "attached" } else { "detached" },
                            age,
                            info.tabs.join(", ")
                        );
                    }
                    None => {
                        // Nothing answers on that socket any more.
                        if let Ok(path) = paths::socket_path(&name) {
                            let _ = std::fs::remove_file(path);
                        }
                    }
                }
            }
            if !any {
                println!("No sessions are running. Start one with `astrum`.");
            }
            Ok(())
        }
        Cmd::Kill { all } => {
            let targets = if all { client::session_names() } else { vec![cli.session.clone()] };
            for name in targets {
                if client::kill(&name) {
                    println!("Stopped session `{name}`.");
                } else if !all {
                    println!("Session `{name}` is not running.");
                }
            }
            Ok(())
        }
        Cmd::Config { init } => {
            let path = paths::config_file();
            if init && !path.exists() {
                std::fs::create_dir_all(paths::config_dir())
                    .and_then(|_| std::fs::write(&path, config::SAMPLE))
                    .map(|_| println!("Wrote {}", path.display()))
                    .map_err(anyhow::Error::from)
            } else {
                println!("{}{}", path.display(), if path.exists() { "" } else { "  (not created yet; run `astrum config --init`)" });
                Ok(())
            }
        }
        Cmd::Reload => client::reload(&cli.session).map(|report| println!("{report}")),
        Cmd::Themes => {
            for t in theme::all() {
                println!("{:<18} {}", t.name, t.label);
            }
            Ok(())
        }
        Cmd::Snapshot { cols, rows, actions, wait_ms, html } => {
            snapshot::run(snapshot::Opts { session: cli.session.clone(), cols, rows, actions, wait_ms, html })
        }
    };
    if let Err(e) = result {
        eprintln!("astrum: {e:#}");
        std::process::exit(1);
    }
}
