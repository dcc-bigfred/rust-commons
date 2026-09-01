//! Resilient dcc-bus WebSocket client.
//!
//! Two long-lived sockets are kept (each with its own keep-alive pings):
//!
//! * **programming** — organizer JWT, used for CV / address frames
//!   (`loco.cvRead`, `loco.cvWrite`, `loco.addrGet`, `loco.addrSet`).
//! * **drive** — organizer JWT + `X-BigFred-Impersonate-As` for the
//!   currently selected participant; used for ops-track pulses (F2).
//!   Replaced when the wizard picks a different user.
//!
//! Both are warmed eagerly (`ensure_connected` after login,
//! `ensure_drive` when a participant is selected) and re-dialled with
//! exponential backoff if they die.
//!
//! The daemon that actually drives the command station is spawned by
//! BigFred when a layout session selects the station. If no daemon is
//! listening the proxy answers `503`, which surfaces here as
//! [`Error::DccBusUnreachable`] — the organizer has to open the layout
//! in BigFred once so the station comes up.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use rand::Rng;
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::HeaderName;
use tokio_tungstenite::tungstenite::Message;

use crate::config::BigFredConfig;
use crate::error::{Error, Result};
use crate::wire::{Ack, CommandStation, Envelope, Status, FRAME_SET_FUNCTION, IMPERSONATE_HEADER};

const ACK_TIMEOUT: Duration = Duration::from_secs(30);
/// Keep-alive for both permanent sockets. Must stay below dcc-bus deadman
/// (default 6s; BigFred heartbeat is 2s).
const PING_INTERVAL: Duration = Duration::from_secs(2);
const CONNECT_ATTEMPTS: u32 = 3;
const BACKOFF_BASE_MS: u64 = 250;
const BACKOFF_MAX_MS: u64 = 4_000;
/// Best-effort timeout for the trailing `OFF` of a function pulse. Shorter
/// than [`ACK_TIMEOUT`] so a pulse never blocks the organizer for half a
/// minute; the ON ack still uses [`ACK_TIMEOUT`] because a stuck command
/// station is worth surfacing verbatim.
const PULSE_OFF_TIMEOUT: Duration = Duration::from_secs(5);

type Pending = Arc<StdMutex<HashMap<String, oneshot::Sender<Ack>>>>;

/// A live socket plus the tasks that keep it readable and warm.
struct Session {
    tx: mpsc::UnboundedSender<Message>,
    pending: Pending,
    alive: Arc<AtomicBool>,
    command_station_id: u64,
    tasks: Vec<JoinHandle<()>>,
}

impl Session {
    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed) && !self.tx.is_closed()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
        for task in self.tasks.drain(..) {
            task.abort();
        }
    }
}

/// Impersonated drive socket keyed by participant login.
struct DriveSession {
    login: String,
    session: Session,
}

/// Owns two dcc-bus sockets: organizer programming + participant drive.
pub struct DccBusClient {
    cfg: Arc<RwLock<BigFredConfig>>,
    http: reqwest::Client,
    programming: Mutex<Option<Session>>,
    drive: Mutex<Option<DriveSession>>,
    status: StdMutex<Status>,
}

impl DccBusClient {
    pub fn new(cfg: Arc<RwLock<BigFredConfig>>, http: reqwest::Client) -> Self {
        Self {
            cfg,
            http,
            programming: Mutex::new(None),
            drive: Mutex::new(None),
            status: StdMutex::new(Status::default()),
        }
    }

