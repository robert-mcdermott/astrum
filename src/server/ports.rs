//! Ports on other machines, used from this one: the server's side of
//! `crate::remote::ports`.
//!
//! `/forward 3000` in a tab on another machine opens a port on this one
//! (the same number, if it is free) and carries every connection made to
//! it to port 3000 there, over the connection the tab already has. A
//! program there that listens only on that machine itself, a development
//! server say, is then at `http://localhost:3000` here.
//!
//! The port here is open only to this machine (it listens on the loopback
//! addresses), and belongs to the machine's tabs: it closes with the last
//! of them. While the connection is down, connections made to it are
//! closed at once; when it is back, they are carried again.

use super::{Ev, Menu, MenuAction, Overlay, Server};
use crate::remote::{Answer, Ask};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;
use tokio::task::AbortHandle;

/// A port there, and the port here that stands for it.
pub struct Port {
    pub id: u64,
    /// The machine, as the user named it to `ssh`.
    pub host: String,
    pub there: u16,
    pub here: u16,
    /// What listens here.
    tasks: Vec<AbortHandle>,
}

impl Port {
    /// Where a browser on this machine finds it.
    pub fn address(&self) -> String {
        format!("http://localhost:{}", self.here)
    }
}

impl Drop for Port {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Is something on this machine answering on `port` already? (Asked by
/// trying it, on both of the machine's own addresses: a port can be
/// opened beside one that is listening on every address, and the
/// connections meant for that one would then come here.)
fn taken(port: u16) -> bool {
    let both = [SocketAddr::from((Ipv4Addr::LOCALHOST, port)), SocketAddr::from((Ipv6Addr::LOCALHOST, port))];
    both.iter().any(|addr| std::net::TcpStream::connect_timeout(addr, Duration::from_millis(200)).is_ok())
}

/// Listen on `port` of this machine (0: any that is free), for this
/// machine only. Both of its own addresses where both can be had, since
/// `localhost` may mean either.
fn listen(port: u16) -> Option<(u16, Vec<std::net::TcpListener>)> {
    if port != 0 && taken(port) {
        return None;
    }
    let v4 = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).ok()?;
    let port = v4.local_addr().ok()?.port();
    let mut all = vec![v4];
    all.extend(std::net::TcpListener::bind((Ipv6Addr::LOCALHOST, port)).ok());
    Some((port, all))
}

/// The ports to try here for `wanted`, in order: itself, then the ones
/// after it (for a port only root may open, the ones after its usual
/// stand-in: 8080 for 80), then whatever is free.
fn candidates(wanted: u16) -> Vec<u16> {
    let from = if wanted < 1024 { wanted + 8000 } else { wanted };
    let mut all = vec![wanted];
    all.extend((0..=20u16).filter_map(|n| from.checked_add(n)).filter(|p| *p != wanted));
    all.push(0);
    all
}

/// The port a word names: a number, or the one an address ends in
/// (`localhost:5173`, `http://127.0.0.1:8000/docs`).
pub(super) fn port_of(word: &str) -> Option<u16> {
    let word = word.split_once("://").map_or(word, |(_, rest)| rest);
    let word = word.split('/').next().unwrap_or(word);
    let digits = word.rsplit(':').next().unwrap_or(word);
    digits.parse().ok().filter(|p| *p != 0)
}

const USAGE: &str = "`/forward 3000` in a tab on another machine makes port 3000 there reachable on this machine, as http://localhost:3000. `/forward 3000 stop` ends it, and `/forward` alone lists the ports forwarded.";

impl Server {
    /// The other machine tab `idx` is working on, if it is on one.
    fn far_of_tab(&self, idx: usize) -> Option<String> {
        let panes = &self.tabs.get(idx)?.panes;
        panes.focused().there().or_else(|| panes.agent().there()).map(|t| t.host.clone())
    }

    fn far_name(&self, host: &str) -> String {
        self.far(host).map(|f| f.name()).unwrap_or_else(|| host.to_string())
    }

    /// The ports forwarded from the machine the keyboard's pane is on.
    pub fn ports_in_front(&self) -> Vec<&Port> {
        let host = self.far_of_tab(self.active);
        self.ports.iter().filter(|p| Some(&p.host) == host.as_ref()).collect()
    }

