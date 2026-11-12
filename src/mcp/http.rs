//! A server on the network (filled in later in this slice).

use super::{Client, Failure, OnEvent};

pub fn start(_url: &str, _headers: &[(String, String)], _token: Option<String>, _on_event: OnEvent) -> Result<Client, Failure> {
    Err(Failure::Broken("servers on the network are not supported yet".into()))
}

/// The version both sides have agreed on, to be sent with every request.
pub fn agreed(_client: &Client, _protocol: &str) {}
