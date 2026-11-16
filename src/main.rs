//! Astrum: a persistent terminal session manager with tabs, mouse support
//! and a built-in terminal agent. Runs inside any terminal emulator.

mod agent;
mod client;
mod config;
mod git;
mod image;
mod inner;
mod input;
mod keychain;
mod keys;
mod mcp;
mod osc;
mod pane;
mod paths;
mod procinfo;
mod proto;
mod remote;
mod server;
mod service;
mod signal;
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

    /// Attach to a running session to watch only: nothing typed, pasted or
    /// clicked in this terminal reaches it
    #[arg(long)]
    watch: bool,

    /// Open a layout in the session as this terminal attaches: the tabs,
    /// splits and commands of ~/.config/astrum/layouts/NAME.toml
    #[arg(long, value_name = "NAME", conflicts_with = "watch")]
    layout: Option<String>,

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
    Themes {
        /// Their colors as well, as JSON (for the website's gallery)
        #[arg(long)]
        json: bool,
    },
    /// List what Astrum's keys do, with the keys each has now
    Keys {
        /// As JSON (for the website)
        #[arg(long)]
        json: bool,
    },
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
    /// Move a running session onto this Astrum (after installing a new
    /// version), keeping everything in it running
    Upgrade {
        /// Even if the session already runs this version
        #[arg(long)]
        force: bool,
        /// Every running session, not only the one named
        #[arg(long)]
        all: bool,
    },
    /// List the layouts there are (`astrum --layout NAME` opens one)
    Layouts,
    /// Open a pane in a running session and run a command in it; prints
    /// the pane's id
    Run {
        /// In a new tab (the default)
        #[arg(long, conflicts_with = "split")]
        new_tab: bool,
        /// In a split: `right` of, or `down` from, the pane it is run in
        /// (or `--pane`)
        #[arg(long, value_parser = ["right", "down"])]
        split: Option<String>,
        /// In a pane floating over the tab in front
        #[arg(long, conflicts_with_all = ["split", "new_tab"])]
        float: bool,
        /// The pane to split, by its id (`astrum list`)
        #[arg(long)]
        pane: Option<u64>,
        /// Wait for the command to end, and exit with its exit code
        #[arg(long)]
        wait: bool,
        /// Leave the pane up when the command ends, until Enter
        #[arg(long)]
        keep: bool,
        /// Move the keyboard to the new pane
        #[arg(long)]
        focus: bool,
        /// The command and its arguments, after `--`
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Type text into a pane of a running session
    Send {
        /// The pane, by its id; the one this is run in, or the one the
        /// keyboard is in, otherwise
        #[arg(long)]
        pane: Option<u64>,
        /// Press Enter after it
        #[arg(long)]
        enter: bool,
        text: String,
    },
    /// Print what a pane of a running session shows
    Capture {
        #[arg(long)]
        pane: Option<u64>,
        /// Everything the pane has kept, not only its screen
        #[arg(long)]
        history: bool,
    },
    /// List a running session's tabs and panes
    List {
        #[arg(long)]
        json: bool,
    },
    /// Say how a script is doing, for the pane it runs in: working, done,
    /// blocked (needs you), error, or clear
    Status {
        #[arg(value_parser = ["idle", "working", "done", "blocked", "error", "clear"])]
        state: String,
        message: Option<String>,
        #[arg(long)]
        pane: Option<u64>,
        /// How far it has got, 0 to 100
        #[arg(long)]
        progress: Option<u8>,
        /// With `blocked`: permission, question or auth
        #[arg(long, value_parser = ["permission", "question", "auth"])]
        kind: Option<String>,
    },
    #[command(name = "__server", hide = true)]
    Server {
        /// Started by a service manager rather than by a client.
        #[arg(long)]
        service: bool,
        /// Take over the session a server before this one wrote down here.
        #[arg(long)]
        take: Option<String>,
    },
    /// Say whether this program can take a running session over
    #[command(name = "__takes", hide = true)]
    Takes { file: Option<String> },
    /// Keep shells for sessions on other machines (started by `__link`)
    #[command(name = "__hold", hide = true)]
    Hold,
    /// Join `ssh`, run from the user's own machine, to the keeper of shells here
    #[command(name = "__link", hide = true)]
    Link {
        /// Only print the number of the language this version speaks
        #[arg(long)]
        speaks: bool,
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

pub fn valid_session(name: &str) -> bool {
    !name.is_empty() && name.len() <= 40 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Ask something of the session and print the answer. Run inside a
/// session, with no other named, it is that session that is asked.
fn control(session: &str, named: bool, what: impl FnOnce(&str) -> proto::Control, show: impl FnOnce(String) -> String) -> anyhow::Result<()> {
    let inside = std::env::var("ASTRUM_SESSION").ok().filter(|s| !named && valid_session(s));
    let session = inside.as_deref().unwrap_or(session);
    let (ok, text, code) = client::control(session, what(session))?;
    if !ok {
        anyhow::bail!("{text}");
    }
    let text = show(text);
    if !text.is_empty() {
        print!("{text}{}", if text.ends_with('\n') { "" } else { "\n" });
    }
    if let Some(code) = code.filter(|c| *c != 0) {
        std::process::exit(code);
    }
    Ok(())
}

fn main() {
    use clap::{CommandFactory, FromArgMatches};
    let matches = Cli::command().get_matches();
    let cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    // (Was a session named, or is it only the default?)
    let named = matches.value_source("session") == Some(clap::parser::ValueSource::CommandLine);
    if !valid_session(&cli.session) {
        eprintln!("astrum: session names may only contain letters, digits, '-' and '_'");
        std::process::exit(2);
    }
    let result: anyhow::Result<()> = match cli.command.unwrap_or(Cmd::Attach) {
        Cmd::Attach => client::attach(&cli.session, cli.watch, cli.layout.clone()).map(|msg| println!("{msg}")),
        Cmd::Run { new_tab: _, split, float, pane, wait, keep, focus, command } => {
            let place = match split.as_deref() {
                _ if float => proto::Place::Float,
                Some("right") => proto::Place::Right,
                Some("down") => proto::Place::Down,
                _ => proto::Place::NewTab,
            };
            let cwd = std::env::current_dir().ok().map(|p| p.to_string_lossy().into_owned());
            control(&cli.session, named, |s| proto::Control::Run { command, cwd, place, pane: pane.or_else(|| client::own_pane(s)), wait, keep, focus }, |id| if wait { String::new() } else { id })
        }
        Cmd::Send { pane, enter, text } => control(&cli.session, named, |s| proto::Control::Send { pane: pane.or_else(|| client::own_pane(s)), text, enter }, |t| t),
        Cmd::Capture { pane, history } => control(&cli.session, named, |s| proto::Control::Capture { pane: pane.or_else(|| client::own_pane(s)), history }, |t| t),
        Cmd::List { json } => control(&cli.session, named, |_| proto::Control::List, |t| if json { t } else { client::listing_text(&t) }),
        Cmd::Status { state, message, pane, progress, kind } => {
            control(&cli.session, named, |s| proto::Control::Status { pane: pane.or_else(|| client::own_pane(s)), state, message: message.unwrap_or_default(), progress, kind }, |t| t)
        }
        Cmd::Layouts => {
            let names = server::layouts::names();
            for name in &names {
                match server::layouts::load(name) {
                    Ok(layout) => println!("{name:<16} {} tab{}", layout.tabs.len(), if layout.tabs.len() == 1 { "" } else { "s" }),
                    Err(why) => println!("{name:<16} {why}"),
                }
            }
            if names.is_empty() {
                println!("No layouts yet. One is a file in {}; right-click a tab → Save this tab as a layout… writes one.", paths::tildify(&server::layouts::dir().to_string_lossy()));
            }
            Ok(())
        }
        Cmd::Takes { file } => server::handover::takes(file.as_deref()),
        Cmd::Upgrade { force, all } => {
            let names = if all { client::session_names().into_iter().filter(|n| client::connect(n).is_some()).collect() } else { vec![cli.session.clone()] };
            let mut failed = None;
            for name in &names {
                match client::upgrade(name, force) {
                    Ok(said) => println!("{said}"),
                    Err(why) if all => {
                        eprintln!("astrum: {why:#}");
                        failed = Some(anyhow::anyhow!("not every session was moved"));
                    }
                    Err(why) => failed = Some(why),
                }
            }
            if names.is_empty() {
                println!("No session is running.");
            }
            failed.map_or(Ok(()), Err)
        }
        Cmd::Server { service, take } => tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(anyhow::Error::from)
            .and_then(|rt| rt.block_on(server::serve(cli.session.clone(), service, take))),
        Cmd::Hold => {
            // What the connection that happened to start this left in the
            // environment says nothing about the ones that come after.
            for name in ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY", "SSH_AUTH_SOCK", "SSH_ORIGINAL_COMMAND"] {
                unsafe { std::env::remove_var(name) };
            }
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(anyhow::Error::from)
                .and_then(|rt| rt.block_on(remote::hold::hold()))
        }
        Cmd::Link { speaks: true } => Ok(println!("{}", remote::LINK)),
        Cmd::Link { speaks: false } => remote::hold::link(),
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
        Cmd::Keys { json } => {
            let (cfg, mut warnings) = config::Config::load();
            let (keymap, more) = keys::Keymap::new(&cfg.keys);
            warnings.extend(more);
            let prefix = config::Chord::parse(&cfg.prefix).map(|c| c.label()).unwrap_or_else(|| "^B".into());
            if json {
                let list: Vec<serde_json::Value> = keys::ACTIONS.iter().map(|(name, action, what)| serde_json::json!({"name": name, "keys": keymap.specs(*action), "label": keymap.label(*action, &prefix), "what": what})).collect();
                println!("{}", serde_json::json!({"prefix": cfg.prefix, "agent_key": cfg.agent_key, "actions": list}));
                return;
            }
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
        Cmd::Themes { json: true } => {
            let (cfg, _) = config::Config::load();
            let (themes, _) = theme::all_with(&cfg.themes);
            println!("{}", theme::as_json(&themes));
            Ok(())
        }
        Cmd::Themes { json: false } => {
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