    /// `/forward`, with whatever followed it.
    pub(super) fn forward_command(&mut self, idx: usize, arg: &str) {
        let words: Vec<&str> = arg.split_whitespace().collect();
        let stop = |w: &str| matches!(w.to_ascii_lowercase().as_str(), "stop" | "off" | "remove" | "close");
        let host = self.far_of_tab(idx);
        match (words.as_slice(), host) {
            ([], _) => {
                let text = self.ports_listed();
                self.notice(idx, text);
            }
            ([word], host) if stop(word) => {
                let ids: Vec<u64> = self.ports.iter().filter(|p| host.as_ref().is_none_or(|h| *h == p.host)).map(|p| p.id).collect();
                if ids.is_empty() {
                    self.notice(idx, "No ports are forwarded.");
                }
                for id in ids {
                    self.stop_port(Some(idx), id);
                }
            }
            (_, None) => self.notice(
                idx,
                format!("This tab's shell is on this machine, so there is no port to bring here. {USAGE}\n\nA tab on another machine: `≡ menu` → New tab on another machine…, or `/ssh host`."),
            ),
            ([port, word], Some(host)) if stop(word) => match port_of(port).and_then(|n| self.ports.iter().find(|p| p.host == host && (p.there == n || p.here == n)).map(|p| p.id)) {
                Some(id) => self.stop_port(Some(idx), id),
                None => self.notice(idx, format!("Port {port} is not forwarded from {}.", self.far_name(&host))),
            },
            ([port], Some(host)) | ([port, _], Some(host)) => {
                let here = words.get(1).map(|w| port_of(w));
                match (port_of(port), here) {
                    (Some(there), None) => self.forward_port(Some(idx), &host, there, None),
                    (Some(there), Some(Some(here))) => self.forward_port(Some(idx), &host, there, Some(here)),
                    _ => self.notice(idx, format!("A port is a number from 1 to 65535. {USAGE}")),
                }
            }
            _ => self.notice(idx, USAGE),
        }
    }

    fn ports_listed(&self) -> String {
        if self.ports.is_empty() {
            return format!("No ports are forwarded. {USAGE}");
        }
        let mut text = "**Forwarded ports**\n".to_string();
        for port in &self.ports {
            let up = self.far(&port.host).is_some_and(|f| f.way.is_some());
            text.push_str(&format!("\n- Port {} on {} is {} here{}", port.there, self.far_name(&port.host), port.address(), if up { "" } else { " (not connected at the moment)" }));
        }
        text.push_str("\n\n`/forward 3000 stop` ends one; closing the machine's last tab ends them all.");
        text
    }

    /// Say something about a port: in the tab's panel where one asked,
    /// and where the mouse did, at the top of the window.
    fn port_said(&mut self, idx: Option<usize>, text: String) {
        match idx.filter(|i| *i < self.tabs.len()) {
            Some(idx) => self.notice(idx, text),
            None => self.toast(text),
        }
    }

    /// Open a port here for port `there` on `host`: `here` if given and
    /// free, the same number if that is, or one that is.
    pub(super) fn forward_port(&mut self, idx: Option<usize>, host: &str, there: u16, here: Option<u16>) {
        let name = self.far_name(host);
        let Some(far) = self.far(host) else { return };
        if far.there.is_none() {
            return self.port_said(idx, format!("{name} has not been reached yet. Forward the port once its shell is there."));
        }
        if !far.carries_ports {
            let version = far.version.clone().unwrap_or_else(|| "an earlier version".into());
            return self.port_said(
                idx,
                format!(
                    "The Astrum on {name} ({version}) is from before ports could be forwarded. Update it there (`astrum upgrade` in a shell on it), close the tabs on {name}, wait half a minute for the older one to stop, and open the tab again."
                ),
            );
        }
        if let Some(port) = self.ports.iter().find(|p| p.host == host && p.there == there) {
            return self.port_said(idx, format!("Port {there} on {name} is already forwarded: it is {} on this machine.", port.address()));
        }
        // A machine reached from itself (`ssh localhost`, to try this out)
        // has one set of ports, not two: the port here cannot be the one
        // it stands for, or every connection to it would lead back to it.
        let mark = crate::remote::mark();
        let same = !mark.is_empty() && far.there.as_ref().is_some_and(|m| m.mark == mark);
        let wanted = here.unwrap_or(there);
        let Some((here, listeners)) = candidates(wanted).into_iter().filter(|p| !(same && *p == there)).find_map(listen) else {
            return self.port_said(idx, format!("No port could be opened on this machine for port {there} on {name}."));
        };
        let id = self.next_port;
        self.next_port += 1;
        let mut tasks = Vec::new();
        for listener in listeners {
            let tx = self.tx.clone();
            let ready = listener.set_nonblocking(true).and_then(|_| tokio::net::TcpListener::from_std(listener));
            let Ok(listener) = ready else { continue };
            tasks.push(
                tokio::spawn(async move {
                    loop {
                        match listener.accept().await {
                            Ok((socket, _)) => {
                                if tx.send(Ev::Accepted { port: id, socket }).await.is_err() {
                                    break;
                                }
                            }
                            // (Too many files open, say: not the end of listening.)
                            Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
                        }
                    }
                })
                .abort_handle(),
            );
        }
        let port = Port { id, host: host.to_string(), there, here, tasks };
        let moved = match here == wanted {
            true => String::new(),
            false if same && wanted == there => format!(" ({name} is this machine, so it cannot be port {there} here as well)"),
            false => format!(" (port {wanted} is in use here)"),
        };
        self.port_said(idx, format!("Port {there} on {name} is now {} on this machine{moved}.", port.address()));
        self.ports.push(port);
        self.dirty = true;
        // Is anything listening there? Said only if nothing is: the port
        // here is open either way, for when something does.
        if let (Some(way), Some(tab)) = (self.far(host).and_then(|f| f.way.clone()), idx.and_then(|i| self.tabs.get(i)).map(|t| t.id)) {
            let tx = self.tx.clone();
            tokio::spawn(async move {
                if let Ok(Answer::Text { failed: true, .. }) = way.ask(Ask::Listening { port: there }).await {
                    let _ = tx.send(Ev::PortSilent { tab, port: id }).await;
                }
            });
        }
    }

