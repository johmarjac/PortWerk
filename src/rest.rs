//! REST / HTTP client tab: saved requests, parameters, headers, body,
//! authentication, variables, history and a formatted response view.

use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText, Ui};
use serde::{Deserialize, Serialize};

use crate::app::section;

pub const METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

fn method_color(m: &str) -> Color32 {
    match m {
        "GET" => Color32::from_rgb(30, 150, 70),
        "POST" => Color32::from_rgb(210, 130, 0),
        "PUT" => Color32::from_rgb(30, 110, 200),
        "PATCH" => Color32::from_rgb(140, 80, 200),
        "DELETE" => Color32::from_rgb(200, 50, 50),
        _ => Color32::from_rgb(110, 110, 110),
    }
}

// ------------------------------------------------------------------ data

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Kv {
    pub on: bool,
    pub key: String,
    pub value: String,
}

impl Default for Kv {
    fn default() -> Self {
        Self { on: true, key: String::new(), value: String::new() }
    }
}

impl Kv {
    fn new(k: &str, v: &str) -> Self {
        Self { on: true, key: k.into(), value: v.into() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum BodyKind {
    None,
    Json,
    Text,
    Form,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum AuthKind {
    None,
    Bearer,
    Basic,
    ApiKey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RestRequest {
    pub name: String,
    pub method: String,
    pub url: String,
    pub params: Vec<Kv>,
    pub headers: Vec<Kv>,
    pub body_kind: BodyKind,
    pub body: String,
    pub form: Vec<Kv>,
    pub auth: AuthKind,
    pub token: String,
    pub user: String,
    pub password: String,
    pub api_key_name: String,
    pub api_key_value: String,
}

impl Default for RestRequest {
    fn default() -> Self {
        Self {
            name: "Neue Anfrage".into(),
            method: "GET".into(),
            url: String::new(),
            params: Vec::new(),
            headers: Vec::new(),
            body_kind: BodyKind::None,
            body: String::new(),
            form: Vec::new(),
            auth: AuthKind::None,
            token: String::new(),
            user: String::new(),
            password: String::new(),
            api_key_name: "X-API-Key".into(),
            api_key_value: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RestSettings {
    pub requests: Vec<RestRequest>,
    pub selected: usize,
    pub vars: Vec<Kv>,
    pub timeout_s: u64,
    pub follow_redirects: bool,
    pub insecure: bool,
}

impl Default for RestSettings {
    fn default() -> Self {
        Self {
            requests: vec![
                RestRequest {
                    name: "Beispiel GET".into(),
                    url: "{{baseUrl}}/get".into(),
                    params: vec![Kv::new("suche", "hallo welt")],
                    headers: vec![Kv::new("Accept", "application/json")],
                    ..Default::default()
                },
                RestRequest {
                    name: "Beispiel POST (JSON)".into(),
                    method: "POST".into(),
                    url: "{{baseUrl}}/post".into(),
                    body_kind: BodyKind::Json,
                    body: "{\n  \"geraet\": \"Sensor 1\",\n  \"wert\": 42\n}".into(),
                    ..Default::default()
                },
            ],
            selected: 0,
            vars: vec![Kv::new("baseUrl", "https://httpbin.org")],
            timeout_s: 30,
            follow_redirects: true,
            insecure: false,
        }
    }
}

pub struct RestResponse {
    status: u16,
    reason: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    ms: u128,
}

struct HistoryEntry {
    time: String,
    method: String,
    url: String,
    result: String,
    ok: bool,
}

struct Prepared {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
}

#[derive(PartialEq, Clone, Copy)]
enum ReqTab {
    Params,
    Headers,
    Body,
    Auth,
}

#[derive(PartialEq, Clone, Copy)]
enum RespTab {
    Body,
    Headers,
}

pub struct RestClient {
    pub s: RestSettings,
    pending: Option<(Receiver<Result<RestResponse, String>>, Instant, String, String)>,
    response: Option<Result<RestResponse, String>>,
    resp_view: String,
    pretty: bool,
    req_tab: ReqTab,
    resp_tab: RespTab,
    history: Vec<HistoryEntry>,
    note: Option<String>,
}

// ------------------------------------------------------------------ helpers

fn subst(s: &str, vars: &[Kv]) -> String {
    let mut out = s.to_string();
    for v in vars.iter().filter(|v| v.on && !v.key.trim().is_empty()) {
        out = out.replace(&format!("{{{{{}}}}}", v.key.trim()), &v.value);
    }
    out
}

fn url_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

fn human_size(n: usize) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / 1024.0 / 1024.0)
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn execute(p: Prepared, timeout_s: u64, follow: bool, insecure: bool) -> Result<RestResponse, String> {
    let tls = ureq::tls::TlsConfig::builder().disable_verification(insecure).build();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(timeout_s.max(1))))
        .max_redirects(if follow { 10 } else { 0 })
        .allow_non_standard_methods(true)
        .tls_config(tls)
        .build()
        .into();
    let mut b = ureq::http::Request::builder().method(p.method.as_str()).uri(p.url.as_str());
    for (k, v) in &p.headers {
        b = b.header(k.as_str(), v.as_str());
    }
    let start = Instant::now();
    let res = match p.body {
        Some(body) => agent.run(b.body(body).map_err(|e| format!("Ungültige Anfrage: {e}"))?),
        None => agent.run(b.body(()).map_err(|e| format!("Ungültige Anfrage: {e}"))?),
    };
    let mut resp = res.map_err(|e| e.to_string())?;
    let status = resp.status();
    let headers = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
        .collect();
    let body = resp
        .body_mut()
        .with_config()
        .limit(100 * 1024 * 1024)
        .read_to_vec()
        .map_err(|e| format!("Antwort konnte nicht gelesen werden: {e}"))?;
    Ok(RestResponse {
        status: status.as_u16(),
        reason: status.canonical_reason().unwrap_or("").to_string(),
        headers,
        body,
        ms: start.elapsed().as_millis(),
    })
}

/// Simple blocking HTTP call for scripts: returns (status, body).
pub fn simple_request(method: &str, url: &str, body: Option<Vec<u8>>, content_type: Option<&str>) -> Result<(u16, Vec<u8>), String> {
    let mut headers = Vec::new();
    if let Some(ct) = content_type {
        headers.push(("Content-Type".to_string(), ct.to_string()));
    }
    let r = execute(Prepared { method: method.to_uppercase(), url: url.to_string(), headers, body }, 30, true, false)?;
    Ok((r.status, r.body))
}

fn kv_table(ui: &mut Ui, id: &str, rows: &mut Vec<Kv>, key_hint: &str, val_hint: &str) {
    let mut remove = None;
    let val_w = (ui.available_width() - 290.0).max(160.0);
    for (i, r) in rows.iter_mut().enumerate() {
        ui.push_id((id, i), |ui| {
            ui.horizontal(|ui| {
                ui.checkbox(&mut r.on, "").on_hover_text("Aktiv");
                ui.add(egui::TextEdit::singleline(&mut r.key).hint_text(key_hint).desired_width(200.0));
                ui.add(egui::TextEdit::singleline(&mut r.value).hint_text(val_hint).desired_width(val_w));
                if ui.small_button("✖").on_hover_text("Entfernen").clicked() {
                    remove = Some(i);
                }
            });
        });
    }
    if let Some(i) = remove {
        rows.remove(i);
    }
    if ui.button("➕ Hinzufügen").clicked() {
        rows.push(Kv::default());
    }
}

// ------------------------------------------------------------------ client

impl RestClient {
    pub fn new(mut s: RestSettings) -> Self {
        if s.requests.is_empty() {
            s.requests.push(RestRequest::default());
        }
        Self {
            s,
            pending: None,
            response: None,
            resp_view: String::new(),
            pretty: true,
            req_tab: ReqTab::Params,
            resp_tab: RespTab::Body,
            history: Vec::new(),
            note: None,
        }
    }

    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    fn current(&mut self) -> &mut RestRequest {
        if self.s.requests.is_empty() {
            self.s.requests.push(RestRequest::default());
        }
        let i = self.s.selected.min(self.s.requests.len() - 1);
        self.s.selected = i;
        &mut self.s.requests[i]
    }

    fn prepare(&self, r: &RestRequest) -> Result<Prepared, String> {
        let vars = &self.s.vars;
        let mut url = subst(r.url.trim(), vars);
        if url.is_empty() {
            return Err("Bitte eine URL eingeben".into());
        }
        if !url.contains("://") {
            url = format!("http://{url}");
        }
        let query: Vec<String> = r
            .params
            .iter()
            .filter(|p| p.on && !p.key.trim().is_empty())
            .map(|p| format!("{}={}", url_encode(&subst(p.key.trim(), vars)), url_encode(&subst(&p.value, vars))))
            .collect();
        if !query.is_empty() {
            url.push(if url.contains('?') { '&' } else { '?' });
            url.push_str(&query.join("&"));
        }
        let mut headers: Vec<(String, String)> = r
            .headers
            .iter()
            .filter(|h| h.on && !h.key.trim().is_empty())
            .map(|h| (subst(h.key.trim(), vars), subst(&h.value, vars)))
            .collect();
        let has = |hs: &Vec<(String, String)>, n: &str| hs.iter().any(|(k, _)| k.eq_ignore_ascii_case(n));
        match r.auth {
            AuthKind::None => {}
            AuthKind::Bearer => headers.push(("Authorization".into(), format!("Bearer {}", subst(&r.token, vars)))),
            AuthKind::Basic => {
                let raw = format!("{}:{}", subst(&r.user, vars), subst(&r.password, vars));
                headers.push(("Authorization".into(), format!("Basic {}", base64(raw.as_bytes()))));
            }
            AuthKind::ApiKey => headers.push((subst(&r.api_key_name, vars), subst(&r.api_key_value, vars))),
        }
        let body = match r.body_kind {
            BodyKind::None => None,
            BodyKind::Json => {
                if !has(&headers, "content-type") {
                    headers.push(("Content-Type".into(), "application/json".into()));
                }
                Some(subst(&r.body, vars).into_bytes())
            }
            BodyKind::Text => {
                if !has(&headers, "content-type") {
                    headers.push(("Content-Type".into(), "text/plain; charset=utf-8".into()));
                }
                Some(subst(&r.body, vars).into_bytes())
            }
            BodyKind::Form => {
                if !has(&headers, "content-type") {
                    headers.push(("Content-Type".into(), "application/x-www-form-urlencoded".into()));
                }
                let f: Vec<String> = r
                    .form
                    .iter()
                    .filter(|p| p.on && !p.key.trim().is_empty())
                    .map(|p| format!("{}={}", url_encode(&subst(p.key.trim(), vars)), url_encode(&subst(&p.value, vars))))
                    .collect();
                Some(f.join("&").into_bytes())
            }
        };
        let body = if matches!(r.method.as_str(), "GET" | "HEAD") { None } else { body };
        Ok(Prepared { method: r.method.clone(), url, headers, body })
    }

    fn send(&mut self, ctx: &egui::Context) {
        let r = self.current().clone();
        match self.prepare(&r) {
            Err(e) => self.note = Some(e),
            Ok(p) => {
                self.note = None;
                let (tx, rx) = channel();
                let (t, f, ins) = (self.s.timeout_s, self.s.follow_redirects, self.s.insecure);
                let label = (p.method.clone(), p.url.clone());
                let ctx = ctx.clone();
                thread::spawn(move || {
                    let _ = tx.send(execute(p, t, f, ins));
                    ctx.request_repaint();
                });
                self.pending = Some((rx, Instant::now(), label.0, label.1));
            }
        }
    }

    fn curl(&mut self) -> Result<String, String> {
        let r = self.current().clone();
        let p = self.prepare(&r)?;
        let mut s = format!("curl -X {} {}", p.method, shell_quote(&p.url));
        for (k, v) in &p.headers {
            s.push_str(&format!(" \\\n  -H {}", shell_quote(&format!("{k}: {v}"))));
        }
        if let Some(b) = &p.body {
            s.push_str(&format!(" \\\n  --data-raw {}", shell_quote(&String::from_utf8_lossy(b))));
        }
        if self.s.insecure {
            s.push_str(" -k");
        }
        Ok(s)
    }

    fn rebuild_view(&mut self) {
        self.resp_view = match &self.response {
            Some(Ok(r)) => {
                let text = String::from_utf8_lossy(&r.body);
                let pretty = if self.pretty {
                    serde_json::from_slice::<serde_json::Value>(&r.body)
                        .ok()
                        .and_then(|v| serde_json::to_string_pretty(&v).ok())
                } else {
                    None
                };
                let mut s = pretty.unwrap_or_else(|| text.into_owned());
                if s.len() > 2_000_000 {
                    let mut cut = 2_000_000;
                    while !s.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    s.truncate(cut);
                    s.push_str("\n… (gekürzt, vollständig über „Speichern“)");
                }
                s
            }
            Some(Err(e)) => e.clone(),
            None => String::new(),
        };
    }

    fn poll(&mut self) {
        let Some((rx, started, method, url)) = &self.pending else { return };
        if let Ok(res) = rx.try_recv() {
            let time = chrono::Local::now().format("%H:%M:%S").to_string();
            let (result, ok) = match &res {
                Ok(r) => (format!("{} · {} ms", r.status, r.ms), r.status < 400),
                Err(_) => ("Fehler".to_string(), false),
            };
            self.history.insert(0, HistoryEntry { time, method: method.clone(), url: url.clone(), result, ok });
            self.history.truncate(100);
            self.response = Some(res);
            self.pending = None;
            self.rebuild_view();
        } else if started.elapsed() > Duration::from_secs(self.s.timeout_s + 5) {
            self.pending = None;
            self.response = Some(Err("Zeitüberschreitung".into()));
            self.rebuild_view();
        }
    }

    // -------------------------------------------------------------- UI

    pub fn ui(&mut self, ui: &mut Ui) {
        self.poll();
        let ctx = ui.ctx().clone();
        if self.pending.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        egui::Panel::left("rest_list")
            .resizable(true)
            .default_size(250.0)
            .min_size(200.0)
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(10.0))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.side_ui(ui));
            });

        egui::CentralPanel::default().frame(egui::Frame::central_panel(ui.style()).inner_margin(10.0)).show(ui, |ui| {
            self.url_bar(ui, &ctx);
            ui.add_space(4.0);
            egui::Panel::top("rest_req")
                .resizable(true)
                .default_size(250.0)
                .min_size(120.0)
                .frame(egui::Frame::new().inner_margin(egui::Margin { left: 0, right: 0, top: 0, bottom: 8 }))
                .show(ui, |ui| self.request_ui(ui));
            egui::CentralPanel::default().frame(egui::Frame::new().inner_margin(egui::Margin { left: 0, right: 0, top: 8, bottom: 0 })).show(ui, |ui| {
                self.response_ui(ui)
            });
        });
    }

    fn side_ui(&mut self, ui: &mut Ui) {
        ui.set_max_width(ui.available_width());
        section(ui, "Anfragen");
        ui.horizontal(|ui| {
            if ui.button("➕ Neu").clicked() {
                self.s.requests.push(RestRequest::default());
                self.s.selected = self.s.requests.len() - 1;
            }
            if ui.button("🗐").on_hover_text("Duplizieren").clicked() {
                let mut c = self.current().clone();
                c.name.push_str(" (Kopie)");
                self.s.requests.insert(self.s.selected + 1, c);
                self.s.selected += 1;
            }
            if ui.add_enabled(self.s.requests.len() > 1, egui::Button::new("🗑")).on_hover_text("Löschen").clicked() {
                self.s.requests.remove(self.s.selected);
                self.s.selected = self.s.selected.saturating_sub(1);
            }
        });
        ui.add_space(4.0);
        let mut select = None;
        for (i, r) in self.s.requests.iter().enumerate() {
            let selected = i == self.s.selected;
            let resp = ui
                .horizontal(|ui| {
                    ui.add_sized(
                        [52.0, 18.0],
                        egui::Label::new(RichText::new(&r.method).monospace().small().strong().color(method_color(&r.method))),
                    );
                    ui.add(egui::Button::selectable(selected, &r.name).truncate())
                })
                .inner;
            if resp.clicked() {
                select = Some(i);
            }
        }
        if let Some(i) = select {
            self.s.selected = i;
        }

        ui.add_space(8.0);
        egui::CollapsingHeader::new(RichText::new("Variablen").strong()).default_open(true).show(ui, |ui| {
            ui.label(RichText::new("In URL, Parametern, Headern und Body als {{name}} nutzbar.").small().weak());
            let mut remove = None;
            for (i, v) in self.s.vars.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.checkbox(&mut v.on, "");
                    ui.add(egui::TextEdit::singleline(&mut v.key).hint_text("name").desired_width(70.0));
                    ui.add(egui::TextEdit::singleline(&mut v.value).hint_text("Wert").desired_width(118.0));
                    if ui.small_button("✖").clicked() {
                        remove = Some(i);
                    }
                });
            }
            if let Some(i) = remove {
                self.s.vars.remove(i);
            }
            if ui.small_button("➕ Variable").clicked() {
                self.s.vars.push(Kv::default());
            }
        });

        egui::CollapsingHeader::new(RichText::new("Einstellungen").strong()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Zeitlimit");
                ui.add(egui::DragValue::new(&mut self.s.timeout_s).range(1..=600).suffix(" s"));
            });
            ui.checkbox(&mut self.s.follow_redirects, "Weiterleitungen folgen");
            ui.checkbox(&mut self.s.insecure, "Zertifikate nicht prüfen")
                .on_hover_text("Für Geräte mit selbst signierten Zertifikaten. Nur in vertrauenswürdigen Netzen verwenden.");
        });

        egui::CollapsingHeader::new(RichText::new(format!("Verlauf ({})", self.history.len())).strong()).show(ui, |ui| {
            if self.history.is_empty() {
                ui.label(RichText::new("Noch nichts gesendet.").weak().italics());
            }
            let mut load = None;
            for (i, h) in self.history.iter().enumerate() {
                let col = if h.ok { Color32::from_rgb(30, 150, 70) } else { Color32::from_rgb(200, 60, 50) };
                let r = ui
                    .add(
                        egui::Button::new(
                            RichText::new(format!("{} {} {}\n{}", h.time, h.method, h.result, h.url)).small(),
                        )
                        .frame(false)
                        .wrap(),
                    )
                    .on_hover_text("Klick: Methode und URL in eine neue Anfrage übernehmen");
                ui.painter().vline(r.rect.left() - 4.0, r.rect.y_range(), egui::Stroke::new(2.0, col));
                if r.clicked() {
                    load = Some(i);
                }
            }
            if let Some(i) = load {
                let h = &self.history[i];
                self.s.requests.push(RestRequest {
                    name: format!("{} {}", h.method, h.url.split('?').next().unwrap_or("")),
                    method: h.method.clone(),
                    url: h.url.clone(),
                    ..Default::default()
                });
                self.s.selected = self.s.requests.len() - 1;
            }
        });
    }

    fn url_bar(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let pending = self.pending.is_some();
        let mut send = false;
        let mut copy_curl = false;
        {
            let r = self.current();
            ui.horizontal(|ui| {
                ui.label(RichText::new("Name").weak());
                ui.add(egui::TextEdit::singleline(&mut r.name).desired_width(260.0));
            });
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("rest_method")
                    .selected_text(RichText::new(&r.method).strong().color(method_color(&r.method)))
                    .width(90.0)
                    .show_ui(ui, |ui| {
                        for m in METHODS {
                            ui.selectable_value(&mut r.method, m.to_string(), RichText::new(m).color(method_color(m)));
                        }
                    });
                let w = (ui.available_width() - 200.0).max(200.0);
                let te = ui.add(
                    egui::TextEdit::singleline(&mut r.url)
                        .font(egui::TextStyle::Monospace)
                        .hint_text("https://gerät.local/api/status  oder  {{baseUrl}}/pfad")
                        .desired_width(w),
                );
                if te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    send = true;
                }
                let (label, fill) = if pending {
                    ("⏳ Läuft …", Color32::from_rgb(220, 150, 0))
                } else {
                    ("▶ Senden", Color32::from_rgb(0, 130, 160))
                };
                if ui
                    .add_enabled(!pending, egui::Button::new(RichText::new(label).color(Color32::WHITE).strong()).fill(fill))
                    .on_hover_text("Anfrage senden (Enter im URL-Feld oder Strg+Enter)")
                    .clicked()
                {
                    send = true;
                }
                if ui.button("cURL").on_hover_text("Anfrage als cURL-Befehl kopieren").clicked() {
                    copy_curl = true;
                }
            });
        }
        if ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter)) {
            send = true;
        }
        if send && !pending {
            self.send(ctx);
        }
        if copy_curl {
            match self.curl() {
                Ok(c) => {
                    ctx.copy_text(c);
                    self.note = Some("cURL-Befehl in die Zwischenablage kopiert".into());
                }
                Err(e) => self.note = Some(e),
            }
        }
        if let Some(n) = &self.note {
            ui.label(RichText::new(n).color(Color32::from_rgb(200, 110, 0)).small());
        }
    }

    fn request_ui(&mut self, ui: &mut Ui) {
        let tab = &mut self.req_tab;
        let (np, nh) = {
            let r = &self.s.requests[self.s.selected.min(self.s.requests.len() - 1)];
            (r.params.iter().filter(|p| p.on).count(), r.headers.iter().filter(|p| p.on).count())
        };
        ui.horizontal(|ui| {
            ui.selectable_value(tab, ReqTab::Params, format!("Parameter ({np})"));
            ui.selectable_value(tab, ReqTab::Headers, format!("Header ({nh})"));
            ui.selectable_value(tab, ReqTab::Body, "Body");
            ui.selectable_value(tab, ReqTab::Auth, "Anmeldung");
        });
        ui.separator();
        let tab = self.req_tab;
        let r = self.current();
        egui::ScrollArea::vertical().id_salt("rest_req_scroll").auto_shrink([false, false]).show(ui, |ui| match tab {
            ReqTab::Params => {
                ui.label(RichText::new("Werden als ?name=wert an die URL gehängt (automatisch kodiert).").small().weak());
                kv_table(ui, "rest_params", &mut r.params, "Name", "Wert");
            }
            ReqTab::Headers => {
                kv_table(ui, "rest_headers", &mut r.headers, "Header, z. B. Accept", "Wert");
            }
            ReqTab::Body => {
                ui.horizontal(|ui| {
                    for (k, l) in [(BodyKind::None, "Kein"), (BodyKind::Json, "JSON"), (BodyKind::Text, "Text"), (BodyKind::Form, "Formular")] {
                        ui.radio_value(&mut r.body_kind, k, l);
                    }
                    if r.body_kind == BodyKind::Json {
                        ui.separator();
                        if ui.button("Formatieren").on_hover_text("JSON einrücken").clicked() {
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&r.body) {
                                r.body = serde_json::to_string_pretty(&v).unwrap_or_default();
                            }
                        }
                        if !r.body.trim().is_empty() && !r.body.contains("{{") {
                            match serde_json::from_str::<serde_json::Value>(&r.body) {
                                Ok(_) => ui.label(RichText::new("✔ gültiges JSON").small().color(Color32::from_rgb(30, 150, 70))),
                                Err(e) => ui.label(RichText::new(format!("⚠ {e}")).small().color(Color32::from_rgb(200, 110, 0))),
                            };
                        }
                    }
                });
                if matches!(r.method.as_str(), "GET" | "HEAD") && r.body_kind != BodyKind::None {
                    ui.label(RichText::new("Hinweis: Bei GET und HEAD wird kein Body gesendet.").small().weak());
                }
                match r.body_kind {
                    BodyKind::None => {
                        ui.label(RichText::new("Diese Anfrage hat keinen Body.").weak().italics());
                    }
                    BodyKind::Json | BodyKind::Text => {
                        ui.add(
                            egui::TextEdit::multiline(&mut r.body)
                                .code_editor()
                                .desired_rows(8)
                                .desired_width(f32::INFINITY),
                        );
                    }
                    BodyKind::Form => kv_table(ui, "rest_form", &mut r.form, "Feld", "Wert"),
                }
            }
            ReqTab::Auth => {
                ui.horizontal(|ui| {
                    for (k, l) in [(AuthKind::None, "Keine"), (AuthKind::Bearer, "Bearer-Token"), (AuthKind::Basic, "Benutzer + Passwort"), (AuthKind::ApiKey, "API-Schlüssel")] {
                        ui.radio_value(&mut r.auth, k, l);
                    }
                });
                ui.add_space(4.0);
                let field = |ui: &mut Ui, label: &str, value: &mut String, secret: bool, w: f32| {
                    ui.horizontal(|ui| {
                        ui.add_sized([100.0, 20.0], egui::Label::new(label));
                        ui.add(egui::TextEdit::singleline(value).password(secret).desired_width(w));
                    });
                };
                match r.auth {
                    AuthKind::None => {
                        ui.label(RichText::new("Keine Anmeldung.").weak().italics());
                    }
                    AuthKind::Bearer => field(ui, "Token", &mut r.token, true, 380.0),
                    AuthKind::Basic => {
                        field(ui, "Benutzer", &mut r.user, false, 240.0);
                        field(ui, "Passwort", &mut r.password, true, 240.0);
                    }
                    AuthKind::ApiKey => {
                        field(ui, "Header-Name", &mut r.api_key_name, false, 240.0);
                        field(ui, "Schlüssel", &mut r.api_key_value, true, 380.0);
                    }
                }
                ui.label(RichText::new("Tipp: Geheimnisse als Variable ablegen und hier {{name}} eintragen.").small().weak());
            }
        });
    }

    fn response_ui(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Antwort").strong().size(14.0));
            if let Some((_, started, _, _)) = &self.pending {
                ui.spinner();
                ui.label(format!("{:.1} s", started.elapsed().as_secs_f32()));
            }
            match &self.response {
                Some(Ok(r)) => {
                    let col = match r.status {
                        200..=299 => Color32::from_rgb(30, 150, 70),
                        300..=399 => Color32::from_rgb(30, 110, 200),
                        400..=499 => Color32::from_rgb(210, 130, 0),
                        _ => Color32::from_rgb(200, 50, 50),
                    };
                    egui::Frame::new().fill(col).corner_radius(4.0).inner_margin(egui::Margin::symmetric(6, 1)).show(ui, |ui| {
                        ui.label(RichText::new(format!("{} {}", r.status, r.reason)).color(Color32::WHITE).strong());
                    });
                    ui.label(RichText::new(format!("{} ms", r.ms)).weak());
                    ui.label(RichText::new(human_size(r.body.len())).weak());
                }
                Some(Err(_)) => {
                    ui.label(RichText::new("Fehler").color(Color32::from_rgb(200, 50, 50)).strong());
                }
                None => {}
            }
        });
        let Some(res) = &self.response else {
            ui.label(RichText::new("Noch keine Antwort. URL eingeben und „Senden“ klicken.").weak().italics());
            return;
        };
        let nh = res.as_ref().map(|r| r.headers.len()).unwrap_or(0);
        let mut rebuild = false;
        let mut save = false;
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.resp_tab, RespTab::Body, "Body");
            ui.selectable_value(&mut self.resp_tab, RespTab::Headers, format!("Header ({nh})"));
            ui.separator();
            if ui.checkbox(&mut self.pretty, "JSON formatieren").changed() {
                rebuild = true;
            }
            if ui.button("📋 Kopieren").clicked() {
                ui.ctx().copy_text(self.resp_view.clone());
            }
            if ui.button("💾 Speichern").clicked() {
                save = true;
            }
        });
        if rebuild {
            self.rebuild_view();
        }
        if save {
            if let Some(Ok(r)) = &self.response {
                if let Some(p) = rfd::FileDialog::new().set_file_name("antwort.txt").save_file() {
                    if let Err(e) = std::fs::write(&p, &r.body) {
                        self.note = Some(format!("Speichern fehlgeschlagen: {e}"));
                    }
                }
            }
        }
        let dark = ui.visuals().dark_mode;
        egui::Frame::new()
            .fill(if dark { Color32::from_gray(18) } else { Color32::WHITE })
            .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
            .corner_radius(4.0)
            .inner_margin(6.0)
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                egui::ScrollArea::both().id_salt("rest_resp_scroll").auto_shrink([false, false]).show(ui, |ui| {
                    match (self.resp_tab, &self.response) {
                        (RespTab::Headers, Some(Ok(r))) => {
                            egui::Grid::new("rest_resp_headers").num_columns(2).striped(true).spacing([12.0, 4.0]).show(ui, |ui| {
                                for (k, v) in &r.headers {
                                    ui.label(RichText::new(k).monospace().strong());
                                    ui.label(RichText::new(v).monospace());
                                    ui.end_row();
                                }
                            });
                        }
                        (_, Some(Err(e))) => {
                            ui.label(RichText::new(e).color(Color32::from_rgb(200, 50, 50)).monospace());
                        }
                        _ => {
                            let mut view = self.resp_view.as_str();
                            ui.add(
                                egui::TextEdit::multiline(&mut view)
                                    .code_editor()
                                    .desired_width(f32::INFINITY),
                            );
                        }
                    }
                });
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variables_params_and_auth() {
        let mut s = RestSettings::default();
        s.vars = vec![Kv::new("host", "example.org"), Kv::new("tok", "abc")];
        let c = RestClient::new(s);
        let r = RestRequest {
            method: "POST".into(),
            url: "{{host}}/api".into(),
            params: vec![Kv::new("q", "a b&c"), Kv { on: false, ..Kv::new("x", "1") }],
            auth: AuthKind::Bearer,
            token: "{{tok}}".into(),
            body_kind: BodyKind::Json,
            body: "{\"a\":1}".into(),
            ..Default::default()
        };
        let p = c.prepare(&r).unwrap();
        assert_eq!(p.url, "http://example.org/api?q=a%20b%26c");
        assert!(p.headers.contains(&("Authorization".into(), "Bearer abc".into())));
        assert!(p.headers.contains(&("Content-Type".into(), "application/json".into())));
        assert_eq!(p.body.unwrap(), b"{\"a\":1}");
    }

    #[test]
    fn base64_matches_reference() {
        assert_eq!(base64(b"user:pass"), "dXNlcjpwYXNz");
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(base64(b"ab"), "YWI=");
    }

    #[test]
    fn real_http_roundtrip_on_localhost() {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            // read the whole request (headers + 2-byte body) before answering
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            while !String::from_utf8_lossy(&req).contains("\r\n\r\n{}") {
                let n = s.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                req.extend_from_slice(&buf[..n]);
            }
            let req = String::from_utf8_lossy(&req).to_string();
            let body = if req.starts_with("POST /x?a=1") { "{\"ok\":true}" } else { "falsch" };
            write!(s, "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            s.flush().unwrap();
            let _ = s.shutdown(std::net::Shutdown::Write);
            let _ = s.read(&mut buf);
        });
        let p = Prepared {
            method: "POST".into(),
            url: format!("http://127.0.0.1:{port}/x?a=1"),
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: Some(b"{}".to_vec()),
        };
        let r = execute(p, 5, true, false).unwrap();
        assert_eq!(r.status, 201);
        assert_eq!(r.body, b"{\"ok\":true}");
    }
}
