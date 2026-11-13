//! MQTT client (MQTT 3.1.1, TCP or TLS). Several sessions as tabs; each
//! connects to a broker, subscribes to topics, shows messages and a topic
//! overview, and publishes (also repeatedly). Scripts can publish through
//! a connected session with `mqtt_publish`.

use std::collections::{BTreeMap, VecDeque};
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText, Ui};
use rumqttc::{Client, Event, MqttOptions, Packet, QoS, TlsConfiguration, Transport};
use serde::{Deserialize, Serialize};

use crate::app::section;
use crate::codec;
use crate::sessions::{ACCENT, GREEN, RED, Session, Shared, big_button, status_line};

const MAX_MESSAGES: usize = 5000;
const MAX_TOPICS: usize = 5000;

fn qos(q: u8) -> QoS {
    match q {
        1 => QoS::AtLeastOnce,
        2 => QoS::ExactlyOnce,
        _ => QoS::AtMostOnce,
    }
}

fn qos_num(q: QoS) -> u8 {
    match q {
        QoS::AtMostOnce => 0,
        QoS::AtLeastOnce => 1,
        QoS::ExactlyOnce => 2,
    }
}

// ------------------------------------------------------------------ TLS

mod tls {
    use std::sync::Arc;

    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};

    /// Accepts any server certificate (for brokers with self-signed certificates).
    #[derive(Debug)]
    struct AcceptAll(Arc<CryptoProvider>);

    impl ServerCertVerifier for AcceptAll {
        fn verify_server_cert(
            &self,
            _: &CertificateDer<'_>,
            _: &[CertificateDer<'_>],
            _: &ServerName<'_>,
            _: &[u8],
            _: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            m: &[u8],
            c: &CertificateDer<'_>,
            d: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            verify_tls12_signature(m, c, d, &self.0.signature_verification_algorithms)
        }
        fn verify_tls13_signature(
            &self,
            m: &[u8],
            c: &CertificateDer<'_>,
            d: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            verify_tls13_signature(m, c, d, &self.0.signature_verification_algorithms)
        }
        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.0.signature_verification_algorithms.supported_schemes()
        }
    }

    pub fn config(insecure: bool, ca_file: &str) -> Result<ClientConfig, String> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?;
        if insecure {
            return Ok(builder.dangerous().with_custom_certificate_verifier(Arc::new(AcceptAll(provider))).with_no_client_auth());
        }
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        if !ca_file.trim().is_empty() {
            let certs = CertificateDer::pem_file_iter(ca_file.trim())
                .map_err(|e| format!("CA-Datei nicht lesbar: {e}"))?;
            let mut n = 0;
            for c in certs {
                roots.add(c.map_err(|e| format!("CA-Datei fehlerhaft: {e}"))?).map_err(|e| e.to_string())?;
                n += 1;
            }
            if n == 0 {
                return Err("Die CA-Datei enthält kein Zertifikat (PEM erwartet)".into());
            }
        }
        Ok(builder.with_root_certificates(roots).with_no_client_auth())
    }
}

// ------------------------------------------------------------------ worker

pub enum MqCmd {
    Subscribe(String, u8),
    Unsubscribe(String),
    Publish { topic: String, payload: Vec<u8>, qos: u8, retain: bool },
    Close,
}

enum MqEvent {
    Connected,
    Disconnected(String),
    Closed,
    Info(String),
    Error(String),
    Message { topic: String, payload: Vec<u8>, qos: u8, retain: bool },
    Sent { topic: String, payload: Vec<u8>, qos: u8, retain: bool },
}

struct MqLink {
    tx: Sender<MqCmd>,
    rx: Receiver<MqEvent>,
}

impl Drop for MqLink {
    fn drop(&mut self) {
        let _ = self.tx.send(MqCmd::Close);
    }
}

#[derive(Clone)]
struct ConnCfg {
    host: String,
    port: u16,
    tls: bool,
    insecure: bool,
    ca_file: String,
    client_id: String,
    user: String,
    password: String,
    keep_alive_s: u16,
    clean_session: bool,
    auto_reconnect: bool,
    subs: Vec<(String, u8)>,
}

