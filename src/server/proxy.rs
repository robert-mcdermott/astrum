//! The network another machine is on, used from this one: a proxy.
//!
//! `/proxy` in a tab on another machine opens a port on this one that
//! speaks SOCKS5. A program told to use it (a browser, `curl`) asks it
//! for a machine by name, and the connection is made from the other
//! machine, where the name is looked up: what only that network knows,
//! and only it can reach, is then reachable from here. It is `ssh -D`'s
//! work, over the connection the tab already has.
//!
//! The port speaks two things and nothing else:
//!
//! - SOCKS5, with no sign-in, to connect (not to listen, nor UDP);
//! - `CONNECT`, as web proxies take it, for programs that know only
//!   `https_proxy`.
//!
//! It does not pass on an ordinary web request. That is what a page in a
//! browser could send it, and a page must not be able to reach into
//! another network through a port on this machine. A page cannot send
//! `CONNECT`, and cannot begin a request with SOCKS5's first byte.
//!
//! The port is open to this machine only. The agent cannot open one.

use super::{Ev, Menu, MenuAction, Overlay, Server};
use crate::remote::policy;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::task::AbortHandle;

/// The port a SOCKS proxy is looked for at.
const USUAL: u16 = 1080;
/// How long a program has to say what it wants.
const SAY_BY: Duration = Duration::from_secs(15);

/// A proxy through another machine.
pub struct Proxy {
    pub id: u64,
    /// The machine, as the user named it to `ssh`.
    pub host: String,
    pub here: u16,
    tasks: Vec<AbortHandle>,
}

impl Proxy {
    pub fn address(&self) -> String {
        format!("socks5h://127.0.0.1:{}", self.here)
    }

    /// What a shell is told, for the programs started from it.
    fn for_a_shell(&self) -> String {
        format!("export ALL_PROXY=socks5h://127.0.0.1:{0} HTTPS_PROXY=http://127.0.0.1:{0}", self.here)
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// The way a program asked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Speaks {
    Socks5,
    Connect,
}

/// What SOCKS5 answers a request with: `code` 0 is "made".
fn socks5_reply(code: u8) -> Vec<u8> {
    vec![5, code, 0, 1, 0, 0, 0, 0, 0, 0]
}

/// The code SOCKS5 has for why a connection was not made.
fn socks5_code(why: &str) -> u8 {
    let why = why.to_ascii_lowercase();
    match () {
        // Not allowed by the rules.
        () if why.contains(policy::FILE) => 2,
        // Refused by the machine asked for.
        () if why.contains("refused") => 5,
        // The machine could not be found, or did not answer.
        () if why.contains("not known") || why.contains("no address") || why.contains("looking up") || why.contains("did not answer") || why.contains("timed out") => 4,
        () => 1,
    }
}

fn http_reply(status: &str, more: &str, body: &str) -> Vec<u8> {
    format!("HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n{more}\r\n{body}", body.len()).into_bytes()
}

/// What a program is told of the connection it asked for.
pub(super) fn answer(speaks: Speaks, through: String) -> impl Fn(Result<(), &str>) -> Vec<u8> + Send + 'static {
    move |made| match (speaks, made) {
        (Speaks::Socks5, Ok(())) => socks5_reply(0),
        (Speaks::Socks5, Err(why)) => socks5_reply(socks5_code(why)),
        (Speaks::Connect, Ok(())) => b"HTTP/1.1 200 Connection established\r\n\r\n".to_vec(),
        (Speaks::Connect, Err(why)) => {
            let status = if why.contains(policy::FILE) { "403 Forbidden" } else { "502 Bad Gateway" };
            http_reply(status, "", &format!("Astrum, through {through}: {why}\n"))
        }
    }
}

/// `host:port`, an address with colons of its own in brackets.
fn host_and_port(text: &str) -> Option<(String, u16)> {
    let (host, port) = text.rsplit_once(':')?;
    let port = port.parse::<u16>().ok().filter(|p| *p != 0)?;
    let named = !host.is_empty() && host.len() <= 255 && !host.chars().any(|c| c.is_whitespace() || c.is_control() || c == '/' || c == '@');
    named.then(|| (host.to_string(), port))
}

