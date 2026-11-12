//! Astrum: a persistent terminal session manager with tabs, mouse support
//! and a built-in terminal agent. Runs inside any terminal emulator.

mod agent;
mod client;
mod config;
mod image;
mod input;
mod keychain;
mod keys;
mod osc;
mod pane;
mod paths;
mod procinfo;
mod proto;
mod server;
mod service;
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
    /// Stop a session and close its shells. Its tabs come back the next
    /// time it is started, unless --forget is given
    Kill {
        /// Stop every session
        #[arg(long)]
        all: bool,
        /// Also discard the saved tabs, so the session starts afresh
        #[arg(long)]
        forget: bool,
    },
    /// Show the config file location; --init writes a commented starter file
    Config {
        #[arg(long)]
        init: bool,
    },
    /// List the built-in color themes
    Themes,
    /// List what Astrum's keys do, with the keys each has now
    Keys,
    /// Print a completion script for a shell
    Completions {
        /// bash, zsh or fish
        shell: String,
    },
    /// Print the manual page
    Man,
    /// Have a running session re-read its providers from the config file,
    /// the keychain and this shell's environment, without restarting
    Reload,
    /// Run the session as a login service: started when you log in and
    /// restarted if it stops (on Linux it can also outlast a logout)
    Service {
        #[command(subcommand)]
        action: ServiceCmd,
    },
    #[command(name = "__server", hide = true)]
    Server {
        /// Started by a service manager rather than by a client.
        #[arg(long)]
        service: bool,
    },
    /// Print what a session has spent on reading output and drawing (for measuring)
    #[command(name = "debug-work", hide = true)]
    Work,
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

#[derive(Subcommand)]
enum ServiceCmd {
    /// Set the service up and start it
    Install {
        /// Only show what would be written, and where
        #[arg(long)]
        print: bool,
    },
    /// Stop the service and remove it
    Uninstall,
    /// Say whether the service is installed and running
    Status,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_completions_and_the_manual_name_every_command() {
        let cli = Cli::command();
        let commands: Vec<String> = cli.get_subcommands().filter(|c| !c.is_hide_set()).map(|c| c.get_name().to_string()).collect();
        assert!(commands.len() >= 10, "{commands:?}");
        let texts = [
            ("bash", include_str!("../completions/astrum.bash")),
            ("zsh", include_str!("../completions/_astrum")),
            ("fish", include_str!("../completions/astrum.fish")),
            ("man", include_str!("../doc/astrum.1")),
        ];
        for (what, text) in texts {
            for command in &commands {
                assert!(text.contains(command.as_str()), "{what} does not mention `{command}`");
            }
            // And the options of the commands that have them.
            for option in ["all", "forget", "init", "print", "session"] {
                assert!(text.contains(option), "{what} does not mention --{option}");
            }
        }
    }
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
        Cmd::Server { service } => tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(anyhow::Error::from)
            .and_then(|rt| rt.block_on(server::serve(cli.session.clone(), service))),
        Cmd::Service { action } => match action {
            ServiceCmd::Install { print: true } => service::preview(&cli.session),
            ServiceCmd::Install { print: false } => service::install(&cli.session),
            ServiceCmd::Uninstall => service::uninstall(&cli.session),
            ServiceCmd::Status => service::status(&cli.session),
        }
        .map(|report| println!("{report}")),
        Cmd::Work => match client::info(&cli.session) {
            Some(info) => serde_json::to_string(&info.work).map(|work| println!("{work}")).map_err(Into::into),
            None => Err(anyhow::anyhow!("Session `{}` is not running.", cli.session)),
        },
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
                        // Nothing is listening on that socket any more: it is
                        // left over. (One that can be connected to belongs to
                        // a session that was slow to answer, and stays.)
                        if client::connect(&name).is_none() {
                            if let Ok(path) = paths::socket_path(&name) {
                                let _ = std::fs::remove_file(path);
                            }
                        }
                    }
                }
            }
            // Sessions that are not running but would come back if started.
            let running = client::session_names();
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            let mut saved_any = false;
            for (name, saved) in server::restore::list() {
                if running.contains(&name) || !valid_session(&name) {
                    continue;
                }
                saved_any = true;
                let start = if name == "default" { "astrum".to_string() } else { format!("astrum -s {name}") };
                println!(
                    "{:<16} {} tab{}  stopped   saved {}  (`{start}` brings {} back)",
                    name,
                    saved.tabs.len(),
                    if saved.tabs.len() == 1 { "" } else { "s" },
                    server::store::ago(saved.saved, now),
                    if saved.tabs.len() == 1 { "it" } else { "them" },
                );
            }
            if !any && !saved_any {
                println!("No sessions are running. Start one with `astrum`.");
            } else if !any {
                println!("No sessions are running.");
            }
            Ok(())
        }
        Cmd::Kill { all, forget } => {
            let mut targets = if all { client::session_names() } else { vec![cli.session.clone()] };
            if all && forget {
                // Stopped sessions have saved tabs to discard too.
                for (name, _) in server::restore::list() {
                    if valid_session(&name) && !targets.contains(&name) {
                        targets.push(name);
                    }
                }
            }
            for name in targets {
                let stopped = client::kill(&name);
                let forgot = forget && server::restore::forget(&name);
                match (stopped, forgot) {
                    (true, true) => println!("Stopped session `{name}` and discarded its saved tabs."),
                    (true, false) => println!("Stopped session `{name}`."),
                    (false, true) => println!("Discarded the saved tabs of session `{name}`."),
                    (false, false) if !all => println!("Session `{name}` is not running."),
                    _ => {}
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
        Cmd::Completions { shell } => match shell.to_ascii_lowercase().as_str() {
            "bash" => Ok(print!("{}", include_str!("../completions/astrum.bash"))),
            "zsh" => Ok(print!("{}", include_str!("../completions/_astrum"))),
            "fish" => Ok(print!("{}", include_str!("../completions/astrum.fish"))),
            other => Err(anyhow::anyhow!("no completions for `{other}`: there are scripts for bash, zsh and fish")),
        },
        Cmd::Man => Ok(print!("{}", include_str!("../doc/astrum.1"))),
        Cmd::Keys => {
            let (cfg, mut warnings) = config::Config::load();
            let (keymap, more) = keys::Keymap::new(&cfg.keys);
            warnings.extend(more);
            let prefix = config::Chord::parse(&cfg.prefix).map(|c| c.label()).unwrap_or_else(|| "^B".into());
            println!("{:<16} {:<22} {}", "ACTION", "KEYS", "WHAT IT DOES");
            for (name, action, what) in keys::ACTIONS {
                let specs = keymap.specs(*action);
                let shown = if specs.is_empty() { "none".to_string() } else { specs.join(", ") };
                println!("{name:<16} {shown:<22} {what}");
            }
            println!("\nA plain key is pressed after the prefix ({prefix}); one with ctrl or alt works by itself.");
            println!("Change them under [keys] in {}", paths::config_file().display());
            for w in warnings {
                eprintln!("astrum: config: {w}");
            }
            Ok(())
        }
        Cmd::Themes => {
            let (cfg, _) = config::Config::load();
            let (themes, warnings) = theme::all_with(&cfg.themes);
            for t in themes {
                let own = if cfg.themes.keys().any(|k| k.to_ascii_lowercase().replace([' ', '_'], "-") == t.name) { "  (yours)" } else { "" };
                println!("{:<18} {}{own}", t.name, t.label);
            }
            for w in warnings {
                eprintln!("astrum: config: {w}");
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
