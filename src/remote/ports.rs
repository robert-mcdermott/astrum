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
    /// How much the other end has sent that is not yet written to the
    /// socket here: never more than `WINDOW`, if it keeps to the rule.
    owed: Arc<AtomicUsize>,
    /// The other end has said its socket has no more to give.
    ended: bool,
    /// What the connection belongs to, for letting go of all that does.
    of: u64,
    say: Say,
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
        self.carry_for(id, 0, socket, say, gate)
    }

    /// The same, for a connection that belongs to `of` (a port, say: see
    /// `drop_of`).
    pub fn carry_for(&self, id: u64, of: u64, socket: impl Future<Output = std::io::Result<TcpStream>> + Send + 'static, say: Say, gate: Gate) {
        let (to, from) = mpsc::unbounded_channel();
        let room = Arc::new(Semaphore::new(WINDOW));
        let owed = Arc::new(AtomicUsize::new(0));
        let Ok(mut all) = self.all.lock() else { return };
        if all.contains_key(&id) {
            return;
        }
        all.insert(id, Stream { to, room: room.clone(), owed: owed.clone(), ended: false, of, say: say.clone(), task: None });
        let streams = self.clone();
        let task = tokio::spawn(async move {
            match socket.await {
                Ok(socket) => {
                    if !pass(id, socket, from, room, owed, &say, gate).await {
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

    /// The other end read this from its socket. (One that sends more
    /// than this end has room for is not keeping to the rule that spares
    /// this machine's memory: its connection is let go. What comes after
    /// it said it had no more is not passed on.)
    pub fn bytes(&self, id: u64, bytes: Vec<u8>) {
        let Ok(mut all) = self.all.lock() else { return };
        let Some(stream) = all.get(&id) else { return };
        if stream.ended {
            return;
        }
        if stream.owed.fetch_add(bytes.len(), Ordering::Relaxed) + bytes.len() <= WINDOW {
            let _ = stream.to.send(In::Bytes(bytes));
            return;
        }
        let over = all.remove(&id);
        drop(all);
        if let Some(over) = over {
            if let Some(task) = over.task {
                task.abort();
            }
            (over.say)(Said::Drop(id, None));
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
        if let Ok(mut all) = self.all.lock() {
            if let Some(stream) = all.get_mut(&id) {
                stream.ended = true;
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

    /// Let go of every connection that belongs to `of`, and tell the
    /// other end of each.
    pub fn drop_of(&self, of: u64) {
        let gone: Vec<(u64, Stream)> = match self.all.lock() {
            Ok(mut all) => {
                let ids: Vec<u64> = all.iter().filter(|(_, s)| s.of == of).map(|(id, _)| *id).collect();
                ids.into_iter().filter_map(|id| all.remove(&id).map(|s| (id, s))).collect()
            }
            Err(_) => return,
        };
        for (id, stream) in gone {
            if let Some(task) = stream.task {
                task.abort();
            }
            (stream.say)(Said::Drop(id, None));
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
async fn pass(id: u64, socket: TcpStream, mut from: mpsc::UnboundedReceiver<In>, room: Arc<Semaphore>, owed: Arc<AtomicUsize>, say: &Say, gate: Gate) -> bool {
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
                    owed.fetch_sub(bytes.len(), Ordering::Relaxed);
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

    #[tokio::test]
    async fn an_end_that_sends_more_than_there_is_room_for_is_let_go() {
        let streams = Streams::default();
        let dropped = Arc::new(Mutex::new(Vec::new()));
        let heard = dropped.clone();
        let say: Say = Arc::new(move |said| {
            if let Said::Drop(id, _) = said {
                heard.lock().unwrap().push(id);
            }
        });
        let mut buf = [0u8; 8];

        // A window is taken, whatever the socket here is doing with it...
        let (mut app, socket) = pair().await;
        streams.carry(1, async move { Ok(socket) }, say.clone(), None);
        streams.bytes(1, vec![7u8; WINDOW]);
        let mut got = vec![0u8; WINDOW];
        app.read_exact(&mut got).await.unwrap();
        assert!(dropped.lock().unwrap().is_empty());
        // ...and once that is written there is room for as much again, but not for more.
        until("the window to be written", || streams.all.lock().unwrap()[&1].owed.load(Ordering::Relaxed) == 0).await;
        streams.bytes(1, vec![7u8; WINDOW + 1]);
        assert_eq!(*dropped.lock().unwrap(), vec![1]);
        assert_eq!(streams.len(), 0);
        assert!(matches!(app.read(&mut buf).await, Ok(0) | Err(_)));

        // What comes after an end said it had no more is not passed on.
        let (mut app, socket) = pair().await;
        streams.carry(2, async move { Ok(socket) }, say.clone(), None);
        streams.bytes(2, b"all".to_vec());
        streams.end(2);
        streams.bytes(2, b" and more".to_vec());
        let mut all = Vec::new();
        app.read_to_end(&mut all).await.unwrap();
        assert_eq!(all, b"all");
        streams.dropped(2);
    }

    #[tokio::test]
    async fn the_connections_of_one_port_are_let_go_together() {
        let streams = Streams::default();
        let dropped = Arc::new(Mutex::new(Vec::new()));
        let heard = dropped.clone();
        let say: Say = Arc::new(move |said| {
            if let Said::Drop(id, _) = said {
                heard.lock().unwrap().push(id);
            }
        });
        let (mut one, a) = pair().await;
        let (mut two, b) = pair().await;
        let (mut other, c) = pair().await;
        streams.carry_for(1, 3000, async move { Ok(a) }, say.clone(), None);
        streams.carry_for(2, 3000, async move { Ok(b) }, say.clone(), None);
        streams.carry_for(3, 8000, async move { Ok(c) }, say.clone(), None);
        streams.drop_of(3000);
        let mut said = dropped.lock().unwrap().clone();
        said.sort();
        assert_eq!(said, vec![1, 2]);
        let mut buf = [0u8; 8];
        assert!(matches!(one.read(&mut buf).await, Ok(0) | Err(_)));
        assert!(matches!(two.read(&mut buf).await, Ok(0) | Err(_)));
        // The other port's is as it was.
        assert_eq!(streams.len(), 1);
        streams.bytes(3, b"still".to_vec());
        other.read_exact(&mut buf[..5]).await.unwrap();
        assert_eq!(&buf[..5], b"still");
        streams.clear();
    }
}
