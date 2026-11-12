//! Wire protocol between the thin client and the session server.
//!
//! Frames are `[tag: u8][len: u32 LE][payload]`. Control messages are JSON
//! (tag 0); terminal output from the server is raw bytes (tag 1) so the hot
//! path never pays for encoding.

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const VERSION: u32 = 1;
pub const TAG_MSG: u8 = 0;
pub const TAG_OUTPUT: u8 = 1;
const MAX_FRAME: usize = 64 * 1024 * 1024;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Hello {
    pub version: u32,
    pub cols: u16,
    pub rows: u16,
    pub cwd: Option<String>,
    /// True for clients that only want to issue a control request.
    pub control: bool,
    /// Whether the client's terminal understands 24-bit color sequences.
    /// Absent from older clients, which are assumed to.
    #[serde(default)]
    pub truecolor: Option<bool>,
    /// The client's terminal's own text and background colors (red, green,
    /// blue), where it would say; programs that ask are told these.
    #[serde(default)]
    pub fg: Option<[u8; 3]>,
    #[serde(default)]
    pub bg: Option<[u8; 3]>,
    /// The client knows `ServerMsg::OpenUrl`. (One that does not would
    /// stumble over it.)
    #[serde(default)]
    pub opens_links: bool,
    /// Driven by a script, not by a person (`debug-snapshot`): there is
    /// nobody to show the first-run walkthrough to.
    #[serde(default)]
    pub scripted: bool,
    /// The ways the client's terminal can show a picture.
    #[serde(default)]
    pub images: crate::image::Caps,
    /// The size of one of its cells in pixels, where it says.
    #[serde(default)]
    pub cell_px: (u16, u16),
    /// How its terminal shows a notification (`osc9`, `osc99`, `osc777`),
    /// if it is known to show one.
    #[serde(default)]
    pub notify: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ClientMsg {
    Hello(Hello),
    /// An input event from the outer terminal (key, mouse, paste, resize, focus).
    Event(crossterm::event::Event),
    Detach,
    /// Control requests (`astrum ls`, `astrum kill`).
    Info,
    Kill,
    /// `astrum reload`: read the providers again. `env` carries, from the
    /// shell the request was made in, the value of each environment
    /// variable a provider takes its key from (empty when unset).
    Reload { env: Vec<(String, String)> },
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ServerMsg {
    /// The server is done with this client; restore the terminal and exit.
    Bye { reason: String },
    /// Put text on the system clipboard of the machine the client runs on.
    Clipboard(String),
    /// Open a link with whatever that machine opens links with.
    OpenUrl(String),
    Info(SessionInfo),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SessionInfo {
    pub session: String,
    pub pid: u32,
    pub tabs: Vec<String>,
    pub clients: usize,
    pub started_unix: u64,
    /// How much drawing the session has done, for measuring it.
    #[serde(default)]
    pub work: Work,
}

/// What a session has spent on taking in program output and drawing
/// frames since it started (times in microseconds).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq)]
pub struct Work {
    /// Frames drawn, counted once for each terminal sent one.
    pub frames: u64,
    /// Laying out a frame: the panes, the bars, the panel.
    pub draw_us: u64,
    /// Finding what changed since the last frame and writing that out.
    pub send_us: u64,
    /// Bytes sent to terminals.
    pub sent: u64,
    /// Bytes of program output taken in, and the time that took.
    pub read: u64,
    pub read_us: u64,
}

pub enum ServerFrame {
    Msg(ServerMsg),
    Output(Vec<u8>),
}

// ---- blocking (client side) -------------------------------------------------

pub fn write_frame<W: Write>(w: &mut W, tag: u8, payload: &[u8]) -> io::Result<()> {
    let mut head = [0u8; 5];
    head[0] = tag;
    head[1..].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    w.write_all(&head)?;
    w.write_all(payload)?;
    w.flush()
}

pub fn read_frame<R: Read>(r: &mut R) -> io::Result<(u8, Vec<u8>)> {
    let mut head = [0u8; 5];
    r.read_exact(&mut head)?;
    let len = u32::from_le_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok((head[0], buf))
}

pub fn send_client_msg<W: Write>(w: &mut W, msg: &ClientMsg) -> io::Result<()> {
    let payload = serde_json::to_vec(msg).map_err(io::Error::other)?;
    write_frame(w, TAG_MSG, &payload)
}

pub fn read_server_frame<R: Read>(r: &mut R) -> io::Result<ServerFrame> {
    let (tag, payload) = read_frame(r)?;
    match tag {
        TAG_OUTPUT => Ok(ServerFrame::Output(payload)),
        _ => serde_json::from_slice(&payload).map(ServerFrame::Msg).map_err(io::Error::other),
    }
}

// ---- async (server side) ----------------------------------------------------

pub async fn read_client_msg<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<ClientMsg> {
    let mut head = [0u8; 5];
    r.read_exact(&mut head).await?;
    let len = u32::from_le_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    serde_json::from_slice(&buf).map_err(io::Error::other)
}

pub async fn write_server_frame<W: AsyncWrite + Unpin>(w: &mut W, frame: &ServerFrame) -> io::Result<()> {
    let (tag, owned);
    let payload: &[u8] = match frame {
        ServerFrame::Output(bytes) => {
            tag = TAG_OUTPUT;
            bytes
        }
        ServerFrame::Msg(msg) => {
            tag = TAG_MSG;
            owned = serde_json::to_vec(msg).map_err(io::Error::other)?;
            &owned
        }
    };
    let mut head = [0u8; 5];
    head[0] = tag;
    head[1..].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    w.write_all(&head).await?;
    w.write_all(payload).await?;
    w.flush().await
}