    fn lock_status(&self) -> std::sync::MutexGuard<'_, Status> {
        self.status.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("status mutex poisoned — recovering inner guard");
            poisoned.into_inner()
        })
    }

    pub fn status(&self) -> Status {
        self.lock_status().clone()
    }

    fn refresh_drive_status(&self, drive: &Option<DriveSession>) {
        let mut status = self.lock_status();
        match drive {
            Some(d) if d.session.is_alive() => {
                status.drive_connected = true;
                status.drive_as = Some(d.login.clone());
            }
            _ => {
                status.drive_connected = false;
                status.drive_as = None;
            }
        }
    }

    /// Opens (or reuses) the organizer programming WebSocket. No-op when a
    /// live session already exists — used to warm the link right after login.
    pub async fn ensure_connected(&self, token: &str) -> Result<Status> {
        let mut guard = self.programming.lock().await;
        if guard.as_ref().is_some_and(|s| !s.is_alive()) {
            *guard = None;
        }
        if guard.is_none() {
            *guard = Some(self.connect_with_backoff_as(token, None).await?);
        }
        drop(guard);
        Ok(self.status())
    }

    /// Opens (or switches) the impersonated drive WebSocket for `as_login`.
    /// Reuses the existing socket when it is alive and already that user.
    pub async fn ensure_drive(&self, token: &str, as_login: &str) -> Result<Status> {
        let login = as_login.trim();
        if login.is_empty() {
            return Err(Error::ImpersonateRequired);
        }
        let mut guard = self.drive.lock().await;
        let reuse = guard
            .as_ref()
            .is_some_and(|d| d.login == login && d.session.is_alive());
        if !reuse {
            if let Some(prev) = guard.take() {
                tracing::info!(
                    previous = %prev.login,
                    next = %login,
                    "dcc-bus drive session switching user"
                );
            }
            let session = self.connect_with_backoff_as(token, Some(login)).await?;
            *guard = Some(DriveSession {
                login: login.to_string(),
                session,
            });
        }
        self.refresh_drive_status(&guard);
        drop(guard);
        Ok(self.status())
    }

    /// Sends one programming frame and waits for the matching `ack`.
    /// Reconnects once if the cached programming socket turned out to be dead.
    pub async fn request(
        &self,
        token: &str,
        frame: &str,
        payload: serde_json::Value,
    ) -> Result<Ack> {
        let mut guard = self.programming.lock().await;
        if guard.as_ref().is_some_and(|s| !s.is_alive()) {
            *guard = None;
        }
        if guard.is_none() {
            *guard = Some(self.connect_with_backoff_as(token, None).await?);
        }

        match send_and_wait(
            guard.as_ref().ok_or(Error::DccBusSessionLost)?,
            frame,
            payload.clone(),
        )
        .await
        {
            Ok(ack) => Ok(ack),
            Err(err) if err.is_dcc_bus_unavailable() => {
                *guard = None;
                *guard = Some(self.connect_with_backoff_as(token, None).await?);
                send_and_wait(
                    guard.as_ref().ok_or(Error::DccBusSessionLost)?,
                    frame,
                    payload,
                )
                .await
            }
            Err(err) => Err(err),
        }
    }

    /// Impersonated on→wait→off for one function on the cached drive socket.
    ///
    /// # Rollback
    ///
    /// If the `OFF` frame fails or the caller drops this future mid-pulse,
    /// a best-effort fire-and-forget `OFF` is still emitted so the function
    /// does not stay latched on the ops track. The `OFF` ack waits at most
    /// [`PULSE_OFF_TIMEOUT`].
    pub async fn pulse_function(
        &self,
        token: &str,
        as_login: &str,
        address: u16,
        function: u8,
        duration_ms: u64,
    ) -> Result<Ack> {
        let login = as_login.trim();
        if login.is_empty() {
            return Err(Error::ImpersonateRequired);
        }

        let mut guard = self.drive.lock().await;
        let reuse = guard
            .as_ref()
            .is_some_and(|d| d.login == login && d.session.is_alive());
        if !reuse {
            let _ = guard.take();
            let session = self.connect_with_backoff_as(token, Some(login)).await?;
            *guard = Some(DriveSession {
                login: login.to_string(),
                session,
            });
            self.refresh_drive_status(&guard);
        }

        let session = &guard.as_ref().ok_or(Error::DccBusDriveSessionLost)?.session;

        send_and_wait(
            session,
            FRAME_SET_FUNCTION,
            serde_json::json!({
                "address": address,
                "function": function,
                "on": true,
            }),
        )
        .await?;

        let pulse_guard = PulseOffGuard {
            tx: session.tx.clone(),
            address,
            function,
            armed: true,
        };

        tokio::time::sleep(Duration::from_millis(duration_ms)).await;

        pulse_guard.disarm_and_send_off(session).await
    }

    /// Picks the programming-capable command station with the lowest id.
    /// When `fixed_dcc_bus` is set, those values skip catalogue autodetection.
    pub async fn pick_station(&self, token: &str) -> Result<CommandStation> {
        let fixed = self.cfg.read().await.fixed_dcc_bus;
        if let Some((cs_id, _)) = fixed {
            return Ok(CommandStation {
                id: cs_id,
                name: format!("dcc-bus #{cs_id}"),
                programming: true,
                ..CommandStation::default()
            });
        }
        let api_base = self.cfg.read().await.api_base.clone();
        let url = format!("{api_base}/api/v1/command-stations/catalogue");
        let res = self
            .http
            .get(&url)
            .header("authorization", format!("Bearer {token}"))
            .send()
            .await
            .map_err(|err| Error::OauthUnreachable(err.to_string()))?;
        let status = res.status();
        let bytes = res.bytes().await.unwrap_or_default();
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(Error::Unauthorized);
        }
        if !status.is_success() {
            return Err(Error::CatalogueUnavailable(
                String::from_utf8_lossy(&bytes).to_string(),
            ));
        }
        let mut stations: Vec<CommandStation> = serde_json::from_slice(&bytes)
            .map_err(|err| Error::CatalogueBadResponse(err.to_string()))?;
        stations.sort_by_key(|s| s.id);
        stations
            .into_iter()
            .find(|s| s.programming)
            .ok_or(Error::NoProgrammingStation)
    }

    async fn connect_with_backoff_as(
        &self,
        token: &str,
        as_login: Option<&str>,
    ) -> Result<Session> {
        let mut last: Option<Error> = None;
        for attempt in 0..CONNECT_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(backoff_delay(attempt)).await;
            }
            match self.connect(token, as_login).await {
                Ok(session) => {
                    if as_login.is_none() {
                        let mut status = self.lock_status();
                        status.connected = true;
                        status.last_error = None;
                        if attempt > 0 {
                            status.reconnects += 1;
                        }
                    }
                    return Ok(session);
                }
                Err(err) => {
                    tracing::warn!(
                        attempt,
                        error = %err.code(),
                        as_login = as_login.unwrap_or(""),
                        "dcc-bus connect failed"
                    );
                    if as_login.is_none() {
                        let mut status = self.lock_status();
                        status.connected = false;
                        status.last_error = Some(err.code());
                    }
                    last = Some(err);
                }
            }
        }
        Err(last.unwrap_or_else(|| Error::DccBusUnreachable(String::new())))
    }

    async fn connect(&self, token: &str, as_login: Option<&str>) -> Result<Session> {
        let station = self.pick_station(token).await?;
        if as_login.is_none() {
            let mut status = self.lock_status();
            status.command_station_id = Some(station.id);
            status.command_station_name = Some(station.name.clone());
            status.default_programming_track_output =
                Some(station.default_programming_track_output.clone());
        }

        let url = format!(
            "{}/api/v1/dcc-bus/{}/ws?token={}",
            self.cfg.read().await.ws_base,
            station.id,
            urlencode(token)
        );
        let mut req = url
            .into_client_request()
            .map_err(|err| Error::DccBusBadUrl(err.to_string()))?;
        if let Some(login) = as_login {
            let name = HeaderName::from_static(IMPERSONATE_HEADER);
            let value = login.parse().map_err(|_| Error::InvalidImpersonateLogin)?;
            req.headers_mut().insert(name, value);
        }
        let (stream, _) = tokio_tungstenite::connect_async(req)
            .await
            .map_err(|err| Error::DccBusUnreachable(err.to_string()))?;

        let (mut sink, mut source) = stream.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        let pending: Pending = Arc::new(StdMutex::new(HashMap::new()));
        let alive = Arc::new(AtomicBool::new(true));

        let writer = tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                if sink.send(msg).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });

        let reader_pending = Arc::clone(&pending);
        let reader_alive = Arc::clone(&alive);
        let reader = tokio::spawn(async move {
            while let Some(Ok(msg)) = source.next().await {
                let text = match msg {
                    Message::Text(text) => text,
                    Message::Binary(bin) => match String::from_utf8(bin) {
                        Ok(text) => text,
                        Err(_) => continue,
                    },
                    Message::Close(_) => break,
                    _ => continue,
                };
                let Ok(env) = serde_json::from_str::<Envelope>(&text) else {
                    continue;
                };
                if env.kind != "ack" {
                    continue;
                }
                let Some(id) = env.id else { continue };
                let waiter = reader_pending
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&id);
                if let Some(waiter) = waiter {
                    let ack = env
                        .payload
                        .and_then(|p| serde_json::from_value::<Ack>(p).ok())
                        .unwrap_or_default();
                    let _ = waiter.send(ack);
                }
            }
            reader_alive.store(false, Ordering::Relaxed);
            reader_pending
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clear();
        });

        let ping_tx = tx.clone();
        let pinger = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(PING_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                let Ok(frame) = serde_json::to_string(&Envelope {
                    kind: "ping".to_string(),
                    id: None,
                    payload: Some(serde_json::json!({})),
                }) else {
                    break;
                };
                if ping_tx.send(Message::Text(frame)).is_err() {
                    break;
                }
            }
        });

        tracing::info!(
            command_station = station.id,
            name = %station.name,
            as_login = as_login.unwrap_or(""),
            role = if as_login.is_some() {
                "drive"
            } else {
                "programming"
            },
            "dcc-bus connected"
        );
        Ok(Session {
            tx,
            pending,
            alive,
            command_station_id: station.id,
            tasks: vec![writer, reader, pinger],
        })
    }
}