/// What a program that has connected to the proxy asks for: the machine
/// and the port, and the way it asked. What cannot be passed on is
/// answered here, in the way it was asked, and `None` is what is left.
async fn asked(socket: &mut TcpStream) -> Option<(String, u16, Speaks)> {
    match socket.read_u8().await.ok()? {
        5 => {
            // The ways it can sign in: with none is the one there is.
            let ways = socket.read_u8().await.ok()? as usize;
            let mut offered = vec![0u8; ways];
            socket.read_exact(&mut offered).await.ok()?;
            if !offered.contains(&0) {
                let _ = socket.write_all(&[5, 0xff]).await;
                return None;
            }
            socket.write_all(&[5, 0]).await.ok()?;
            let mut head = [0u8; 4];
            socket.read_exact(&mut head).await.ok()?;
            let (version, command, kind) = (head[0], head[1], head[3]);
            if version != 5 {
                return None;
            }
            let host = match kind {
                1 => {
                    let mut ip = [0u8; 4];
                    socket.read_exact(&mut ip).await.ok()?;
                    Ipv4Addr::from(ip).to_string()
                }
                3 => {
                    let len = socket.read_u8().await.ok()? as usize;
                    let mut name = vec![0u8; len];
                    socket.read_exact(&mut name).await.ok()?;
                    String::from_utf8(name).ok().filter(|n| !n.is_empty() && !n.chars().any(|c| c.is_whitespace() || c.is_control()))?
                }
                4 => {
                    let mut ip = [0u8; 16];
                    socket.read_exact(&mut ip).await.ok()?;
                    format!("[{}]", Ipv6Addr::from(ip))
                }
                _ => {
                    // (That kind of address is not known.)
                    let _ = socket.write_all(&socks5_reply(8)).await;
                    return None;
                }
            };
            let port = socket.read_u16().await.ok()?;
            // To connect: not to listen for a connection, nor for UDP.
            if command != 1 || port == 0 {
                let _ = socket.write_all(&socks5_reply(7)).await;
                return None;
            }
            Some((host, port, Speaks::Socks5))
        }
        first if first.is_ascii_uppercase() => {
            // A web request: its head, up to the empty line, and no more
            // of what follows than that (it is for the machine asked for).
            let mut head = vec![first];
            while !head.ends_with(b"\r\n\r\n") {
                if head.len() >= 8192 {
                    let _ = socket.write_all(&http_reply("431 Request Header Fields Too Large", "", "")).await;
                    return None;
                }
                head.push(socket.read_u8().await.ok()?);
            }
            let head = String::from_utf8_lossy(&head);
            let mut words = head.lines().next().unwrap_or("").split(' ');
            let (method, target) = (words.next().unwrap_or(""), words.next().unwrap_or(""));
            match (method, host_and_port(target)) {
                ("CONNECT", Some((host, port))) => Some((host, port, Speaks::Connect)),
                ("CONNECT", None) => {
                    let _ = socket.write_all(&http_reply("400 Bad Request", "", "CONNECT takes a machine and a port: CONNECT wiki.internal:443\n")).await;
                    None
                }
                // (Anything a page in a browser could have sent.)
                _ => {
                    let said = "This is Astrum's proxy through another machine. It takes SOCKS5 and CONNECT, and passes on no other request.\n";
                    let _ = socket.write_all(&http_reply("405 Method Not Allowed", "Allow: CONNECT\r\n", said)).await;
                    None
                }
            }
        }
        // (SOCKS4, or something else altogether.)
        _ => None,
    }
}

const USAGE: &str = "`/proxy` in a tab on another machine opens a SOCKS5 proxy on this machine through which that machine's network is reached, with names looked up there. `/proxy 1081` chooses its port, `/proxy stop` ends it.";

impl Server {
    /// The proxy through the machine the keyboard's pane is on.
    pub fn proxy_in_front(&self) -> Option<&Proxy> {
        let host = self.far_of_tab(self.active)?;
        self.proxies.iter().find(|p| p.host == host)
    }

