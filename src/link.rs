//! Background workers for serial ports, TCP client, TCP server and UDP.
//! Each worker runs on its own thread and talks to the UI through channels.

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ModemLines {
    pub cts: bool,
    pub dsr: bool,
    pub ri: bool,
    pub cd: bool,
}

pub enum Event {
    Info(String),
    Error(String),
    Opened,
    Closed,
    Rx { peer: Option<String>, data: Vec<u8> },
    Tx { peer: Option<String>, data: Vec<u8> },
    ClientUp(String),
    ClientDown(String),
    Lines(ModemLines),
}

pub enum Cmd {
    /// `to`: TCP server – a client address (None = all clients);
    /// UDP – "host:port" of the receiver.
    Send { to: Option<String>, data: Vec<u8> },
    SetDtr(bool),
    SetRts(bool),
    SetBreak(bool),
    Kick(String),
    Close,
}

pub struct Link {
    tx: Sender<Cmd>,
    rx: Receiver<Event>,
}

impl Link {
    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }
    pub fn poll(&self) -> Vec<Event> {
        let mut v = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(e) => v.push(e),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    break;
                }
            }
        }
        v
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Close);
    }
}

struct Out {
    tx: Sender<Event>,
    ctx: eframe::egui::Context,
}

impl Out {
    fn emit(&self, e: Event) {
        let _ = self.tx.send(e);
        self.ctx.request_repaint();
    }
}

fn spawn<F>(ctx: &eframe::egui::Context, f: F) -> Link
where
    F: FnOnce(Out, Receiver<Cmd>) + Send + 'static,
{
    let (cmd_tx, cmd_rx) = channel();
    let (ev_tx, ev_rx) = channel();
    let out = Out { tx: ev_tx, ctx: ctx.clone() };
    thread::spawn(move || f(out, cmd_rx));
    Link { tx: cmd_tx, rx: ev_rx }
}

fn resolve(host: &str, port: u16) -> Result<SocketAddr, String> {
    (host.trim(), port)
        .to_socket_addrs()
        .map_err(|e| format!("Adresse '{host}' nicht auflösbar: {e}"))?
        .next()
        .ok_or_else(|| format!("Adresse '{host}' nicht auflösbar"))
}

