//! Script runner (Lua 5.4) for protocol work that goes beyond fixed send
//! lines. A script runs on its own thread; it sends through the tab's
//! connection and reads what that connection receives.
//!
//! In Lua, strings are byte strings, so binary data is simply a string:
//! `string.char(0x01, 0x03)`, `from_hex("01 03")`, `#data`, `data:byte(1)`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use mlua::{HookTriggers, Lua, MultiValue, VmState};

use crate::codec;

pub enum ScriptMsg {
    Send(Vec<u8>),
    Log(String),
    Error(String),
    Done,
}

pub struct ScriptRun {
    stop: Arc<AtomicBool>,
    feed: Sender<Vec<u8>>,
    out: Receiver<ScriptMsg>,
}

impl ScriptRun {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
    pub fn feed(&self, data: &[u8]) {
        let _ = self.feed.send(data.to_vec());
    }
    pub fn poll(&self) -> Vec<ScriptMsg> {
        self.out.try_iter().collect()
    }
}

impl Drop for ScriptRun {
    fn drop(&mut self) {
        self.stop();
    }
}

struct RxState {
    rx: Receiver<Vec<u8>>,
    buf: Vec<u8>,
}

impl RxState {
    fn pull(&mut self) {
        for d in self.rx.try_iter() {
            self.buf.extend_from_slice(&d);
        }
    }
}

const STOPPED: &str = "Skript gestoppt";

fn stopped() -> mlua::Error {
    mlua::Error::runtime(STOPPED)
}

