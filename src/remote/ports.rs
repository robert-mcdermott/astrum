//! Connections carried over the link: a port on the other machine, used
//! from this one.
//!
//! This machine listens on a port of its own. Each connection made to it
//! is given a number and told to the program keeping the shells, which
//! connects to the port there; what either end then reads from its socket
//! goes to the other as frames, beside the shells' own. Both ends do the
//! same work, so it is here once (`Streams`), and each says what it has
//! to say (`Said`) in its own messages.
//!
//! Neither end sends more of a connection than the other has room for:
//! each may be `WINDOW` bytes ahead of what the other has passed on to
//! its socket, and is told (`Took`) as that is done. So a download nobody
//! is reading, or a slow link, holds up that connection and not the
//! memory of either machine, and the shells' bytes are never far behind
//! a carried connection's.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{Semaphore, mpsc};
use tokio::task::AbortHandle;

/// How far ahead of the other end's socket one end may be, per connection.
pub const WINDOW: usize = 256 * 1024;
/// The most connections one machine's are carried at a time.
pub const MOST: usize = 256;
/// The most read from a socket, and sent, at once.
const PIECE: usize = 32 * 1024;

/// What one end has to tell the other about a carried connection.
#[derive(Debug)]
pub enum Said {
    /// What was read from the socket here.
    Bytes(u64, Vec<u8>),
    /// So much of what was sent has been written to the socket here.
    Took(u64, u64),
    /// The socket here has no more to give.
    End(u64),
    /// The connection is over, and why, if something went wrong.
    Drop(u64, Option<String>),
}

enum In {
    Bytes(Vec<u8>),
    End,
}

struct Stream {
    to: mpsc::UnboundedSender<In>,
    /// How much more may be sent before the other end says it took some.
    room: Arc<Semaphore>,
    task: Option<AbortHandle>,
}

/// One end's carried connections.
#[derive(Clone, Default)]
pub struct Streams {
    all: Arc<Mutex<HashMap<u64, Stream>>>,
}

/// How one end says something to the other.
pub type Say = Arc<dyn Fn(Said) + Send + Sync>;

/// Bytes waiting to go over the link, and how many may wait before the
/// carried connections stop adding to them (the shells' go first).
pub type Gate = Option<(Arc<AtomicUsize>, usize)>;

impl Streams {
    /// Carry a connection as `id`: `socket` is this end of it, once it is
    /// there (a socket already had, or one still being connected). What
    /// the other end sends meanwhile waits for it.
    pub fn carry(&self, id: u64, socket: impl Future<Output = std::io::Result<TcpStream>> + Send + 'static, say: Say, gate: Gate) {
        let (to, from) = mpsc::unbounded_channel();
        let room = Arc::new(Semaphore::new(WINDOW));
        let Ok(mut all) = self.all.lock() else { return };
        if all.contains_key(&id) {
            return;
        }
        all.insert(id, Stream { to, room: room.clone(), task: None });
        let streams = self.clone();
        let task = tokio::spawn(async move {
            match socket.await {
                Ok(socket) => {
                    if !pass(id, socket, from, room, &say, gate).await {
                        say(Said::Drop(id, None));
                    }
                }
                Err(e) => say(Said::Drop(id, Some(e.to_string()))),
            }
            streams.forget(id);
        });
        // (It may be over already, and have taken itself out.)
        if let Some(stream) = all.get_mut(&id) {
            stream.task = Some(task.abort_handle());
        }
    }

    fn forget(&self, id: u64) -> Option<Stream> {
        self.all.lock().ok().and_then(|mut all| all.remove(&id))
    }

    /// The other end read this from its socket.
    pub fn bytes(&self, id: u64, bytes: Vec<u8>) {
        if let Ok(all) = self.all.lock() {
            if let Some(stream) = all.get(&id) {
                let _ = stream.to.send(In::Bytes(bytes));
            }
        }
    }

