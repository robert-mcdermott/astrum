//! A server on the network (filled in later in this slice).

use super::{Client, Failure, OnEvent};

pub fn start(_url: &str, _headers: &[(String, String)], _token: Option<String>, _on_event: OnEvent) -> Result<Client, Failure> {
    Err(Failure::Broken("servers on the network are not supported yet".into()))
}

/// The version both sides have agreed on, to be sent with every request.
pub fn agreed(_client: &Client, _protocol: &str) {}

/// Is this an address on this machine, where a connection is not seen by anyone else?
pub fn is_this_machine(url: &str) -> bool {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit_once('@').map(|(_, h)| h).unwrap_or(host);
    let host = if host.starts_with('[') { host.split(']').next().unwrap_or("").trim_start_matches('[') } else { host.split(':').next().unwrap_or("") };
    matches!(host, "localhost" | "127.0.0.1" | "::1") || host.ends_with(".localhost")
}