/// Sleep in slices, aborting early when the stop flag goes up.
fn nap(stop: &AtomicBool, ms: i64) -> mlua::Result<()> {
    let end = Instant::now() + Duration::from_millis(ms.max(0) as u64);
    while Instant::now() < end {
        if stop.load(Ordering::Relaxed) {
            return Err(stopped());
        }
        thread::sleep(Duration::from_millis(5).min(end.saturating_duration_since(Instant::now())));
    }
    Ok(())
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

fn values_to_text(args: MultiValue) -> String {
    args.iter()
        .map(|v| match v {
            mlua::Value::String(s) => s.to_string_lossy(),
            other => other.to_string().unwrap_or_else(|_| format!("{other:?}")),
        })
        .collect::<Vec<_>>()
        .join("\t")
}

fn setup(lua: &Lua, emit: impl Fn(ScriptMsg) + Clone + 'static, stop: Arc<AtomicBool>, rx: Rc<RefCell<RxState>>) -> mlua::Result<()> {
    let g = lua.globals();
    {
        let stop = stop.clone();
        lua.set_hook(HookTriggers::new().every_nth_instruction(1000), move |_, _| {
            if stop.load(Ordering::Relaxed) { Err(stopped()) } else { Ok(VmState::Continue) }
        })?;
    }

    // --- output
    {
        let e = emit.clone();
        g.set("log", lua.create_function(move |_, args: MultiValue| {
            e(ScriptMsg::Log(values_to_text(args)));
            Ok(())
        })?)?;
    }
    {
        let e = emit.clone();
        g.set("print", lua.create_function(move |_, args: MultiValue| {
            e(ScriptMsg::Log(values_to_text(args)));
            Ok(())
        })?)?;
    }

    // --- sending
    {
        let e = emit.clone();
        g.set("send", lua.create_function(move |_, s: mlua::LuaString| {
            let text = String::from_utf8_lossy(&s.as_bytes()).into_owned();
            let d = codec::parse_text(&text).map_err(mlua::Error::runtime)?;
            e(ScriptMsg::Send(d));
            Ok(())
        })?)?;
    }
    {
        let e = emit.clone();
        g.set("send_hex", lua.create_function(move |_, s: String| {
            let d = codec::parse_hex(&s).map_err(mlua::Error::runtime)?;
            e(ScriptMsg::Send(d));
            Ok(())
        })?)?;
    }
    {
        let e = emit.clone();
        g.set("send_bytes", lua.create_function(move |_, s: mlua::LuaString| {
            e(ScriptMsg::Send(s.as_bytes().to_vec()));
            Ok(())
        })?)?;
    }

    // --- timing
    {
        let st = stop.clone();
        g.set("wait", lua.create_function(move |_, ms: i64| nap(&st, ms))?)?;
    }
    {
        let start = Instant::now();
        g.set("now_ms", lua.create_function(move |_, ()| Ok(start.elapsed().as_millis() as i64))?)?;
    }

    // --- receiving
    let recv_raw = {
        let rx = rx.clone();
        let st = stop.clone();
        move |timeout: Option<i64>| -> mlua::Result<Vec<u8>> {
            let end = Instant::now() + Duration::from_millis(timeout.unwrap_or(1000).max(0) as u64);
            loop {
                rx.borrow_mut().pull();
                if !rx.borrow().buf.is_empty() {
                    // give the rest of a burst a moment to arrive
                    nap(&st, 20)?;
                    rx.borrow_mut().pull();
                    return Ok(std::mem::take(&mut rx.borrow_mut().buf));
                }
                if Instant::now() >= end {
                    return Ok(Vec::new());
                }
                nap(&st, 5)?;
            }
        }
    };
    {
        let r = recv_raw.clone();
        g.set("recv", lua.create_function(move |lua, t: Option<i64>| lua.create_string(r(t)?))?)?;
    }
    {
        let r = recv_raw.clone();
        g.set("recv_hex", lua.create_function(move |_, t: Option<i64>| Ok(codec::to_hex(&r(t)?)))?)?;
    }
    {
        let rx = rx.clone();
        let st = stop.clone();
        g.set("wait_for", lua.create_function(move |_, (pat, timeout): (mlua::LuaString, Option<i64>)| {
            let text = String::from_utf8_lossy(&pat.as_bytes()).into_owned();
            let needle = codec::parse_text(&text).map_err(mlua::Error::runtime)?;
            let end = Instant::now() + Duration::from_millis(timeout.unwrap_or(1000).max(0) as u64);
            loop {
                {
                    let mut s = rx.borrow_mut();
                    s.pull();
                    if let Some(p) = find(&s.buf, &needle) {
                        s.buf.drain(..p + needle.len());
                        return Ok(true);
                    }
                }
                if Instant::now() >= end {
                    return Ok(false);
                }
                nap(&st, 5)?;
            }
        })?)?;
    }
    {
        let rx = rx.clone();
        g.set("clear_rx", lua.create_function(move |_, ()| {
            let mut s = rx.borrow_mut();
            s.pull();
            s.buf.clear();
            Ok(())
        })?)?;
    }

    // --- helpers (byte data = Lua string)
    g.set("from_hex", lua.create_function(|lua, s: String| {
        lua.create_string(codec::parse_hex(&s).map_err(mlua::Error::runtime)?)
    })?)?;
    g.set("to_hex", lua.create_function(|_, s: mlua::LuaString| Ok(codec::to_hex(&s.as_bytes())))?)?;
    g.set("crc16_modbus", lua.create_function(|_, s: mlua::LuaString| Ok(codec::crc16_modbus(&s.as_bytes()) as i64))?)?;
    g.set("crc16_ccitt", lua.create_function(|_, s: mlua::LuaString| Ok(codec::crc16_ccitt(&s.as_bytes()) as i64))?)?;
    g.set("crc32", lua.create_function(|_, s: mlua::LuaString| Ok(codec::crc32(&s.as_bytes()) as i64))?)?;
    g.set("crc8", lua.create_function(|_, s: mlua::LuaString| Ok(codec::crc8(&s.as_bytes()) as i64))?)?;
    g.set("sum8", lua.create_function(|_, s: mlua::LuaString| Ok(codec::sum8(&s.as_bytes()) as i64))?)?;
    g.set("xor8", lua.create_function(|_, s: mlua::LuaString| Ok(codec::xor8(&s.as_bytes()) as i64))?)?;

    // scripts talk to the connection only: no files, processes or modules
    for name in ["os", "io", "require", "dofile", "loadfile", "package", "debug"] {
        g.set(name, mlua::Value::Nil)?;
    }
    Ok(())
}

pub fn run(ctx: eframe::egui::Context, src: String) -> ScriptRun {
    let stop = Arc::new(AtomicBool::new(false));
    let (feed_tx, feed_rx) = channel::<Vec<u8>>();
    let (out_tx, out_rx) = channel::<ScriptMsg>();
    let stop2 = stop.clone();
    thread::spawn(move || {
        let stop = stop2;
        let emit = move |m: ScriptMsg| {
            let _ = out_tx.send(m);
            ctx.request_repaint();
        };
        let rx = Rc::new(RefCell::new(RxState { rx: feed_rx, buf: Vec::new() }));
        let lua = Lua::new();
        if let Err(e) = setup(&lua, emit.clone(), stop.clone(), rx) {
            emit(ScriptMsg::Error(format!("Skript konnte nicht vorbereitet werden: {e}")));
            emit(ScriptMsg::Done);
            return;
        }
        emit(ScriptMsg::Log("Skript gestartet".into()));
        match lua.load(&src).set_name("Skript").exec() {
            Ok(()) => emit(ScriptMsg::Log("Skript beendet".into())),
            Err(e) => {
                let msg = e.to_string();
                if stop.load(Ordering::Relaxed) || msg.contains(STOPPED) {
                    emit(ScriptMsg::Log(STOPPED.into()));
                } else {
                    let first = msg.lines().next().unwrap_or(&msg).replace("runtime error: ", "");
                    emit(ScriptMsg::Error(format!("Skriptfehler: {first}")));
                }
            }
        }
        emit(ScriptMsg::Done);
    });
    ScriptRun { stop, feed: feed_tx, out: out_rx }
}

pub const EXAMPLES: &[(&str, &str)] = &[
    (
        "Schleife: 10 Nachrichten",
        r#"-- Schickt 10 nummerierte Nachrichten im Abstand von 500 ms
for i = 1, 10 do
    send("Nachricht " .. i .. "\r\n")
    wait(500)
end
"#,
    ),
    (
        "Frage / Antwort (AT-Befehl)",
        r#"-- Sendet einen Befehl und wartet bis zu 2 s auf "OK"
clear_rx()
send("AT\r")
if wait_for("OK", 2000) then
    log("Gerät hat mit OK geantwortet")
else
    log("Keine Antwort innerhalb von 2 s")
end
"#,
    ),
    (
        "Modbus RTU mit CRC",
        r#"-- Holding-Register lesen: Gerät 1, ab Adresse 0, 10 Register
local req = from_hex("01 03 00 00 00 0A")
local crc = crc16_modbus(req)
req = req .. string.char(crc & 0xFF, (crc >> 8) & 0xFF)  -- CRC low byte zuerst
send_bytes(req)
local antwort = recv(1000)
if #antwort == 0 then
    log("Keine Antwort")
else
    log("Antwort: " .. to_hex(antwort))
end
"#,
    ),
    (
        "Automatischer Antworter",
        r#"-- Antwortet auf jedes "PING" mit "PONG" – läuft bis Stopp
while true do
    if wait_for("PING", 1000) then
        send("PONG\r\n")
        log("PING beantwortet")
    end
end
"#,
    ),
    (
        "Endlos-Schleife mit Zähler",
        r#"-- Sendet jede Sekunde AA + Zähler-Byte – läuft bis Stopp
local n = 0
while true do
    send_bytes(string.char(0xAA, n % 256))
    n = n + 1
    wait(1000)
end
"#,
    ),
];

pub const HELP: &str = "Sprache: Lua 5.4  (Kommentar: -- …, Text verbinden: ..)
Befehle:
  send(\"Text\\r\\n\")      Text senden ($XX und #DDD erlaubt)
  send_hex(\"01 02 FF\")  Bytes als Hex senden
  send_bytes(daten)      Bytes roh senden (Lua-String)
  wait(ms)               warten
  recv(ms)               Empfangenes holen (wartet max. ms)
  recv_hex(ms)           … als Hex-Text
  wait_for(\"OK\", ms)    true, wenn \"OK\" rechtzeitig kam
  clear_rx()             Empfangspuffer leeren
  log(…) / print(…)      Meldung ins Fenster schreiben
  now_ms()               Millisekunden seit Skriptstart
Helfer:
  from_hex(\"01 02\"), to_hex(daten), string.char(1, 2), #daten
  crc16_modbus(d), crc16_ccitt(d), crc32(d), crc8(d), sum8(d), xor8(d)";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_sends_and_waits_for_answer() {
        let ctx = eframe::egui::Context::default();
        let src = r#"
            local req = from_hex("01 03 00 00 00 0A")
            local crc = crc16_modbus(req)
            req = req .. string.char(crc & 0xFF, (crc >> 8) & 0xFF)
            send_bytes(req)
            if wait_for("OK", 2000) then log("ja") else log("nein") end
        "#;
        let run = run(ctx, src.into());
        let mut sent = None;
        let mut logs = Vec::new();
        let end = Instant::now() + Duration::from_secs(5);
        'o: while Instant::now() < end {
            for m in run.poll() {
                match m {
                    ScriptMsg::Send(d) => {
                        sent = Some(d);
                        run.feed(b"xxOKyy");
                    }
                    ScriptMsg::Log(l) => logs.push(l),
                    ScriptMsg::Error(e) => panic!("{e}"),
                    ScriptMsg::Done => break 'o,
                }
            }
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(sent.unwrap(), vec![1, 3, 0, 0, 0, 0x0A, 0xC5, 0xCD]);
        assert!(logs.iter().any(|l| l == "ja"), "{logs:?}");
    }

    #[test]
    fn script_can_be_stopped() {
        let ctx = eframe::egui::Context::default();
        let run = run(ctx, "while true do end".into());
        thread::sleep(Duration::from_millis(100));
        run.stop();
        let end = Instant::now() + Duration::from_secs(3);
        let mut done = false;
        while Instant::now() < end && !done {
            done = run.poll().into_iter().any(|m| matches!(m, ScriptMsg::Done));
            thread::sleep(Duration::from_millis(5));
        }
        assert!(done);
    }

    #[test]
    fn examples_compile() {
        let lua = Lua::new();
        for (name, src) in EXAMPLES {
            lua.load(*src).into_function().unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }
}