// ---------------------------------------------------------------- serial

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Parity {
    None,
    Odd,
    Even,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Flow {
    None,
    Hardware,
    Software,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerialConfig {
    pub port: String,
    pub baud: u32,
    pub data_bits: u8,
    pub parity: Parity,
    pub stop_bits: u8,
    pub flow: Flow,
}

impl Default for SerialConfig {
    fn default() -> Self {
        Self { port: String::new(), baud: 9600, data_bits: 8, parity: Parity::None, stop_bits: 1, flow: Flow::None }
    }
}

pub fn list_serial_ports() -> Vec<String> {
    serialport::available_ports()
        .map(|v| v.into_iter().map(|p| p.port_name).collect())
        .unwrap_or_default()
}

pub fn open_serial(ctx: &eframe::egui::Context, cfg: SerialConfig, dtr: bool, rts: bool) -> Link {
    spawn(ctx, move |out, cmds| {
        let builder = serialport::new(&cfg.port, cfg.baud)
            .data_bits(match cfg.data_bits {
                5 => serialport::DataBits::Five,
                6 => serialport::DataBits::Six,
                7 => serialport::DataBits::Seven,
                _ => serialport::DataBits::Eight,
            })
            .parity(match cfg.parity {
                Parity::None => serialport::Parity::None,
                Parity::Odd => serialport::Parity::Odd,
                Parity::Even => serialport::Parity::Even,
            })
            .stop_bits(if cfg.stop_bits == 2 { serialport::StopBits::Two } else { serialport::StopBits::One })
            .flow_control(match cfg.flow {
                Flow::None => serialport::FlowControl::None,
                Flow::Hardware => serialport::FlowControl::Hardware,
                Flow::Software => serialport::FlowControl::Software,
            })
            .timeout(Duration::from_millis(15));
        let mut port = match builder.open() {
            Ok(p) => p,
            Err(e) => {
                out.emit(Event::Error(format!("{} konnte nicht geöffnet werden: {e}", cfg.port)));
                out.emit(Event::Closed);
                return;
            }
        };
        let _ = port.write_data_terminal_ready(dtr);
        let _ = port.write_request_to_send(rts);
        out.emit(Event::Opened);
        out.emit(Event::Info(format!(
            "{} geöffnet ({} Baud, {}{}{})",
            cfg.port,
            cfg.baud,
            cfg.data_bits,
            match cfg.parity {
                Parity::None => "N",
                Parity::Odd => "O",
                Parity::Even => "E",
            },
            cfg.stop_bits
        )));
        let mut buf = [0u8; 4096];
        let mut last_lines = None;
        let mut last_poll = Instant::now() - Duration::from_secs(1);
        'outer: loop {
            loop {
                match cmds.try_recv() {
                    Ok(Cmd::Send { data, .. }) => match port.write_all(&data) {
                        Ok(()) => out.emit(Event::Tx { peer: None, data }),
                        Err(e) => out.emit(Event::Error(format!("Senden fehlgeschlagen: {e}"))),
                    },
                    Ok(Cmd::SetDtr(v)) => {
                        let _ = port.write_data_terminal_ready(v);
                    }
                    Ok(Cmd::SetRts(v)) => {
                        let _ = port.write_request_to_send(v);
                    }
                    Ok(Cmd::SetBreak(v)) => {
                        let _ = if v { port.set_break() } else { port.clear_break() };
                    }
                    Ok(Cmd::Kick(_)) => {}
                    Ok(Cmd::Close) | Err(TryRecvError::Disconnected) => break 'outer,
                    Err(TryRecvError::Empty) => break,
                }
            }
            match port.read(&mut buf) {
                Ok(0) => {}
                Ok(n) => out.emit(Event::Rx { peer: None, data: buf[..n].to_vec() }),
                Err(e) if e.kind() == ErrorKind::TimedOut => {}
                Err(e) => {
                    out.emit(Event::Error(format!("Lesefehler: {e}")));
                    break;
                }
            }
            if last_poll.elapsed() > Duration::from_millis(100) {
                last_poll = Instant::now();
                let l = ModemLines {
                    cts: port.read_clear_to_send().unwrap_or(false),
                    dsr: port.read_data_set_ready().unwrap_or(false),
                    ri: port.read_ring_indicator().unwrap_or(false),
                    cd: port.read_carrier_detect().unwrap_or(false),
                };
                if last_lines != Some(l) {
                    last_lines = Some(l);
                    out.emit(Event::Lines(l));
                }
            }
        }
        out.emit(Event::Info(format!("{} geschlossen", cfg.port)));
        out.emit(Event::Closed);
    })
}

// ---------------------------------------------------------------- TCP client

pub fn open_tcp_client(ctx: &eframe::egui::Context, host: String, port: u16) -> Link {
    spawn(ctx, move |out, cmds| {
        let addr = match resolve(&host, port) {
            Ok(a) => a,
            Err(e) => {
                out.emit(Event::Error(e));
                out.emit(Event::Closed);
                return;
            }
        };
        out.emit(Event::Info(format!("Verbinde mit {addr} …")));
        let mut s = match TcpStream::connect_timeout(&addr, Duration::from_secs(5)) {
            Ok(s) => s,
            Err(e) => {
                out.emit(Event::Error(format!("Verbindung zu {addr} fehlgeschlagen: {e}")));
                out.emit(Event::Closed);
                return;
            }
        };
        let _ = s.set_nodelay(true);
        let _ = s.set_read_timeout(Some(Duration::from_millis(15)));
        out.emit(Event::Opened);
        out.emit(Event::Info(format!("Verbunden mit {addr}")));
        let mut buf = [0u8; 8192];
        'outer: loop {
            loop {
                match cmds.try_recv() {
                    Ok(Cmd::Send { data, .. }) => match s.write_all(&data) {
                        Ok(()) => out.emit(Event::Tx { peer: None, data }),
                        Err(e) => out.emit(Event::Error(format!("Senden fehlgeschlagen: {e}"))),
                    },
                    Ok(Cmd::Close) | Err(TryRecvError::Disconnected) => break 'outer,
                    Ok(_) => {}
                    Err(TryRecvError::Empty) => break,
                }
            }
            match s.read(&mut buf) {
                Ok(0) => {
                    out.emit(Event::Info("Gegenstelle hat die Verbindung beendet".into()));
                    break;
                }
                Ok(n) => out.emit(Event::Rx { peer: None, data: buf[..n].to_vec() }),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(e) => {
                    out.emit(Event::Error(format!("Verbindung unterbrochen: {e}")));
                    break;
                }
            }
        }
        let _ = s.shutdown(std::net::Shutdown::Both);
        out.emit(Event::Info("Getrennt".into()));
        out.emit(Event::Closed);
    })
}