async fn send_and_wait(session: &Session, frame: &str, payload: serde_json::Value) -> Result<Ack> {
    let id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = oneshot::channel();
    {
        let mut pending = session
            .pending
            .lock()
            .map_err(|_| Error::DccBusPendingPoisoned)?;
        pending.insert(id.clone(), tx);
    }

    let envelope = serde_json::to_string(&Envelope {
        kind: frame.to_string(),
        id: Some(id.clone()),
        payload: Some(payload),
    })
    .map_err(|err| Error::FrameEncodeFailed(err.to_string()))?;

    if session.tx.send(Message::Text(envelope)).is_err() {
        if let Ok(mut pending) = session.pending.lock() {
            pending.remove(&id);
        }
        return Err(Error::DccBusUnreachable(String::new()));
    }

    match tokio::time::timeout(ACK_TIMEOUT, rx).await {
        Ok(Ok(ack)) if ack.ok => Ok(ack),
        Ok(Ok(ack)) => Err(Error::BadStatus {
            status: 502,
            code: ack
                .error
                .unwrap_or_else(|| "programming_failed".to_string()),
            detail: Some(format!("command station {}", session.command_station_id)),
        }),
        Ok(Err(_)) => Err(Error::DccBusUnreachable(String::new())),
        Err(_) => {
            if let Ok(mut pending) = session.pending.lock() {
                pending.remove(&id);
            }
            Err(Error::ProgrammingTimeout)
        }
    }
}