    /// The other end has written so much to its socket.
    pub fn took(&self, id: u64, n: u64) {
        if let Ok(all) = self.all.lock() {
            if let Some(stream) = all.get(&id) {
                // (Never more room than there is at the start, whatever is said.)
                let n = (n.min(WINDOW as u64) as usize).min(WINDOW.saturating_sub(stream.room.available_permits()));
                stream.room.add_permits(n);
            }
        }
    }

    /// The other end's socket has no more to give.
    pub fn end(&self, id: u64) {
        if let Ok(all) = self.all.lock() {
            if let Some(stream) = all.get(&id) {
                let _ = stream.to.send(In::End);
            }
        }
    }

    /// The other end has let the connection go: so does this one.
    pub fn dropped(&self, id: u64) {
        if let Some(task) = self.forget(id).and_then(|s| s.task) {
            task.abort();
        }
    }

    /// The link is gone, and every connection with it.
    pub fn clear(&self) {
        let all: Vec<Stream> = self.all.lock().map(|mut all| all.drain().map(|(_, s)| s).collect()).unwrap_or_default();
        for task in all.into_iter().filter_map(|s| s.task) {
            task.abort();
        }
    }

    pub fn len(&self) -> usize {
        self.all.lock().map(|all| all.len()).unwrap_or(0)
    }
}

