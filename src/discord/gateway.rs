//! A minimal Discord gateway session for one event: `INTERACTION_CREATE`.
//! Messages stay on REST polling; the gateway only delivers button presses
//! and modal answers, which Discord sends nowhere else without a public HTTPS
//! endpoint. It identifies with no intents (interactions need none), keeps the
//! heartbeat, answers each press on this node's messages within Discord's
//! three seconds (the answer modal, or "the message will be updated"), and
//! hands the press to the bridge loop, which decides it. It reconnects with
//! backoff and never resumes: a press during a reconnect fails visibly in
//! Discord and can be repeated.
use super::components::{self, Press};
use serde_json::{Value, json};
use std::{io, sync::mpsc, time::{Duration, Instant}};
use tungstenite::{Message, stream::MaybeTlsStream};

const GATEWAY: &str = "wss://gateway.discord.gg/?v=10&encoding=json";

/// Why a session ended.
enum End {
    /// Reconnect now (Discord asked, or the session was invalidated).
    Reconnect,
    /// Reconnect after a backoff.
    Failed(&'static str),
    /// Do not reconnect: the bridge stopped listening or the credential was refused.
    Stop(&'static str),
}

/// Starts the gateway thread; presses arrive on the returned receiver.
pub(super) fn spawn(token: String, tag: String) -> mpsc::Receiver<Press> {
    let (sender, receiver) = mpsc::channel();
    let started = std::thread::Builder::new().name("discord-gateway".into()).spawn(move || {
        let Ok(client) = reqwest::blocking::Client::builder().timeout(Duration::from_secs(5)).redirect(reqwest::redirect::Policy::none()).build() else {
            eprintln!("Could not construct the Discord interaction client; buttons are unavailable.");
            return;
        };
        let mut backoff = 2u64;
        loop {
            match session(&token, &tag, &client, &sender) {
                End::Reconnect => backoff = 2,
                End::Failed(message) => {
                    eprintln!("{message} Buttons reconnect in {backoff} s.");
                    std::thread::sleep(Duration::from_secs(backoff));
                    backoff = (backoff * 2).min(300);
                }
                End::Stop(message) => {
                    if !message.is_empty() {
                        eprintln!("{message}");
                    }
                    return;
                }
            }
        }
    });
    if started.is_err() {
        eprintln!("Could not start the Discord gateway; buttons are unavailable.");
    }
    receiver
}

type Socket = tungstenite::WebSocket<MaybeTlsStream<std::net::TcpStream>>;

fn send(socket: &mut Socket, payload: &Value) -> Result<(), End> {
    socket.send(Message::text(payload.to_string())).map_err(|_| End::Failed("The Discord gateway connection failed."))
}

fn session(token: &str, tag: &str, client: &reqwest::blocking::Client, sender: &mpsc::Sender<Press>) -> End {
    match run(token, tag, client, sender) {
        Ok(never) => match never {},
        Err(end) => end,
    }
}

fn run(token: &str, tag: &str, client: &reqwest::blocking::Client, sender: &mpsc::Sender<Press>) -> Result<std::convert::Infallible, End> {
    let (mut socket, _) = tungstenite::connect(GATEWAY).map_err(|_| End::Failed("Could not reach the Discord gateway."))?;
    // Short reads interleave the heartbeat with waiting for events.
    let stream = match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => stream,
        MaybeTlsStream::Rustls(stream) => &mut stream.sock,
        _ => return Err(End::Failed("Unexpected Discord gateway transport.")),
    };
    stream.set_read_timeout(Some(Duration::from_millis(500))).map_err(|_| End::Failed("Could not configure the Discord gateway."))?;
    let mut interval = None;
    let mut sequence = Value::Null;
    let mut beat = Instant::now();
    let mut acknowledged = true;
    loop {
        if let Some(every) = interval
            && Instant::now() >= beat
        {
            if !acknowledged {
                return Err(End::Failed("The Discord gateway stopped acknowledging heartbeats."));
            }
            send(&mut socket, &json!({"op":1,"d":sequence}))?;
            acknowledged = false;
            beat = Instant::now() + every;
        }
        let text = match socket.read() {
            Ok(Message::Text(text)) => text,
            Ok(Message::Close(frame)) => {
                let code = frame.map(|f| u16::from(f.code)).unwrap_or(0);
                return Err(match code {
                    4004 => End::Stop("Discord rejected the bot credential on the gateway; buttons are unavailable."),
                    4010..=4014 => End::Stop("Discord refused the gateway session; buttons are unavailable."),
                    _ => End::Failed("Discord closed the gateway connection."),
                });
            }
            Ok(_) => continue,
            Err(tungstenite::Error::Io(error)) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => continue,
            Err(_) => return Err(End::Failed("The Discord gateway connection failed.")),
        };
        let Ok(event) = serde_json::from_str::<Value>(&text) else { continue };
        if !event["s"].is_null() {
            sequence = event["s"].clone();
        }
        match event["op"].as_u64() {
            // Hello: start heartbeating (the first after a jittered fraction) and identify.
            Some(10) => {
                let every = Duration::from_millis(event["d"]["heartbeat_interval"].as_u64().unwrap_or(41250).clamp(1000, 120_000));
                interval = Some(every);
                beat = Instant::now() + every.mul_f64(f64::from(std::process::id() % 100) / 100.0);
                send(&mut socket, &json!({"op":2,"d":{"token":token,"intents":0,"properties":{"os":std::env::consts::OS,"browser":"control","device":"control"}}}))?;
            }
            Some(1) => {
                send(&mut socket, &json!({"op":1,"d":sequence}))?;
            }
            Some(11) => acknowledged = true,
            Some(7) => return Err(End::Reconnect),
            Some(9) => {
                std::thread::sleep(Duration::from_secs(3));
                return Err(End::Reconnect);
            }
            Some(0) if event["t"] == "INTERACTION_CREATE" => {
                let Some(press) = components::press(&event["d"], tag) else { continue };
                let url = format!("{}/interactions/{}/{}/callback", super::bridge::API, press.id, press.token);
                let acknowledged = client.post(url).json(&press.response(tag)).send().is_ok_and(|r| r.status().is_success());
                if !acknowledged {
                    eprintln!("Discord did not accept a button acknowledgement.");
                }
                if acknowledged && !press.prompts() && sender.send(press).is_err() {
                    return Err(End::Stop(""));
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