    /// `/proxy`, with whatever followed it.
    pub(super) fn proxy_command(&mut self, idx: usize, arg: &str) {
        let word = arg.trim();
        let Some(host) = self.far_of_tab(idx) else {
            return self.notice(idx, format!("This tab's shell is on this machine, so there is no other network to reach through it. {USAGE}\n\nA tab on another machine: `≡ menu` → New tab on another machine…, or `/ssh host`."));
        };
        match word.to_ascii_lowercase().as_str() {
            "" => {
                self.start_proxy(Some(idx), &host, None);
            }
            "stop" | "off" | "close" => match self.proxies.iter().find(|p| p.host == host).map(|p| p.id) {
                Some(id) => self.stop_proxy(Some(idx), id),
                None => self.notice(idx, format!("There is no proxy through {}.", self.far_name(&host))),
            },
            port => match port.parse::<u16>().ok().filter(|p| *p != 0) {
                Some(here) => {
                    self.start_proxy(Some(idx), &host, Some(here));
                }
                None => self.notice(idx, USAGE),
            },
        }
    }

    /// Open a proxy through `host`, if there is none, and say where it
    /// is (in the tab's panel, or at the top of the window). The port
    /// here, or nothing if there cannot be one, which is said too.
    pub(super) fn start_proxy(&mut self, idx: Option<usize>, host: &str, here: Option<u16>) -> Option<u16> {
        let name = self.far_name(host);
        let refused = match self.far(host) {
            None => Some(format!("{name} is not a machine this session has a tab on.")),
            Some(far) if far.there.is_none() => Some(format!("{name} has not been reached yet. Open the proxy once its shell is there.")),
            Some(far) if !far.reaches => {
                let version = far.version.clone().unwrap_or_else(|| "an earlier version".into());
                Some(format!("The Astrum on {name} ({version}) is from before other machines could be reached through it. Update it there (`astrum upgrade` in a shell on it), close the tabs on {name}, wait half a minute for the older one to stop, and open the tab again."))
            }
            Some(far) => policy::refusal(far.forward, true).map(|why| format!("{name} says no: {why}.")),
        };
        if let Some(why) = refused {
            self.proxy_said(idx, why);
            return None;
        }
        if let Some(proxy) = self.proxies.iter().find(|p| p.host == host) {
            let (at, address) = (proxy.here, proxy.address());
            let moved = here.filter(|h| *h != at).map(|h| format!(" To have it at port {h}, stop it first: `/proxy stop`.")).unwrap_or_default();
            if idx.is_some() {
                self.proxy_said(idx, format!("There is a proxy through {name} already, at {address}.{moved}"));
            }
            return Some(at);
        }
        let wanted = here.unwrap_or(USUAL);
        let Some((here, listeners)) = super::ports::candidates(wanted).into_iter().find_map(super::ports::listen) else {
            self.proxy_said(idx, format!("No port could be opened on this machine for a proxy through {name}."));
            return None;
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
                            Ok((mut socket, _)) => {
                                let tx = tx.clone();
                                // (Each says what it wants in its own time, and not for ever.)
                                tokio::spawn(async move {
                                    let _ = socket.set_nodelay(true);
                                    if let Ok(Some((host, port, speaks))) = tokio::time::timeout(SAY_BY, asked(&mut socket)).await {
                                        let _ = tx.send(Ev::Proxied { proxy: id, socket, host, port, speaks }).await;
                                    }
                                });
                            }
                            Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
                        }
                    }
                })
                .abort_handle(),
            );
        }
        let proxy = Proxy { id, host: host.to_string(), here, tasks };
        let moved = if here == wanted { String::new() } else { format!(" (port {wanted} is in use here)") };
        let said = format!(
            "The network {name} is on can now be used from this machine, through a proxy at `127.0.0.1:{here}`{moved}. It speaks SOCKS5, and names are looked up on {name}.\n\n- **A browser:** click `⇄ proxy` on the status bar → Open a browser through {name}.\n- **A shell here:** `{}`, or for one command `curl -x {} …`.\n\n`/proxy stop` ends it, and so does closing the last tab on {name}.",
            proxy.for_a_shell(),
            proxy.address()
        );
        self.proxies.push(proxy);
        self.dirty = true;
        self.proxy_said(idx, said);
        Some(here)
    }

    /// In the tab's panel where one asked, and where the mouse did, at
    /// the top of the window (and briefly: its menu says the rest).
    fn proxy_said(&mut self, idx: Option<usize>, text: String) {
        match idx.filter(|i| *i < self.tabs.len()) {
            Some(idx) => self.notice(idx, text),
            None => self.toast(text.split("\n\n").next().unwrap_or(&text).replace('`', "")),
        }
    }

    pub(super) fn stop_proxy(&mut self, idx: Option<usize>, id: u64) {
        let Some(at) = self.proxies.iter().position(|p| p.id == id) else { return };
        let proxy = self.proxies.remove(at);
        // (What was going through it is over too, at both ends.)
        if let Some(way) = self.far(&proxy.host).and_then(|f| f.way.as_ref()) {
            way.drop_of(proxy.id);
        }
        let name = self.far_name(&proxy.host);
        self.dirty = true;
        self.proxy_said(idx, format!("The proxy through {name} is closed."));
    }

    /// A program has asked the proxy for a machine: the connection is
    /// made from the other machine, or the program is told why not.
    pub(super) fn on_proxied(&mut self, proxy: u64, mut socket: TcpStream, host: String, port: u16, speaks: Speaks) {
        let Some(proxy) = self.proxies.iter().find(|p| p.id == proxy) else { return };
        let name = self.far_name(&proxy.host);
        let answer = answer(speaks, name.clone());
        match self.far(&proxy.host).filter(|f| f.reaches).and_then(|f| f.way.as_ref()) {
            Some(way) => way.reach(proxy.id, &host, port, socket, answer),
            None => {
                tokio::spawn(async move {
                    let _ = socket.write_all(&answer(Err("the other machine is out of reach just now"))).await;
                });
            }
        }
    }

    /// Proxies through machines nothing is shown from any more are closed.
    pub(super) fn drop_unused_proxies(&mut self) {
        let fars: Vec<&str> = self.fars.iter().map(|f| f.host.as_str()).collect();
        self.proxies.retain(|p| fars.contains(&p.host.as_str()));
    }

    /// The proxy's menu: opened from the status bar, or from `≡ menu`
    /// (where it opens the proxy first if there is none).
    pub(super) fn open_proxy_menu(&mut self, at: Option<(u16, u16)>) {
        let Some(host) = self.far_of_tab(self.active) else { return self.toast("A proxy goes through a tab on another machine") };
        if self.proxies.iter().all(|p| p.host != host) && self.start_proxy(None, &host, None).is_none() {
            return;
        }
        let Some(proxy) = self.proxies.iter().find(|p| p.host == host) else { return };
        let name = self.far_name(&host);
        let items = vec![
            (format!("Open a browser through {name}"), MenuAction::ProxyBrowse(proxy.id)),
            ("Copy the setting for a shell on this machine".to_string(), MenuAction::ProxyCopyShell(proxy.id)),
            (format!("Copy {}", proxy.address()), MenuAction::ProxyCopy(proxy.id)),
            (format!("Close the proxy through {name}"), MenuAction::ProxyStop(proxy.id)),
        ];
        let height = items.len() as u16 + 2;
        let at = at.unwrap_or((self.cols.saturating_sub(60) / 2, self.rows.saturating_sub(height) / 2));
        self.ui.overlay = Some(Overlay::Menu(Menu { at: (at.0, at.1.min(self.rows.saturating_sub(height + 2))), items, selected: 0 }));
        self.dirty = true;
    }

    /// Start a browser that goes through the proxy to `host`'s network
    /// (opening the proxy if there is none), at `url` if one is given.
    pub(super) fn browse_through(&mut self, host: &str, url: Option<String>) {
        let name = self.far_name(host);
        let Some(here) = self.proxies.iter().find(|p| p.host == host).map(|p| p.here).or_else(|| self.start_proxy(None, host, None)) else { return };
        if crate::browser::find().is_none() {
            return self.toast(format!("No browser was found that can be given a proxy when it is started (Chrome, Chromium, Edge or Brave). Set a browser's SOCKS proxy to 127.0.0.1:{here} yourself, or name one in ASTRUM_BROWSER_TOOL."));
        }
        let client = self.clients.iter().find(|c| Some(c.id) == self.lead && c.attached);
        match client {
            Some(c) if c.browses => {
                let _ = c.tx.send(crate::proto::ServerFrame::Msg(crate::proto::ServerMsg::Browse { url, proxy: here, profile: host.to_string() }));
                self.toast(format!("Opening a browser that goes through {name}"));
            }
            _ => self.toast("This terminal's astrum is from before it could start a browser: restart it (detach and run astrum again)"),
        }
    }

    pub(super) fn proxy_menu_action(&mut self, action: &MenuAction) {
        let found = |s: &Server, id: u64| s.proxies.iter().find(|p| p.id == id).map(|p| (p.host.clone(), p.address(), p.for_a_shell()));
        match action {
            MenuAction::ProxyMenu => self.open_proxy_menu(None),
            MenuAction::ProxyBrowse(id) => {
                if let Some((host, ..)) = found(self, *id) {
                    self.browse_through(&host, None);
                }
            }
            MenuAction::ProxyCopyShell(id) => {
                if let Some((.., setting)) = found(self, *id) {
                    self.copy_to_clipboard(setting);
                    self.toast("Copied. Paste it into a shell on this machine: what is started from that shell goes through the proxy");
                }
            }
            MenuAction::ProxyCopy(id) => {
                if let Some((_, address, _)) = found(self, *id) {
                    self.copy_to_clipboard(address);
                    self.toast("Copied the proxy's address");
                }
            }
            MenuAction::ProxyStop(id) => self.stop_proxy(None, *id),
            // A link in a pane on another machine, to a machine that one can reach.
            MenuAction::OpenLinkThrough => {
                let host = self.tab().and_then(|t| t.panes.focused().there()).map(|t| t.host.clone());
                if let (Some(host), Some(link)) = (host, self.ui.menu_link.take()) {
                    self.browse_through(&host, Some(link.uri));
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    /// A program that has connected to the proxy, and the proxy's end of it.
    async fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let near = TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        (near, listener.accept().await.unwrap().0)
    }

    /// What the proxy makes of what a program sends it, and what the
    /// program is sent back before anything is connected.
    async fn said(sent: &[u8]) -> (Option<(String, u16, Speaks)>, Vec<u8>) {
        let (mut program, mut end) = pair().await;
        program.write_all(sent).await.unwrap();
        let asked = tokio::time::timeout(Duration::from_secs(5), asked(&mut end)).await.unwrap();
        drop(end);
        let mut back = Vec::new();
        let _ = program.read_to_end(&mut back).await;
        (asked, back)
    }

    #[tokio::test]
    async fn socks5_is_understood_as_far_as_connecting() {
        // A name, an address, and an address with colons.
        let (asked, back) = said(b"\x05\x01\x00\x05\x01\x00\x03\x0dwiki.internal\x01\xbb").await;
        assert_eq!(asked, Some(("wiki.internal".to_string(), 443, Speaks::Socks5)));
        assert_eq!(back, [5, 0]);
        let (asked, _) = said(b"\x05\x02\x00\x01\x05\x01\x00\x01\x0a\x00\x00\x24\x24\xb1").await;
        assert_eq!(asked, Some(("10.0.0.36".to_string(), 9393, Speaks::Socks5)));
        let mut v6 = b"\x05\x01\x00\x05\x01\x00\x04".to_vec();
        v6.extend_from_slice(&"fd00::5".parse::<Ipv6Addr>().unwrap().octets());
        v6.extend_from_slice(&[0x1f, 0x90]);
        assert_eq!(said(&v6).await.0, Some(("[fd00::5]".to_string(), 8080, Speaks::Socks5)));

        // Only with no sign-in; only to connect; only kinds of address there are.
        let (asked, back) = said(b"\x05\x01\x02").await;
        assert_eq!((asked, back), (None, vec![5, 0xff]));
        let (asked, back) = said(b"\x05\x01\x00\x05\x02\x00\x01\x7f\x00\x00\x01\x00\x50").await;
        assert_eq!((asked, &back[..4]), (None, &[5, 0, 5, 7][..]));
        let (asked, back) = said(b"\x05\x01\x00\x05\x03\x00\x01\x7f\x00\x00\x01\x00\x50").await;
        assert_eq!((asked, back[3]), (None, 7));
        let (asked, back) = said(b"\x05\x01\x00\x05\x01\x00\x09").await;
        assert_eq!((asked, back[3]), (None, 8));
        // A name with something in it that is no part of a name; port 0.
        assert_eq!(said(b"\x05\x01\x00\x05\x01\x00\x03\x03a b\x00\x50").await.0, None);
        assert_eq!(said(b"\x05\x01\x00\x05\x01\x00\x03\x00\x00\x50").await.0, None);
        assert_eq!(said(b"\x05\x01\x00\x05\x01\x00\x03\x01a\x00\x00").await.0, None);
        // SOCKS4, and what is nothing at all.
        assert_eq!(said(b"\x04\x01\x00\x50\x7f\x00\x00\x01\x00").await, (None, vec![]));
        assert_eq!(said(b"\x16\x03\x01").await, (None, vec![]));
    }

    #[tokio::test]
    async fn connect_is_passed_on_and_no_other_request() {
        let (asked, back) = said(b"CONNECT wiki.internal:443 HTTP/1.1\r\nHost: wiki.internal:443\r\nUser-Agent: curl\r\n\r\n").await;
        assert_eq!(asked, Some(("wiki.internal".to_string(), 443, Speaks::Connect)));
        assert!(back.is_empty());
        assert_eq!(said(b"CONNECT [fd00::5]:8443 HTTP/1.1\r\n\r\n").await.0, Some(("[fd00::5]".to_string(), 8443, Speaks::Connect)));
        // Not one byte past the head is taken: it belongs to the machine asked for.
        let (mut program, mut end) = pair().await;
        program.write_all(b"CONNECT a.internal:22 HTTP/1.1\r\n\r\nSSH-2.0-x").await.unwrap();
        assert!(asked_of(&mut end).await.is_some());
        let mut rest = [0u8; 9];
        end.read_exact(&mut rest).await.unwrap();
        assert_eq!(&rest, b"SSH-2.0-x");

        // What a page in a browser can send is refused, whatever it asks for.
        for request in [
            "GET / HTTP/1.1\r\nHost: 127.0.0.1:1080\r\n\r\n",
            "GET http://wiki.internal/ HTTP/1.1\r\nHost: wiki.internal\r\n\r\n",
            "POST http://wiki.internal/admin HTTP/1.1\r\nHost: wiki.internal\r\nContent-Length: 0\r\n\r\n",
            "OPTIONS * HTTP/1.1\r\n\r\n",
        ] {
            let (asked, back) = said(request.as_bytes()).await;
            let back = String::from_utf8_lossy(&back).into_owned();
            assert!(asked.is_none() && back.starts_with("HTTP/1.1 405 ") && back.contains("Allow: CONNECT"), "{request}: {back}");
            assert!(!back.to_ascii_lowercase().contains("access-control"), "{back}");
        }
        for request in ["CONNECT wiki.internal HTTP/1.1\r\n\r\n", "CONNECT :443 HTTP/1.1\r\n\r\n", "CONNECT a/b:443 HTTP/1.1\r\n\r\n", "CONNECT wiki.internal:0 HTTP/1.1\r\n\r\n"] {
            let (asked, back) = said(request.as_bytes()).await;
            assert!(asked.is_none() && back.starts_with(b"HTTP/1.1 400 "), "{request}");
        }
        // A head that never ends is given up on.
        let (asked, back) = said(format!("CONNECT a:1 HTTP/1.1\r\nX: {}", "y".repeat(9000)).as_bytes()).await;
        assert!(asked.is_none() && back.starts_with(b"HTTP/1.1 431 "));
    }

    async fn asked_of(end: &mut TcpStream) -> Option<(String, u16, Speaks)> {
        tokio::time::timeout(Duration::from_secs(5), asked(end)).await.unwrap()
    }

    #[test]
    fn a_program_is_told_what_came_of_it_in_the_way_it_asked() {
        let socks = answer(Speaks::Socks5, "web-3".into());
        assert_eq!(socks(Ok(())), [5, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(socks(Err("Connection refused (os error 61)"))[1], 5);
        assert_eq!(socks(Err("the name wiki.internal is not known there"))[1], 4);
        assert_eq!(socks(Err("it did not answer"))[1], 4);
        assert_eq!(socks(Err(&policy::refusal(policy::Forward::Local, true).unwrap()))[1], 2);
        assert_eq!(socks(Err("something else"))[1], 1);
        let web = answer(Speaks::Connect, "web-3".into());
        assert_eq!(web(Ok(())), b"HTTP/1.1 200 Connection established\r\n\r\n");
        let refused = String::from_utf8(web(Err("Connection refused"))).unwrap();
        assert!(refused.starts_with("HTTP/1.1 502 ") && refused.ends_with("Astrum, through web-3: Connection refused\n"), "{refused}");
        assert!(web(Err(&policy::refusal(policy::Forward::Nothing, true).unwrap())).starts_with(b"HTTP/1.1 403 "));
    }
}