/// Pass bytes between a socket and the other end until both have said all
/// they have. `false`: it broke off, and the other end is to be told.
async fn pass(id: u64, socket: TcpStream, mut from: mpsc::UnboundedReceiver<In>, room: Arc<Semaphore>, say: &Say, gate: Gate) -> bool {
    let _ = socket.set_nodelay(true);
    let (mut read, mut write) = socket.into_split();
    let reading = async {
        let mut buf = vec![0u8; PIECE];
        loop {
            // Room for one byte at least, then as much as there is.
            match room.acquire().await {
                Ok(first) => first.forget(),
                Err(_) => return false,
            }
            if let Some((waiting, most)) = &gate {
                while waiting.load(Ordering::Relaxed) > *most {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }
            let more = room.available_permits().min(PIECE - 1);
            match read.read(&mut buf[..1 + more]).await {
                Ok(0) => {
                    say(Said::End(id));
                    return true;
                }
                Ok(n) => {
                    room.forget_permits(n - 1);
                    say(Said::Bytes(id, buf[..n].to_vec()));
                }
                Err(_) => return false,
            }
        }
    };
    let writing = async {
        loop {
            match from.recv().await {
                Some(In::Bytes(bytes)) => {
                    if write.write_all(&bytes).await.is_err() {
                        return false;
                    }
                    say(Said::Took(id, bytes.len() as u64));
                }
                Some(In::End) => {
                    let _ = write.shutdown().await;
                    return true;
                }
                None => return false,
            }
        }
    };
    tokio::pin!(reading, writing);
    let (mut read_all, mut written_all) = (false, false);
    while !(read_all && written_all) {
        let whole = tokio::select! {
            whole = &mut reading, if !read_all => {
                read_all = true;
                whole
            }
            whole = &mut writing, if !written_all => {
                written_all = true;
                whole
            }
        };
        if !whole {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    /// Two ends joined as the link joins them: what one says, the other hears.
    fn joined() -> (Streams, Say, Streams, Say) {
        let (a, b) = (Streams::default(), Streams::default());
        let hear = |ear: Streams| -> Say {
            Arc::new(move |said| match said {
                Said::Bytes(id, bytes) => ear.bytes(id, bytes),
                Said::Took(id, n) => ear.took(id, n),
                Said::End(id) => ear.end(id),
                Said::Drop(id, _) => ear.dropped(id),
            })
        };
        (a.clone(), hear(b.clone()), b, hear(a))
    }

    async fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let near = TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        (near, listener.accept().await.unwrap().0)
    }

    async fn until(what: &str, mut done: impl FnMut() -> bool) {
        for _ in 0..400 {
            if done() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("waited too long for {what}");
    }

    #[tokio::test]
    async fn a_connection_is_carried_both_ways_and_ends_when_both_have() {
        let (here, here_says, there, there_says) = joined();
        // A browser, as it were, and this end of its connection; a server
        // there, and that end's connection to it.
        let (mut browser, accepted) = pair().await;
        let (dialed, mut server) = pair().await;
        here.carry(7, async move { Ok(accepted) }, here_says, None);
        there.carry(7, async move { Ok(dialed) }, there_says, None);

        // Far more than a window, each way at once.
        let big: Vec<u8> = (0..3 * WINDOW + 1234).map(|i| (i % 251) as u8).collect();
        let (sent, len) = (big.clone(), big.len());
        let up = tokio::spawn(async move {
            browser.write_all(&sent).await.unwrap();
            browser.shutdown().await.unwrap();
            let mut back = Vec::new();
            browser.read_to_end(&mut back).await.unwrap();
            back
        });
        let mut got = vec![0u8; len];
        let answer: Vec<u8> = big.iter().rev().copied().collect();
        let (mut from_browser, mut to_browser) = server.split();
        let (read, wrote) = tokio::join!(from_browser.read_exact(&mut got), to_browser.write_all(&answer));
        (read.unwrap(), wrote.unwrap());
        assert!(got == big, "what the server read is not what the browser wrote");
        // The browser said it had finished: the server reads the end.
        assert_eq!(from_browser.read(&mut [0u8; 8]).await.unwrap(), 0);
        server.shutdown().await.unwrap();
        assert!(up.await.unwrap() == answer, "what the browser read is not what the server wrote");
        until("both ends to have let it go", || here.len() == 0 && there.len() == 0).await;
    }

    #[tokio::test]
    async fn nothing_is_sent_past_what_the_other_end_has_taken() {
        let streams = Streams::default();
        let said = Arc::new(Mutex::new(0usize));
        let count = said.clone();
        let say: Say = Arc::new(move |said| {
            if let Said::Bytes(_, bytes) = said {
                *count.lock().unwrap() += bytes.len();
            }
        });
        let (mut app, socket) = pair().await;
        streams.carry(1, async move { Ok(socket) }, say, None);
        let writer = tokio::spawn(async move {
            let _ = app.write_all(&vec![1u8; 4 * WINDOW]).await;
            app
        });
        until("a window's worth to be sent", || *said.lock().unwrap() == WINDOW).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(*said.lock().unwrap(), WINDOW);
        // The other end takes some: that much more follows, and no more.
        streams.took(1, 1000);
        until("what was taken to be made up", || *said.lock().unwrap() == WINDOW + 1000).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(*said.lock().unwrap(), WINDOW + 1000);
        // (And no word of the other end's makes more room than there is.)
        streams.took(1, u64::MAX);
        until("the rest", || *said.lock().unwrap() >= 2 * WINDOW).await;
        streams.dropped(1);
        assert_eq!(streams.len(), 0);
        drop(writer);
    }

    #[tokio::test]
    async fn a_port_nothing_listens_on_and_a_connection_let_go() {
        let (here, here_says, there, _) = joined();
        // Nothing is listening where that end connects: this end is told, and why.
        let why = Arc::new(Mutex::new(None));
        let (heard, ear) = (why.clone(), here.clone());
        let there_says: Say = Arc::new(move |said| {
            if let Said::Drop(id, why) = said {
                *heard.lock().unwrap() = why;
                ear.dropped(id);
            }
        });
        let (mut browser, accepted) = pair().await;
        here.carry(3, async move { Ok(accepted) }, here_says, None);
        let free = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();
        there.carry(3, TcpStream::connect(free), there_says, None);
        // The browser's connection is closed on it.
        let mut buf = [0u8; 8];
        assert!(matches!(browser.read(&mut buf).await, Ok(0) | Err(_)));
        until("both ends to have let it go", || here.len() == 0 && there.len() == 0).await;
        assert!(why.lock().unwrap().as_deref().is_some_and(|w| w.to_lowercase().contains("refused")), "{why:?}");

        // The link going takes every connection with it.
        let (mut browser, accepted) = pair().await;
        here.carry(4, async move { Ok(accepted) }, Arc::new(|_| {}), None);
        here.clear();
        assert!(matches!(browser.read(&mut buf).await, Ok(0) | Err(_)));
    }
}