fn spawn(ctx: &egui::Context, cfg: ConnCfg) -> MqLink {
    let (cmd_tx, cmd_rx) = channel();
    let (ev_tx, ev_rx) = channel();
    let ctx = ctx.clone();
    thread::spawn(move || {
        let out = |e: MqEvent| {
            let _ = ev_tx.send(e);
            ctx.request_repaint();
        };
        let mut opts = MqttOptions::new(cfg.client_id.clone(), cfg.host.trim(), cfg.port);
        opts.set_keep_alive(Duration::from_secs(cfg.keep_alive_s.max(5) as u64));
        opts.set_clean_session(cfg.clean_session);
        opts.set_max_packet_size(16 * 1024 * 1024, 16 * 1024 * 1024);
        if !cfg.user.is_empty() {
            opts.set_credentials(cfg.user.clone(), cfg.password.clone());
        }
        if cfg.tls {
            match tls::config(cfg.insecure, &cfg.ca_file) {
                Ok(c) => {
                    opts.set_transport(Transport::tls_with_config(TlsConfiguration::Rustls(Arc::new(c))));
                }
                Err(e) => {
                    out(MqEvent::Error(e));
                    out(MqEvent::Closed);
                    return;
                }
            }
        }
        out(MqEvent::Info(format!(
            "Verbinde mit {}:{}{} …",
            cfg.host.trim(),
            cfg.port,
            if cfg.tls { " (TLS)" } else { "" }
        )));
        let (client, mut conn) = Client::new(opts, 256);
        let mut subs = cfg.subs.clone();
        let mut connected = false;
        loop {
            loop {
                match cmd_rx.try_recv() {
                    Ok(MqCmd::Subscribe(t, q)) => {
                        subs.retain(|(x, _)| *x != t);
                        subs.push((t.clone(), q));
                        if connected {
                            if let Err(e) = client.try_subscribe(t.clone(), qos(q)) {
                                out(MqEvent::Error(format!("Abonnieren von {t} fehlgeschlagen: {e}")));
                            }
                        }
                    }
                    Ok(MqCmd::Unsubscribe(t)) => {
                        subs.retain(|(x, _)| *x != t);
                        if connected {
                            let _ = client.try_unsubscribe(t);
                        }
                    }
                    Ok(MqCmd::Publish { topic, payload, qos: q, retain }) => {
                        match client.try_publish(topic.clone(), qos(q), retain, payload.clone()) {
                            Ok(()) => out(MqEvent::Sent { topic, payload, qos: q, retain }),
                            Err(e) => out(MqEvent::Error(format!("Veröffentlichen fehlgeschlagen: {e}"))),
                        }
                    }
                    Ok(MqCmd::Close) | Err(TryRecvError::Disconnected) => {
                        let _ = client.try_disconnect();
                        for _ in 0..10 {
                            if !matches!(conn.recv_timeout(Duration::from_millis(50)), Ok(Ok(_))) {
                                break;
                            }
                        }
                        out(MqEvent::Closed);
                        return;
                    }
                    Err(TryRecvError::Empty) => break,
                }
            }
            match conn.recv_timeout(Duration::from_millis(50)) {
                Ok(Ok(Event::Incoming(Packet::ConnAck(ack)))) => {
                    connected = true;
                    out(MqEvent::Connected);
                    if ack.session_present {
                        out(MqEvent::Info("Broker hat die bisherige Sitzung fortgesetzt".into()));
                    }
                    for (t, q) in &subs {
                        if let Err(e) = client.try_subscribe(t.clone(), qos(*q)) {
                            out(MqEvent::Error(format!("Abonnieren von {t} fehlgeschlagen: {e}")));
                        }
                    }
                }
                Ok(Ok(Event::Incoming(Packet::Publish(p)))) => out(MqEvent::Message {
                    topic: p.topic.clone(),
                    payload: p.payload.to_vec(),
                    qos: qos_num(p.qos),
                    retain: p.retain,
                }),
                Ok(Ok(Event::Incoming(Packet::SubAck(a)))) => {
                    let failed = a.return_codes.iter().filter(|c| matches!(c, rumqttc::SubscribeReasonCode::Failure)).count();
                    if failed > 0 {
                        out(MqEvent::Error(format!("Broker hat {failed} Abo(s) abgelehnt (keine Berechtigung?)")));
                    }
                }
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    let msg = format!("{e}");
                    if connected {
                        connected = false;
                        out(MqEvent::Disconnected(msg));
                    } else {
                        out(MqEvent::Error(format!("Verbindung fehlgeschlagen: {msg}")));
                    }
                    if !cfg.auto_reconnect {
                        out(MqEvent::Closed);
                        return;
                    }
                    // wait before the next attempt, but react to "close" at once
                    match cmd_rx.recv_timeout(Duration::from_secs(3)) {
                        Ok(MqCmd::Close) | Err(RecvTimeoutError::Disconnected) => {
                            out(MqEvent::Closed);
                            return;
                        }
                        Ok(MqCmd::Subscribe(t, q)) => {
                            subs.retain(|(x, _)| *x != t);
                            subs.push((t, q));
                        }
                        Ok(MqCmd::Unsubscribe(t)) => subs.retain(|(x, _)| *x != t),
                        _ => {}
                    }
                    out(MqEvent::Info("Neuer Verbindungsversuch …".into()));
                }
                Err(rumqttc::RecvTimeoutError::Timeout) => {}
                Err(rumqttc::RecvTimeoutError::Disconnected) => {
                    out(MqEvent::Closed);
                    return;
                }
            }
        }
    });
    MqLink { tx: cmd_tx, rx: ev_rx }
}

// ------------------------------------------------------------------ registry for scripts

struct RegEntry {
    id: u64,
    name: String,
    connected: bool,
    tx: Sender<MqCmd>,
}

fn registry() -> &'static Mutex<Vec<RegEntry>> {
    static R: OnceLock<Mutex<Vec<RegEntry>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Vec::new()))
}

/// Publish from a script. `session`: name of an MQTT session, or empty for
/// the first connected one.
pub fn script_publish(session: &str, topic: &str, payload: Vec<u8>, q: u8, retain: bool) -> Result<(), String> {
    let reg = registry().lock().unwrap();
    let entry = reg
        .iter()
        .filter(|e| e.connected)
        .find(|e| session.is_empty() || e.name == session)
        .ok_or_else(|| {
            if session.is_empty() {
                "Keine MQTT-Sitzung verbunden".to_string()
            } else {
                format!("MQTT-Sitzung „{session}“ ist nicht verbunden")
            }
        })?;
    entry
        .tx
        .send(MqCmd::Publish { topic: topic.to_string(), payload, qos: q.min(2), retain })
        .map_err(|_| "MQTT-Sitzung nicht erreichbar".to_string())
}

// ------------------------------------------------------------------ session

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Sub {
    pub on: bool,
    pub topic: String,
    pub qos: u8,
}

