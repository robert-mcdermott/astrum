//! A server that is a program on this machine: one message per line on its
//! standard input and output. What it writes to its error output is kept
//! as its log.

use super::{Client, Cmd, Event, Failure, Log, OnEvent, from_server, keep_log, notify_msg, request_msg};
use serde_json::Value;
use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

/// A line longer than this is not a message.
const MAX_LINE: usize = 64 * 1024 * 1024;
/// How much of a line of its error output is kept (the log keeps less).
const MAX_LOG_LINE: usize = 4096;

/// The program's whole process group, ended when the connection is.
struct Group(i32);

impl Drop for Group {
    fn drop(&mut self) {
        // (Whatever it started is in its group too: `npx` runs `node`.)
        unsafe { libc::kill(-self.0, libc::SIGTERM) };
    }
}

pub fn start(program: &str, args: &[String], env: &[(String, String)], cwd: Option<&str>, on_event: OnEvent, log: Log) -> Result<Client, Failure> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args).env_clear().envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    let home = env.iter().find(|(k, _)| k == "HOME").map(|(_, v)| v.as_str()).unwrap_or("/");
    let dir = cwd.filter(|d| std::path::Path::new(d).is_dir()).unwrap_or(home);
    cmd.current_dir(dir).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0).kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| {
        Failure::Broken(match e.kind() {
            std::io::ErrorKind::NotFound => format!("`{program}` was not found"),
            _ => format!("`{program}` could not be started: {e}"),
        })
    })?;
    let group = Group(child.id().map(|p| p as i32).unwrap_or(0));
    let (Some(mut stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
        return Err(Failure::Broken("its input and output could not be connected".into()));
    };
    let (client, mut cmds) = Client::new(log);

    // Its error output is read to the end, whatever is in it: a program
    // whose error output is no longer read is stopped by the next thing it
    // writes there.
    let log = client.log.clone();
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut line = Vec::new();
        while let Ok(true) = log_line(&mut reader, &mut line).await {
            keep_log(&log, &String::from_utf8_lossy(&line));
        }
    });

    // What is sent goes through a task of its own: a program that is slow
    // to read never holds anything else up.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Value>();
    tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            let mut line = msg.to_string();
            line.push('\n');
            if stdin.write_all(line.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
                break;
            }
        }
        // (Its input closing is how a program is first asked to stop.)
    });

    let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Value>();
    let log = client.log.clone();
    tokio::spawn(async move {
        let mut reader = BufReader::with_capacity(256 * 1024, stdout);
        loop {
            match read_line(&mut reader).await {
                Ok(Some(line)) => match serde_json::from_slice::<Value>(&line) {
                    Ok(msg) if msg.is_object() => {
                        if in_tx.send(msg).is_err() {
                            break;
                        }
                    }
                    // A program that prints something else there is not
                    // speaking the protocol; what it printed is its log.
                    _ => keep_log(&log, &String::from_utf8_lossy(&line)),
                },
                Ok(None) | Err(_) => break,
            }
        }
    });

    let gone = client.gone.clone();
    let log = client.log.clone();
    tokio::spawn(async move {
        let mut pending = HashMap::new();
        let ended = loop {
            tokio::select! {
                cmd = cmds.recv() => match cmd {
                    Some(Cmd::Request { id, method, params, reply }) => {
                        pending.insert(id, reply);
                        let _ = out_tx.send(request_msg(id, &method, &params));
                    }
                    Some(Cmd::Notify { method, params }) => {
                        let _ = out_tx.send(notify_msg(&method, &params));
                    }
                    Some(Cmd::Forget { id }) => {
                        pending.remove(&id);
                    }
                    Some(Cmd::Agreed(_)) => {}
                    Some(Cmd::Close) | None => break None,
                },
                msg = in_rx.recv() => match msg {
                    Some(msg) => {
                        if let Some(answer) = from_server(&msg, &mut pending, &on_event) {
                            let _ = out_tx.send(answer);
                        }
                    }
                    None => {
                        // (A program's output closes a moment before it is
                        // gone: its exit status is waited for, briefly.)
                        let status = tokio::time::timeout(Duration::from_millis(1000), child.wait()).await.ok().and_then(Result::ok);
                        let last = log.lock().ok().and_then(|l| l.back().cloned());
                        let mut why = match status.and_then(|s| s.code()) {
                            Some(code) => format!("the program ended (exit {code})"),
                            None => "the program closed its output".to_string(),
                        };
                        if let Some(last) = last {
                            why.push_str(&format!("; it said: {last}"));
                        }
                        break Some(why);
                    }
                },
            }
        };
        gone.store(true, std::sync::atomic::Ordering::Relaxed);
        for (_, waiting) in pending.drain() {
            let _ = waiting.send(Err(Failure::Broken("the server stopped before it answered".into())));
        }
        // Asked first by closing its input, then told, then made to.
        drop(out_tx);
        if tokio::time::timeout(Duration::from_millis(1500), child.wait()).await.is_err() {
            drop(group);
            if tokio::time::timeout(Duration::from_millis(1500), child.wait()).await.is_err() {
                let _ = child.kill().await;
            }
        } else {
            drop(group);
        }
        if let Some(why) = ended {
            on_event(Event::Ended(why));
        }
    });
    Ok(client)
}

/// The next line of a program's error output into `line`, without its end:
/// at most `MAX_LOG_LINE` bytes of it, the rest of a longer one read and
/// let go. False at the end of the output.
async fn log_line(reader: &mut (impl AsyncBufRead + Unpin), line: &mut Vec<u8>) -> std::io::Result<bool> {
    line.clear();
    let mut any = false;
    loop {
        let buf = reader.fill_buf().await?;
        if buf.is_empty() {
            return Ok(any);
        }
        any = true;
        let (end, found) = match buf.iter().position(|b| *b == b'\n') {
            Some(at) => (at, true),
            None => (buf.len(), false),
        };
        let room = MAX_LOG_LINE.saturating_sub(line.len());
        line.extend_from_slice(&buf[..end.min(room)]);
        reader.consume(if found { end + 1 } else { end });
        if found {
            return Ok(true);
        }
    }
}

/// The next line, without its end; `None` at the end of the output.
async fn read_line(reader: &mut (impl AsyncBufRead + Unpin)) -> std::io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let buf = reader.fill_buf().await?;
        if buf.is_empty() {
            return Ok((!line.is_empty()).then_some(line));
        }
        match buf.iter().position(|b| *b == b'\n') {
            Some(at) => {
                line.extend_from_slice(&buf[..at]);
                reader.consume(at + 1);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(Some(line));
            }
            None => {
                let n = buf.len();
                line.extend_from_slice(buf);
                reader.consume(n);
                if line.len() > MAX_LINE {
                    return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "a line too long"));
                }
            }
        }
    }
}