struct PulseOffGuard {
    tx: mpsc::UnboundedSender<Message>,
    address: u16,
    function: u8,
    armed: bool,
}

impl PulseOffGuard {
    async fn disarm_and_send_off(mut self, session: &Session) -> Result<Ack> {
        self.armed = false;
        let (tx, rx) = oneshot::channel();
        let id = uuid::Uuid::new_v4().to_string();
        {
            let mut pending = session
                .pending
                .lock()
                .map_err(|_| Error::DccBusPendingPoisoned)?;
            pending.insert(id.clone(), tx);
        }
        let envelope = serde_json::to_string(&Envelope {
            kind: FRAME_SET_FUNCTION.to_string(),
            id: Some(id.clone()),
            payload: Some(serde_json::json!({
                "address": self.address,
                "function": self.function,
                "on": false,
            })),
        })
        .map_err(|err| Error::FrameEncodeFailed(err.to_string()))?;

        if self.tx.send(Message::Text(envelope)).is_err() {
            if let Ok(mut pending) = session.pending.lock() {
                pending.remove(&id);
            }
            return Err(Error::DccBusUnreachable(String::new()));
        }

        match tokio::time::timeout(PULSE_OFF_TIMEOUT, rx).await {
            Ok(Ok(ack)) if ack.ok => Ok(ack),
            Ok(Ok(ack)) => Err(Error::BadStatus {
                status: 502,
                code: ack
                    .error
                    .unwrap_or_else(|| "function_off_failed".to_string()),
                detail: Some(format!("command station {}", session.command_station_id)),
            }),
            Ok(Err(_)) => Err(Error::DccBusUnreachable(String::new())),
            Err(_) => {
                if let Ok(mut pending) = session.pending.lock() {
                    pending.remove(&id);
                }
                Err(Error::FunctionOffTimeout)
            }
        }
    }
}