impl Default for Sub {
    fn default() -> Self {
        Self { on: true, topic: String::new(), qos: 0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum MsgView {
    Text,
    Hex,
    Json,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MqttSaved {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub insecure: bool,
    pub ca_file: String,
    pub client_id: String,
    pub user: String,
    pub password: String,
    pub keep_alive_s: u16,
    pub clean_session: bool,
    pub auto_reconnect: bool,
    pub subs: Vec<Sub>,
    pub pub_topic: String,
    pub pub_payload: String,
    pub pub_qos: u8,
    pub pub_retain: bool,
    pub pub_hex: bool,
    pub pub_repeat: bool,
    pub pub_interval_ms: u64,
    pub view: MsgView,
    pub filter: String,
    pub show_sent: bool,
}

fn random_id() -> String {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("portwerk-{:06x}", (n ^ (n >> 24)) as u32 & 0xFF_FFFF)
}

impl Default for MqttSaved {
    fn default() -> Self {
        Self {
            name: "MQTT 1".into(),
            host: "localhost".into(),
            port: 1883,
            tls: false,
            insecure: false,
            ca_file: String::new(),
            client_id: random_id(),
            user: String::new(),
            password: String::new(),
            keep_alive_s: 30,
            clean_session: true,
            auto_reconnect: true,
            subs: Vec::new(),
            pub_topic: "portwerk/test".into(),
            pub_payload: "{\"wert\": 42}".into(),
            pub_qos: 0,
            pub_retain: false,
            pub_hex: false,
            pub_repeat: false,
            pub_interval_ms: 1000,
            view: MsgView::Text,
            filter: String::new(),
            show_sent: true,
        }
    }
}

struct Msg {
    time: String,
    topic: String,
    payload: Vec<u8>,
    qos: u8,
    retain: bool,
    sent: bool,
}

struct TopicStat {
    count: u64,
    last: Vec<u8>,
    time: String,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub struct MqttSession {
    id: u64,
    s: MqttSaved,
    link: Option<MqLink>,
    connected: bool,
    starting: bool,
    messages: VecDeque<Msg>,
    topics: BTreeMap<String, TopicStat>,
    events: VecDeque<(String, String, bool)>,
    selected: Option<usize>,
    paused: bool,
    received: u64,
    sent: u64,
    last_pub: Instant,
    new_sub: String,
    new_sub_qos: u8,
    log_file: Option<(String, File)>,
    /// A broad subscription waiting for confirmation: (topic, qos, existing index).
    confirm_broad: Option<(String, u8, Option<usize>)>,
    rate_start: Instant,
    rate_count: u32,
    rate: u32,
}

fn now() -> String {
    chrono::Local::now().format("%H:%M:%S%.3f").to_string()
}

fn format_payload(p: &[u8], view: MsgView, pretty: bool) -> String {
    match view {
        MsgView::Hex => codec::to_hex(p),
        MsgView::Json => match serde_json::from_slice::<serde_json::Value>(p) {
            Ok(v) if pretty => serde_json::to_string_pretty(&v).unwrap_or_default(),
            Ok(v) => serde_json::to_string(&v).unwrap_or_default(),
            Err(_) => String::from_utf8_lossy(p).into_owned(),
        },
        MsgView::Text => String::from_utf8_lossy(p).into_owned(),
    }
}

fn preview(p: &[u8], view: MsgView) -> String {
    let s = format_payload(p, view, false).replace(['\r', '\n', '\t'], " ");
    if s.chars().count() > 300 { format!("{}…", s.chars().take(300).collect::<String>()) } else { s }
}

/// Subscriptions like "#" or "+/#" that receive every topic of a broker.
pub fn is_broad(filter: &str) -> bool {
    let f = filter.trim();
    f.contains('#') && f.split('/').all(|l| l == "+" || l == "#")
}

/// Does `topic` match the subscription filter (with + and #)?
pub fn topic_matches(filter: &str, topic: &str) -> bool {
    let mut f = filter.split('/');
    let mut t = topic.split('/');
    loop {
        match (f.next(), t.next()) {
            (Some("#"), _) => return true,
            (Some("+"), Some(_)) => {}
            (Some(a), Some(b)) if a == b => {}
            (None, None) => return true,
            _ => return false,
        }
    }
}

impl MqttSession {
    pub fn new(name: String) -> Self {
        Self::from_saved(MqttSaved { name, ..Default::default() })
    }

    pub fn from_saved(mut s: MqttSaved) -> Self {
        // Earlier versions preset an active "#" subscription. Switch it off.
        let old_default = s.subs.len() == 1 && s.subs[0].on && s.subs[0].topic == "#" && s.subs[0].qos == 0;
        if old_default {
            s.subs[0].on = false;
        }
        let mut m = Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            s,
            link: None,
            connected: false,
            starting: false,
            messages: VecDeque::new(),
            topics: BTreeMap::new(),
            events: VecDeque::new(),
            selected: None,
            paused: false,
            received: 0,
            sent: 0,
            last_pub: Instant::now(),
            new_sub: String::new(),
            new_sub_qos: 0,
            log_file: None,
            confirm_broad: None,
            rate_start: Instant::now(),
            rate_count: 0,
            rate: 0,
        };
        m.event("Bereit. Broker eintragen, Themen abonnieren und „Verbinden“ klicken.", false);
        if old_default {
            m.event("Das voreingestellte Abo „#“ (alle Themen) wurde ausgeschaltet.", false);
        }
        m
    }

    pub fn saved(&self) -> MqttSaved {
        self.s.clone()
    }

    fn event(&mut self, msg: impl Into<String>, err: bool) {
        self.events.push_back((now(), msg.into(), err));
        while self.events.len() > 100 {
            self.events.pop_front();
        }
    }

    fn connect(&mut self, ctx: &egui::Context) {
        if self.s.client_id.trim().is_empty() {
            self.s.client_id = random_id();
        }
        let cfg = ConnCfg {
            host: self.s.host.clone(),
            port: self.s.port,
            tls: self.s.tls,
            insecure: self.s.insecure,
            ca_file: self.s.ca_file.clone(),
            client_id: self.s.client_id.trim().to_string(),
            user: self.s.user.clone(),
            password: self.s.password.clone(),
            keep_alive_s: self.s.keep_alive_s,
            clean_session: self.s.clean_session,
            auto_reconnect: self.s.auto_reconnect,
            subs: self.s.subs.iter().filter(|s| s.on && !s.topic.trim().is_empty()).map(|s| (s.topic.trim().to_string(), s.qos)).collect(),
        };
        self.link = Some(spawn(ctx, cfg));
        self.starting = true;
    }

    fn disconnect(&mut self) {
        self.link = None;
        self.connected = false;
        self.starting = false;
        self.event("Getrennt", false);
    }

    fn publish(&mut self) {
        let payload = if self.s.pub_hex {
            match codec::parse_hex(&self.s.pub_payload) {
                Ok(p) => p,
                Err(e) => {
                    self.event(e, true);
                    self.s.pub_repeat = false;
                    return;
                }
            }
        } else {
            self.s.pub_payload.clone().into_bytes()
        };
        let topic = self.s.pub_topic.trim().to_string();
        if topic.is_empty() || topic.contains('+') || topic.contains('#') {
            self.event("Bitte ein Thema ohne + und # zum Veröffentlichen angeben", true);
            self.s.pub_repeat = false;
            return;
        }
        if let Some(l) = &self.link {
            let _ = l.tx.send(MqCmd::Publish { topic, payload, qos: self.s.pub_qos, retain: self.s.pub_retain });
        }
    }

    fn add_message(&mut self, m: Msg) {
        if let Some((_, f)) = self.log_file.as_mut() {
            let dir = if m.sent { "▶" } else { "◀" };
            let _ = writeln!(f, "{} {dir} {} [QoS {}{}] {}", m.time, m.topic, m.qos, if m.retain { ", retain" } else { "" }, String::from_utf8_lossy(&m.payload));
        }
        if !m.sent {
            if self.topics.len() < MAX_TOPICS || self.topics.contains_key(&m.topic) {
                let t = self.topics.entry(m.topic.clone()).or_insert(TopicStat { count: 0, last: Vec::new(), time: String::new() });
                t.count += 1;
                t.last = m.payload.clone();
                t.time = m.time.clone();
            }
        }
        if self.paused {
            return;
        }
        self.messages.push_back(m);
        while self.messages.len() > MAX_MESSAGES {
            self.messages.pop_front();
            self.selected = self.selected.and_then(|s| s.checked_sub(1));
        }
    }

    // -------------------------------------------------------------- UI parts

    fn settings_ui(&mut self, ui: &mut Ui) {
        let active = self.link.is_some();
        section(ui, "Broker");
        ui.add_enabled_ui(!active, |ui| {
            egui::Grid::new("mq_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label("Adresse");
                ui.add(egui::TextEdit::singleline(&mut self.s.host).desired_width(160.0).hint_text("broker.example.com"));
                ui.end_row();
                ui.label("Port");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut self.s.port).range(1..=65535));
                    if ui.checkbox(&mut self.s.tls, "TLS").on_hover_text("Verschlüsselte Verbindung, meist Port 8883").changed() {
                        if self.s.tls && self.s.port == 1883 {
                            self.s.port = 8883;
                        } else if !self.s.tls && self.s.port == 8883 {
                            self.s.port = 1883;
                        }
                    }
                });
                ui.end_row();
                ui.label("Client-ID");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.s.client_id).desired_width(130.0));
                    if ui.small_button("🎲").on_hover_text("Neue zufällige Client-ID").clicked() {
                        self.s.client_id = random_id();
                    }
                });
                ui.end_row();
                ui.label("Benutzer");
                ui.add(egui::TextEdit::singleline(&mut self.s.user).desired_width(160.0).hint_text("optional"));
                ui.end_row();
                ui.label("Passwort");
                ui.add(egui::TextEdit::singleline(&mut self.s.password).password(true).desired_width(160.0).hint_text("optional"))
                    .on_hover_text("Wird im Projekt gespeichert. Projektdateien mit Passwort nur vertrauenswürdig weitergeben.");
                ui.end_row();
                ui.label("Keep-Alive");
                ui.add(egui::DragValue::new(&mut self.s.keep_alive_s).range(5..=3600).suffix(" s"));
                ui.end_row();
            });
            ui.checkbox(&mut self.s.clean_session, "Clean Session")
                .on_hover_text("Ohne Haken merkt sich der Broker Abos und verpasste QoS-1/2-Nachrichten");
            ui.checkbox(&mut self.s.auto_reconnect, "Automatisch neu verbinden");
            if self.s.tls {
                ui.checkbox(&mut self.s.insecure, "Zertifikat nicht prüfen")
                    .on_hover_text("Für Broker mit selbst signiertem Zertifikat. Nur in vertrauenswürdigen Netzen.");
                ui.horizontal(|ui| {
                    ui.label("CA-Datei");
                    ui.add(egui::TextEdit::singleline(&mut self.s.ca_file).desired_width(110.0).hint_text("optional (PEM)"))
                        .on_hover_text("Eigene Zertifizierungsstelle, z. B. Firmen-CA");
                    if ui.small_button("…").clicked() {
                        if let Some(p) = rfd::FileDialog::new().add_filter("Zertifikat", &["pem", "crt", "cer"]).pick_file() {
                            self.s.ca_file = p.display().to_string();
                        }
                    }
                });
            }
        });
        ui.add_space(6.0);
        if big_button(ui, active, "Verbinden", "Trennen").clicked() {
            if active {
                self.disconnect();
            } else if self.s.host.trim().is_empty() {
                self.event("Bitte eine Broker-Adresse eintragen", true);
            } else {
                self.connect(ui.ctx());
            }
        }
        status_line(ui, self.connected, self.starting || (active && !self.connected), "verbunden", "getrennt");
        ui.label(RichText::new(format!("Empfangen: {}   Gesendet: {}", self.received, self.sent)).small().weak());

        section(ui, "Ereignisse");
        for (t, m, err) in self.events.iter().rev().take(30) {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(t.get(..8).unwrap_or(t)).small().weak().monospace());
                ui.label(RichText::new(m).small().color(if *err { RED } else { ui.visuals().text_color() }));
            });
        }
    }

    fn subs_ui(&mut self, ui: &mut Ui) {
        section(ui, "Abonnements");
        let mut remove = None;
        let mut changed: Vec<(bool, String, u8)> = Vec::new();
        let mut confirm = None;
        for (i, s) in self.s.subs.iter_mut().enumerate() {
            ui.push_id(("sub", i), |ui| {
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut s.on, "").on_hover_text("Abo aktiv").changed() {
                        if s.on && is_broad(&s.topic) {
                            s.on = false;
                            confirm = Some((s.topic.clone(), s.qos, Some(i)));
                        } else {
                            changed.push((s.on, s.topic.clone(), s.qos));
                        }
                    }
                    ui.add(egui::Label::new(RichText::new(&s.topic).monospace()).truncate());
                    ui.label(RichText::new(format!("Q{}", s.qos)).small().weak());
                    if ui.small_button("✖").on_hover_text("Abo entfernen").clicked() {
                        remove = Some(i);
                    }
                });
            });
        }
        if let Some(i) = remove {
            let s = self.s.subs.remove(i);
            changed.push((false, s.topic, s.qos));
        }
        if confirm.is_some() {
            self.confirm_broad = confirm;
        }
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.new_sub)
                    .hint_text("z. B. sensor/+/temp")
                    .desired_width((ui.available_width() - 110.0).max(80.0))
                    .font(egui::TextStyle::Monospace),
            );
            egui::ComboBox::from_id_salt("mq_subqos").selected_text(format!("Q{}", self.new_sub_qos)).width(44.0).show_ui(ui, |ui| {
                for q in 0..=2u8 {
                    ui.selectable_value(&mut self.new_sub_qos, q, format!("QoS {q}"));
                }
            });
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.button("➕").on_hover_text("Abonnieren").clicked() || enter) && !self.new_sub.trim().is_empty() {
                let t = self.new_sub.trim().to_string();
                if is_broad(&t) {
                    self.confirm_broad = Some((t, self.new_sub_qos, None));
                } else {
                    self.s.subs.retain(|s| s.topic != t);
                    self.s.subs.push(Sub { on: true, topic: t.clone(), qos: self.new_sub_qos });
                    changed.push((true, t, self.new_sub_qos));
                }
                self.new_sub.clear();
            }
        });
        ui.label(RichText::new("+ = eine Ebene, # = alles darunter").small().weak());
        if self.s.subs.is_empty() {
            ui.label(RichText::new("Noch kein Abo – empfangen wird nur, was hier steht.").small().italics().weak());
        }

        // confirmation for "#" and similar
        if let Some((t, q, idx)) = self.confirm_broad.clone() {
            let mut decision = None;
            egui::Window::new("Alle Themen abonnieren?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ui.ctx(), |ui| {
                    ui.label(format!("„{t}“ abonniert jedes Thema auf diesem Broker."));
                    ui.label("Auf öffentlichen Brokern sind das oft tausende Nachrichten pro Sekunde.");
                    ui.label(RichText::new("Besser gezielt abonnieren, z. B. „haus/#“ oder „sensor/+/temp“.").weak());
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Abbrechen").clicked() {
                            decision = Some(false);
                        }
                        if ui.button(RichText::new("Trotzdem abonnieren").color(RED)).clicked() {
                            decision = Some(true);
                        }
                    });
                });
            match decision {
                Some(true) => {
                    match idx {
                        Some(i) if i < self.s.subs.len() => self.s.subs[i].on = true,
                        _ => {
                            self.s.subs.retain(|s| s.topic != t);
                            self.s.subs.push(Sub { on: true, topic: t.clone(), qos: q });
                        }
                    }
                    changed.push((true, t, q));
                    self.confirm_broad = None;
                }
                Some(false) => self.confirm_broad = None,
                None => {}
            }
        }
        if let Some(l) = &self.link {
            for (on, t, q) in changed {
                let _ = l.tx.send(if on { MqCmd::Subscribe(t, q) } else { MqCmd::Unsubscribe(t) });
            }
        }

        section(ui, &format!("Themen ({})", self.topics.len()));
        if self.topics.is_empty() {
            ui.label(RichText::new("Noch keine Nachrichten empfangen.").weak().italics());
        }
        let mut set_filter = None;
        egui::ScrollArea::vertical().id_salt("mq_topics").auto_shrink([false, false]).show(ui, |ui| {
            for (t, st) in &self.topics {
                let r = ui
                    .add(
                        egui::Button::selectable(self.s.filter == *t, RichText::new(t).monospace().color(ACCENT))
                            .truncate(),
                    )
                    .on_hover_text(format!("{} Nachrichten, zuletzt {}\nKlick: nur dieses Thema anzeigen", st.count, st.time));
                if r.clicked() {
                    set_filter = Some(t.clone());
                }
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    ui.label(RichText::new(format!("{}×", st.count)).small().weak());
                    ui.add(egui::Label::new(RichText::new(preview(&st.last, self.s.view)).small().monospace()).truncate());
                });
            }
        });
        if let Some(t) = set_filter {
            self.s.filter = if self.s.filter == t { String::new() } else { t };
        }
    }

    fn publish_ui(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Veröffentlichen").strong());
            ui.add(
                egui::TextEdit::singleline(&mut self.s.pub_topic)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("Thema")
                    .desired_width(260.0),
            );
            egui::ComboBox::from_id_salt("mq_pubqos").selected_text(format!("QoS {}", self.s.pub_qos)).width(64.0).show_ui(ui, |ui| {
                for q in 0..=2u8 {
                    ui.selectable_value(&mut self.s.pub_qos, q, format!("QoS {q}"));
                }
            });
            ui.checkbox(&mut self.s.pub_retain, "Retain").on_hover_text("Broker behält die Nachricht als letzten Wert");
            ui.checkbox(&mut self.s.pub_hex, "HEX").on_hover_text("Inhalt als Hex-Bytes, z. B. 01 02 FF");
            if !self.s.pub_hex && ui.small_button("JSON formatieren").clicked() {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&self.s.pub_payload) {
                    self.s.pub_payload = serde_json::to_string_pretty(&v).unwrap_or_default();
                }
            }
        });
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::multiline(&mut self.s.pub_payload)
                    .code_editor()
                    .desired_rows(3)
                    .desired_width((ui.available_width() - 190.0).max(200.0)),
            );
            ui.vertical(|ui| {
                if ui
                    .add_enabled(
                        self.connected,
                        egui::Button::new(RichText::new("▶ Senden").color(Color32::WHITE).strong()).fill(ACCENT),
                    )
                    .clicked()
                {
                    self.publish();
                }
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.s.pub_repeat, "alle").on_hover_text("Automatisch wiederholt senden");
                    ui.add(egui::DragValue::new(&mut self.s.pub_interval_ms).range(50..=3_600_000).suffix(" ms").speed(10.0));
                });
                if !self.connected {
                    ui.label(RichText::new("erst verbinden").small().weak().italics());
                }
            });
        });
    }

    fn messages_ui(&mut self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Nachrichten").strong());
            ui.selectable_value(&mut self.s.view, MsgView::Text, "Text");
            ui.selectable_value(&mut self.s.view, MsgView::Hex, "Hex");
            ui.selectable_value(&mut self.s.view, MsgView::Json, "JSON");
            ui.separator();
            ui.label("Filter");
            ui.add(egui::TextEdit::singleline(&mut self.s.filter).hint_text("Thema, + und # erlaubt").desired_width(170.0));
            if !self.s.filter.is_empty() && ui.small_button("✖").on_hover_text("Filter löschen").clicked() {
                self.s.filter.clear();
            }
            ui.checkbox(&mut self.s.show_sent, "Gesendete");
            ui.separator();
            let pause_label = if self.paused { "▶ Weiter" } else { "⏸ Pause" };
            ui.toggle_value(&mut self.paused, pause_label);
            if ui.button("🗑 Leeren").clicked() {
                self.messages.clear();
                self.topics.clear();
                self.selected = None;
            }
            if ui.button("💾 Speichern").clicked() {
                if let Some(p) = rfd::FileDialog::new().add_filter("Text", &["txt", "log"]).set_file_name("mqtt.txt").save_file() {
                    let text: String = self
                        .messages
                        .iter()
                        .map(|m| format!("{} {} {} [QoS {}{}] {}\n", m.time, if m.sent { "▶" } else { "◀" }, m.topic, m.qos, if m.retain { ", retain" } else { "" }, String::from_utf8_lossy(&m.payload)))
                        .collect();
                    if let Err(e) = std::fs::write(&p, text) {
                        self.event(format!("Speichern fehlgeschlagen: {e}"), true);
                    }
                }
            }
            let logging = self.log_file.is_some();
            if ui
                .add(egui::Button::new(if logging { "⏺ Mitschrift läuft" } else { "⏺ Mitschreiben" }).selected(logging))
                .on_hover_text("Alle Nachrichten laufend in eine Datei schreiben")
                .clicked()
            {
                if logging {
                    self.log_file = None;
                    self.event("Mitschrift beendet", false);
                } else if let Some(p) = rfd::FileDialog::new().set_file_name("mqtt.log").save_file() {
                    match std::fs::OpenOptions::new().create(true).append(true).open(&p) {
                        Ok(f) => self.log_file = Some((p.display().to_string(), f)),
                        Err(e) => self.event(format!("Datei nicht beschreibbar: {e}"), true),
                    }
                }
            }
        });

        if self.rate >= 100 {
            let mut stop = false;
            egui::Frame::new().fill(Color32::from_rgb(255, 236, 200)).corner_radius(4.0).inner_margin(6.0).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let brown = Color32::from_rgb(140, 70, 0);
                    ui.label(RichText::new(format!("⚠ Sehr viele Nachrichten: {} pro Sekunde.", self.rate)).color(brown).strong());
                    ui.label(RichText::new("Abos eingrenzen oder:").color(brown));
                    if ui.button("Alle Abos ausschalten").clicked() {
                        stop = true;
                    }
                });
            });
            if stop {
                for sub in &mut self.s.subs {
                    if sub.on {
                        sub.on = false;
                        if let Some(l) = &self.link {
                            let _ = l.tx.send(MqCmd::Unsubscribe(sub.topic.clone()));
                        }
                    }
                }
                self.event("Alle Abos ausgeschaltet", false);
            }
        }
        let filter = self.s.filter.trim().to_string();
        let show_sent = self.s.show_sent;
        let idx: Vec<usize> = self
            .messages
            .iter()
            .enumerate()
            .filter(|(_, m)| (show_sent || !m.sent) && (filter.is_empty() || topic_matches(&filter, &m.topic)))
            .map(|(i, _)| i)
            .collect();

        // detail of the selected message
        let mut take_over = None;
        if let Some(sel) = self.selected.and_then(|i| self.messages.get(i)) {
            let full = format_payload(&sel.payload, self.s.view, true);
            let topic = sel.topic.clone();
            let meta = format!(
                "{} · {} · QoS {}{} · {} Byte",
                sel.time,
                if sel.sent { "gesendet" } else { "empfangen" },
                sel.qos,
                if sel.retain { " · retain" } else { "" },
                sel.payload.len()
            );
            let raw = String::from_utf8_lossy(&sel.payload).into_owned();
            egui::Panel::bottom("mq_detail")
                .resizable(true)
                .default_size(170.0)
                .min_size(80.0)
                .frame(egui::Frame::new().inner_margin(egui::Margin { left: 0, right: 0, top: 6, bottom: 0 }))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&topic).monospace().strong().color(ACCENT));
                        ui.label(RichText::new(meta).small().weak());
                        if ui.small_button("📋 Kopieren").clicked() {
                            ui.ctx().copy_text(full.clone());
                        }
                        if ui.small_button("Zum Veröffentlichen übernehmen").clicked() {
                            take_over = Some((topic.clone(), raw.clone()));
                        }
                    });
                    egui::ScrollArea::vertical().id_salt("mq_detail_scroll").auto_shrink([false, false]).show(ui, |ui| {
                        let mut view = full.as_str();
                        ui.add(egui::TextEdit::multiline(&mut view).code_editor().desired_width(f32::INFINITY));
                    });
                });
        }
        if let Some((t, p)) = take_over {
            self.s.pub_topic = t;
            self.s.pub_payload = p;
            self.s.pub_hex = false;
        }

        let dark = ui.visuals().dark_mode;
        let mut clicked = None;
        egui::Frame::new()
            .fill(if dark { Color32::from_gray(18) } else { Color32::WHITE })
            .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
            .corner_radius(4.0)
            .inner_margin(6.0)
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                if idx.is_empty() {
                    ui.label(RichText::new(if self.messages.is_empty() { "Noch keine Nachrichten." } else { "Keine Nachricht passt zum Filter." }).weak().italics());
                    return;
                }
                let row_h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
                egui::ScrollArea::vertical().id_salt("mq_list").stick_to_bottom(true).auto_shrink([false, false]).show_rows(
                    ui,
                    row_h,
                    idx.len(),
                    |ui, rows| {
                        for r in rows {
                            let i = idx[r];
                            let m = &self.messages[i];
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(&m.time).small().weak().monospace());
                                ui.label(RichText::new(if m.sent { "▶" } else { "◀" }).color(if m.sent { ACCENT } else { GREEN }));
                                ui.label(RichText::new(&m.topic).monospace().strong().color(ACCENT));
                                if m.qos > 0 {
                                    ui.label(RichText::new(format!("Q{}", m.qos)).small().weak());
                                }
                                if m.retain {
                                    ui.label(RichText::new("R").small().weak()).on_hover_text("Retain");
                                }
                                let r = ui.add(
                                    egui::Button::selectable(self.selected == Some(i), RichText::new(preview(&m.payload, self.s.view)).monospace())
                                        .truncate(),
                                );
                                if r.clicked() {
                                    clicked = Some(i);
                                }
                            });
                        }
                    },
                );
            });
        if let Some(i) = clicked {
            self.selected = if self.selected == Some(i) { None } else { Some(i) };
        }
    }
}