// ---------------------------------------------------------------- TCP server

pub fn open_tcp_server(ctx: &eframe::egui::Context, bind: String, port: u16) -> Link {
    spawn(ctx, move |out, cmds| {
        let listener = match TcpListener::bind((bind.trim(), port)) {
            Ok(l) => l,
            Err(e) => {
                out.emit(Event::Error(format!("Port {port} kann nicht geöffnet werden: {e}")));
                out.emit(Event::Closed);
                return;
            }
        };
        let _ = listener.set_nonblocking(true);
        out.emit(Event::Opened);
        out.emit(Event::Info(format!("Server wartet auf {}:{port}", bind.trim())));
        let mut clients: Vec<(String, TcpStream)> = Vec::new();
        let mut buf = [0u8; 8192];
        'outer: loop {
            let mut busy = false;
            loop {
                match listener.accept() {
                    Ok((s, a)) => {
                        let _ = s.set_nonblocking(true);
                        let _ = s.set_nodelay(true);
                        let name = a.to_string();
                        out.emit(Event::ClientUp(name.clone()));
                        out.emit(Event::Info(format!("Client verbunden: {name}")));
                        clients.push((name, s));
                        busy = true;
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) => {
                        out.emit(Event::Error(format!("Accept-Fehler: {e}")));
                        break;
                    }
                }
            }
            loop {
                match cmds.try_recv() {
                    Ok(Cmd::Send { to, data }) => {
                        for (name, s) in clients.iter_mut() {
                            if to.as_ref().is_none_or(|t| t == name) {
                                match s.write_all(&data) {
                                    Ok(()) => out.emit(Event::Tx { peer: Some(name.clone()), data: data.clone() }),
                                    Err(e) => out.emit(Event::Error(format!("Senden an {name} fehlgeschlagen: {e}"))),
                                }
                            }
                        }
                        if clients.is_empty() {
                            out.emit(Event::Error("Kein Client verbunden".into()));
                        }
                    }
                    Ok(Cmd::Kick(name)) => {
                        clients.retain(|(n, s)| {
                            if *n == name {
                                let _ = s.shutdown(std::net::Shutdown::Both);
                                false
                            } else {
                                true
                            }
                        });
                        out.emit(Event::ClientDown(name.clone()));
                        out.emit(Event::Info(format!("Client getrennt: {name}")));
                    }
                    Ok(Cmd::Close) | Err(TryRecvError::Disconnected) => break 'outer,
                    Ok(_) => {}
                    Err(TryRecvError::Empty) => break,
                }
            }
            let mut gone = Vec::new();
            for (i, (name, s)) in clients.iter_mut().enumerate() {
                match s.read(&mut buf) {
                    Ok(0) => gone.push(i),
                    Ok(n) => {
                        busy = true;
                        out.emit(Event::Rx { peer: Some(name.clone()), data: buf[..n].to_vec() })
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                    Err(_) => gone.push(i),
                }
            }
            for i in gone.into_iter().rev() {
                let (name, _) = clients.remove(i);
                out.emit(Event::ClientDown(name.clone()));
                out.emit(Event::Info(format!("Client hat getrennt: {name}")));
            }
            if !busy {
                thread::sleep(Duration::from_millis(5));
            }
        }
        for (_, s) in &clients {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
        out.emit(Event::Info("Server gestoppt".into()));
        out.emit(Event::Closed);
    })
}

// ---------------------------------------------------------------- UDP

pub fn open_udp(ctx: &eframe::egui::Context, bind: String, port: u16, broadcast: bool) -> Link {
    spawn(ctx, move |out, cmds| {
        let sock = match UdpSocket::bind((bind.trim(), port)) {
            Ok(s) => s,
            Err(e) => {
                out.emit(Event::Error(format!("UDP-Port {port} kann nicht geöffnet werden: {e}")));
                out.emit(Event::Closed);
                return;
            }
        };
        let _ = sock.set_broadcast(broadcast);
        let _ = sock.set_read_timeout(Some(Duration::from_millis(15)));
        let local = sock.local_addr().map(|a| a.to_string()).unwrap_or_default();
        out.emit(Event::Opened);
        out.emit(Event::Info(format!("UDP lauscht auf {local}")));
        let mut buf = vec![0u8; 65536];
        'outer: loop {
            loop {
                match cmds.try_recv() {
                    Ok(Cmd::Send { to, data }) => {
                        let target = to.unwrap_or_default();
                        let res = target
                            .to_socket_addrs()
                            .map_err(|e| e.to_string())
                            .and_then(|mut a| a.next().ok_or_else(|| "keine Adresse".to_string()))
                            .and_then(|a| sock.send_to(&data, a).map_err(|e| e.to_string()));
                        match res {
                            Ok(_) => out.emit(Event::Tx { peer: Some(target), data }),
                            Err(e) => out.emit(Event::Error(format!("Senden an '{target}' fehlgeschlagen: {e}"))),
                        }
                    }
                    Ok(Cmd::Close) | Err(TryRecvError::Disconnected) => break 'outer,
                    Ok(_) => {}
                    Err(TryRecvError::Empty) => break,
                }
            }
            match sock.recv_from(&mut buf) {
                Ok((n, from)) => out.emit(Event::Rx { peer: Some(from.to_string()), data: buf[..n].to_vec() }),
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                // Windows reports ICMP "port unreachable" from earlier sends as a read error
                Err(e) if e.kind() == ErrorKind::ConnectionReset => {}
                Err(e) => {
                    out.emit(Event::Error(format!("UDP-Fehler: {e}")));
                    thread::sleep(Duration::from_millis(50));
                }
            }
        }
        out.emit(Event::Info("UDP geschlossen".into()));
        out.emit(Event::Closed);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_for(link: &Link, mut pred: impl FnMut(&Event) -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            for e in link.poll() {
                if pred(&e) {
                    return true;
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn tcp_server_and_client_talk() {
        let ctx = eframe::egui::Context::default();
        let srv = open_tcp_server(&ctx, "127.0.0.1".into(), 47311);
        assert!(wait_for(&srv, |e| matches!(e, Event::Opened)));
        let cli = open_tcp_client(&ctx, "127.0.0.1".into(), 47311);
        assert!(wait_for(&cli, |e| matches!(e, Event::Opened)));
        assert!(wait_for(&srv, |e| matches!(e, Event::ClientUp(_))));
        cli.send(Cmd::Send { to: None, data: b"hallo".to_vec() });
        assert!(wait_for(&srv, |e| matches!(e, Event::Rx { data, .. } if data == b"hallo")));
        srv.send(Cmd::Send { to: None, data: b"zurueck".to_vec() });
        assert!(wait_for(&cli, |e| matches!(e, Event::Rx { data, .. } if data == b"zurueck")));
    }

    #[test]
    fn udp_roundtrip() {
        let ctx = eframe::egui::Context::default();
        let a = open_udp(&ctx, "127.0.0.1".into(), 47312, false);
        let b = open_udp(&ctx, "127.0.0.1".into(), 47313, false);
        assert!(wait_for(&a, |e| matches!(e, Event::Opened)));
        assert!(wait_for(&b, |e| matches!(e, Event::Opened)));
        a.send(Cmd::Send { to: Some("127.0.0.1:47313".into()), data: vec![1, 2, 3] });
        assert!(wait_for(&b, |e| matches!(e, Event::Rx { data, peer: Some(p) } if data == &[1, 2, 3] && p.ends_with(":47312"))));
    }
}