impl Drop for PulseOffGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let id = uuid::Uuid::new_v4().to_string();
        let Ok(frame) = serde_json::to_string(&Envelope {
            kind: FRAME_SET_FUNCTION.to_string(),
            id: Some(id),
            payload: Some(serde_json::json!({
                "address": self.address,
                "function": self.function,
                "on": false,
            })),
        }) else {
            return;
        };
        let _ = self.tx.send(Message::Text(frame));
    }
}

fn backoff_delay(attempt: u32) -> Duration {
    let exp = BACKOFF_BASE_MS.saturating_mul(1u64 << attempt.min(6));
    let capped = exp.min(BACKOFF_MAX_MS);
    let jitter = rand::thread_rng().gen_range(0..=capped / 2);
    Duration::from_millis(capped / 2 + jitter)
}

fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_config;

    #[test]
    fn backoff_grows_and_is_capped() {
        for attempt in 0..8 {
            let delay = backoff_delay(attempt).as_millis() as u64;
            assert!(delay <= BACKOFF_MAX_MS, "attempt {attempt} → {delay}ms");
        }
        assert!(backoff_delay(0).as_millis() >= (BACKOFF_BASE_MS / 2) as u128);
    }

    #[test]
    fn urlencode_keeps_jwt_alphabet() {
        assert_eq!(urlencode("abcABC123-_.~"), "abcABC123-_.~");
        assert_eq!(urlencode("a+b/c=d"), "a%2Bb%2Fc%3Dd");
    }

    #[test]
    fn ack_parses_cv_results() {
        let ack: Ack = serde_json::from_str(
            r#"{"ok":true,"cvs":[{"cv":1,"value":3}],"locoAddress":3,"longAddress":false}"#,
        )
        .unwrap();
        assert!(ack.ok);
        assert_eq!(ack.cvs.unwrap()[0].cv, 1);
        assert_eq!(ack.loco_address, Some(3));
    }

    #[tokio::test]
    async fn pick_station_uses_fixed_dcc_bus() {
        let tmp = std::env::temp_dir().join(format!("bf-dcc-{}", uuid::Uuid::new_v4()));
        let mut cfg = test_config(tmp);
        cfg.fixed_dcc_bus = Some((7, 1));
        let client = DccBusClient::new(Arc::new(RwLock::new(cfg)), reqwest::Client::new());
        let station = client.pick_station("token").await.expect("fixed");
        assert_eq!(station.id, 7);
        assert!(station.programming);
        assert_eq!(station.name, "dcc-bus #7");
    }
}