impl Session for MqttSession {
    fn name(&self) -> &str {
        &self.s.name
    }
    fn set_name(&mut self, n: String) {
        self.s.name = n;
    }
    fn is_open(&self) -> bool {
        self.connected
    }
    fn is_busy(&self) -> bool {
        self.s.pub_repeat && self.connected
    }
    fn stop(&mut self) {
        self.link = None;
        self.connected = false;
        registry().lock().unwrap().retain(|e| e.id != self.id);
    }
    fn duplicate(&self) -> Self {
        let mut s = self.saved();
        s.client_id = random_id();
        Self::from_saved(s)
    }
    fn summary(&self) -> String {
        format!("{} ({}:{})", self.s.name, self.s.host, self.s.port)
    }

    fn pump(&mut self, ctx: &egui::Context) {
        if let Some(link) = &self.link {
            let events: Vec<MqEvent> = link.rx.try_iter().collect();
            let mut closed = false;
            for e in events {
                match e {
                    MqEvent::Connected => {
                        self.connected = true;
                        self.starting = false;
                        self.event(format!("Verbunden mit {}:{}", self.s.host.trim(), self.s.port), false);
                    }
                    MqEvent::Disconnected(m) => {
                        self.connected = false;
                        self.event(format!("Verbindung verloren: {m}"), true);
                    }
                    MqEvent::Closed => closed = true,
                    MqEvent::Info(m) => self.event(m, false),
                    MqEvent::Error(m) => self.event(m, true),
                    MqEvent::Message { topic, payload, qos, retain } => {
                        self.received += 1;
                        self.rate_count += 1;
                        self.add_message(Msg { time: now(), topic, payload, qos, retain, sent: false });
                    }
                    MqEvent::Sent { topic, payload, qos, retain } => {
                        self.sent += 1;
                        self.add_message(Msg { time: now(), topic, payload, qos, retain, sent: true });
                    }
                }
            }
            if closed {
                self.link = None;
                self.connected = false;
                self.starting = false;
            }
        }
        if self.rate_start.elapsed() >= Duration::from_secs(1) {
            self.rate = self.rate_count;
            self.rate_count = 0;
            self.rate_start = Instant::now();
        }
        // registry for scripts
        {
            let mut reg = registry().lock().unwrap();
            reg.retain(|e| e.id != self.id);
            if let Some(l) = &self.link {
                reg.push(RegEntry { id: self.id, name: self.s.name.clone(), connected: self.connected, tx: l.tx.clone() });
            }
        }
        if self.s.pub_repeat && self.connected {
            if self.last_pub.elapsed() >= Duration::from_millis(self.s.pub_interval_ms) {
                self.last_pub = Instant::now();
                self.publish();
            }
            ctx.request_repaint_after(Duration::from_millis(self.s.pub_interval_ms.min(100)));
        }
        if self.link.is_some() && !self.connected {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    fn ui(&mut self, ui: &mut Ui, _shared: &mut Shared) {
        egui::Panel::right("mq_cfg")
            .resizable(true)
            .default_size(280.0)
            .min_size(240.0)
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(10.0))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("mq_cfg_scroll").auto_shrink([false, false]).show(ui, |ui| self.settings_ui(ui));
            });
        egui::Panel::left("mq_subs")
            .resizable(true)
            .default_size(270.0)
            .min_size(200.0)
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(10.0))
            .show(ui, |ui| self.subs_ui(ui));
        egui::Panel::bottom("mq_pub")
            .resizable(false)
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(10, 8)))
            .show(ui, |ui| self.publish_ui(ui));
        egui::CentralPanel::default().frame(egui::Frame::central_panel(ui.style()).inner_margin(10.0)).show(ui, |ui| {
            self.messages_ui(ui);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;

    #[test]
    fn tls_configs_build() {
        assert!(tls::config(false, "").is_ok());
        assert!(tls::config(true, "").is_ok());
        assert!(tls::config(false, "gibt-es-nicht.pem").is_err());
    }

    #[test]
    fn broad_subscriptions_are_detected_and_old_default_is_off() {
        assert!(is_broad("#") && is_broad("+/#") && is_broad("+/+/#"));
        assert!(!is_broad("haus/#") && !is_broad("sensor/+/temp") && !is_broad("+"));
        assert!(MqttSession::new("x".into()).s.subs.is_empty());
        let old = MqttSaved { subs: vec![Sub { on: true, topic: "#".into(), qos: 0 }], ..Default::default() };
        assert!(!MqttSession::from_saved(old).s.subs[0].on);
    }

    #[test]
    fn topic_filters() {
        assert!(topic_matches("#", "a/b/c"));
        assert!(topic_matches("sensor/+/temp", "sensor/k1/temp"));
        assert!(!topic_matches("sensor/+/temp", "sensor/k1/hum"));
        assert!(topic_matches("haus/#", "haus/og/licht"));
        assert!(!topic_matches("haus/og", "haus/og/licht"));
        assert!(topic_matches("a/b", "a/b"));
    }

    fn read_packet(s: &mut std::net::TcpStream) -> (u8, Vec<u8>) {
        let mut h = [0u8; 1];
        s.read_exact(&mut h).unwrap();
        let mut len = 0usize;
        let mut mul = 1;
        loop {
            let mut b = [0u8; 1];
            s.read_exact(&mut b).unwrap();
            len += (b[0] & 0x7F) as usize * mul;
            mul *= 128;
            if b[0] & 0x80 == 0 {
                break;
            }
        }
        let mut body = vec![0u8; len];
        s.read_exact(&mut body).unwrap();
        (h[0], body)
    }

    /// A tiny fake broker: accepts the connection, confirms the subscription,
    /// sends one message and checks the client's publish.
    #[test]
    fn talks_to_a_broker() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let (seen_tx, seen_rx) = channel::<String>();
        thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let (t, _) = read_packet(&mut s);
            assert_eq!(t >> 4, 1, "CONNECT erwartet");
            s.write_all(&[0x20, 0x02, 0x00, 0x00]).unwrap(); // CONNACK
            loop {
                let (t, body) = read_packet(&mut s);
                match t >> 4 {
                    8 => {
                        // SUBSCRIBE -> SUBACK, then a message on the topic
                        s.write_all(&[0x90, 0x03, body[0], body[1], 0x00]).unwrap();
                        let topic = b"sensor/k1/temp";
                        let payload = b"21.5";
                        let mut p = vec![0x30, (2 + topic.len() + payload.len()) as u8, 0, topic.len() as u8];
                        p.extend_from_slice(topic);
                        p.extend_from_slice(payload);
                        s.write_all(&p).unwrap();
                    }
                    3 => {
                        let n = u16::from_be_bytes([body[0], body[1]]) as usize;
                        let topic = String::from_utf8_lossy(&body[2..2 + n]).to_string();
                        let payload = String::from_utf8_lossy(&body[2 + n..]).to_string();
                        seen_tx.send(format!("{topic}={payload}")).unwrap();
                    }
                    12 => s.write_all(&[0xD0, 0x00]).unwrap(), // PINGRESP
                    14 => break,                               // DISCONNECT
                    _ => {}
                }
            }
        });

        let ctx = egui::Context::default();
        let mut m = MqttSession::new("Test".into());
        m.s.host = "127.0.0.1".into();
        m.s.port = port;
        m.s.subs = vec![Sub { on: true, topic: "sensor/+/temp".into(), qos: 0 }];
        m.connect(&ctx);
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end && (m.received == 0 || !m.connected) {
            m.pump(&ctx);
            thread::sleep(Duration::from_millis(10));
        }
        assert!(m.connected, "nicht verbunden: {:?}", m.events);
        assert_eq!(m.received, 1);
        assert_eq!(m.topics["sensor/k1/temp"].last, b"21.5");

        // publish from a script through the registry
        script_publish("Test", "aus/skript", b"hallo".to_vec(), 0, false).unwrap();
        assert_eq!(seen_rx.recv_timeout(Duration::from_secs(5)).unwrap(), "aus/skript=hallo");
        m.stop();
    }
}