    /// Nothing answered on a port there when it was forwarded.
    pub(super) fn on_port_silent(&mut self, tab: u64, port: u64) {
        let Some(port) = self.ports.iter().find(|p| p.id == port) else { return };
        let text = format!("Nothing is listening on port {} on {} yet. {} will work as soon as something is.", port.there, self.far_name(&port.host), port.address());
        if let Some(idx) = self.tab_index(tab) {
            self.notice(idx, text);
        }
    }

    pub(super) fn stop_port(&mut self, idx: Option<usize>, id: u64) {
        let Some(at) = self.ports.iter().position(|p| p.id == id) else { return };
        let port = self.ports.remove(at);
        let name = self.far_name(&port.host);
        self.port_said(idx, format!("Port {} on {name} is no longer forwarded.", port.there));
        self.dirty = true;
    }

    /// A connection has been made to a port here: it is carried to the
    /// machine it stands for, or closed if that is out of reach just now.
    pub(super) fn on_accepted(&mut self, port: u64, socket: tokio::net::TcpStream) {
        let Some(port) = self.ports.iter().find(|p| p.id == port) else { return };
        if let Some(way) = self.far(&port.host).filter(|f| f.carries_ports).and_then(|f| f.way.as_ref()) {
            way.carry(port.there, socket);
        }
    }

    /// Ports of machines nothing is shown from any more are closed.
    pub(super) fn drop_unused_ports(&mut self) {
        let fars: Vec<&str> = self.fars.iter().map(|f| f.host.as_str()).collect();
        self.ports.retain(|p| fars.contains(&p.host.as_str()));
    }

    /// The menu of the status bar's ports: each one to open or to stop,
    /// and another to forward.
    pub(super) fn open_ports_menu(&mut self, col: u16) {
        let mut items = Vec::new();
        for port in self.ports_in_front() {
            items.push((format!("Open {}", port.address()), MenuAction::PortOpen(port.id)));
            items.push((format!("Copy {}", port.address()), MenuAction::PortCopy(port.id)));
            items.push((format!("Stop forwarding port {}", port.there), MenuAction::PortStop(port.id)));
        }
        items.push(("Forward another port…".to_string(), MenuAction::PortAsk));
        let height = items.len() as u16 + 2;
        let at = (col, self.rows.saturating_sub(height + 2));
        self.ui.overlay = Some(Overlay::Menu(Menu { at, items, selected: 0 }));
        self.dirty = true;
    }

    /// Ask which port to forward from the machine the keyboard's pane is on.
    pub(super) fn ask_port(&mut self) {
        match self.far_of_tab(self.active) {
            Some(host) => self.ui.overlay = Some(Overlay::Port { text: String::new(), host }),
            None => self.toast("Ports are forwarded from a tab on another machine"),
        }
        self.dirty = true;
    }

    pub(super) fn port_menu_action(&mut self, action: &MenuAction) {
        let address = |s: &Server, id: u64| s.ports.iter().find(|p| p.id == id).map(|p| p.address());
        match action {
            MenuAction::PortOpen(id) => {
                if let Some(address) = address(self, *id) {
                    self.open_link(&address);
                }
            }
            MenuAction::PortCopy(id) => {
                if let Some(address) = address(self, *id) {
                    self.copy_to_clipboard(address);
                    self.toast("Copied the address");
                }
            }
            MenuAction::PortStop(id) => self.stop_port(None, *id),
            MenuAction::PortAsk => self.ask_port(),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_port_a_word_names() {
        assert_eq!(port_of("3000"), Some(3000));
        assert_eq!(port_of("localhost:5173"), Some(5173));
        assert_eq!(port_of("http://127.0.0.1:8000/docs"), Some(8000));
        assert_eq!(port_of("http://[::1]:8080/"), Some(8080));
        assert_eq!(port_of(":9000"), Some(9000));
        for none in ["", "0", "65536", "web", "http://localhost/", "-3"] {
            assert_eq!(port_of(none), None, "{none}");
        }
    }

    #[test]
    fn the_ports_tried_here() {
        // Itself first, then the ones after it, then any.
        let tried = candidates(3000);
        assert_eq!((&tried[..3], tried.last()), (&[3000, 3001, 3002][..], Some(&0)));
        // A port only root may open: its usual stand-in.
        assert_eq!(&candidates(80)[..3], &[80, 8080, 8081]);
        // And nothing past the last port there is.
        assert_eq!(candidates(65535), [65535, 0]);
    }

    #[test]
    fn a_port_in_use_here_is_not_taken_twice() {
        let held = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        assert!(taken(port));
        assert!(listen(port).is_none());
        // What is free is had, and for this machine only.
        let (got, listeners) = listen(0).unwrap();
        assert!(got != 0 && got != port);
        assert!(listeners.iter().all(|l| l.local_addr().unwrap().ip().is_loopback()));
    }
}
