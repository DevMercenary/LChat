//! Сетевой слой LChat.
//!
//! - Транспорт: TCP, один активный пир (порт [`PORT`]).
//! - Шифрование: Noise (паттерн NNpsk0, X25519 + ChaChaPoly + BLAKE2s).
//!   Общий пароль обеих сторон превращается в PSK — это и шифрование, и защита
//!   от постороннего/MITM: с неверным паролем рукопожатие не проходит.
//! - Автопоиск: UDP-broadcast «маячок» на порт [`DISCOVERY_PORT`].
//!
//! Все блокирующие операции живут в фоновых потоках и общаются с UI каналами.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use sha2::{Digest, Sha256};
use snow::{Builder, TransportState};

/// TCP-порт для чата.
pub const PORT: u16 = 9009;
/// UDP-порт для маячков автопоиска.
pub const DISCOVERY_PORT: u16 = 9010;

const NOISE_PARAMS: &str = "Noise_NNpsk0_25519_ChaChaPoly_BLAKE2s";
/// Максимальный размер одного сообщения Noise (ограничение протокола).
const MAX_NOISE_MSG: usize = 65535;
/// Максимум открытого текста в одном чанке (16 байт — тег AEAD).
const CHUNK: usize = MAX_NOISE_MSG - 16;
/// Ограничение на логический кадр (защита от мусора) — 8 ГиБ.
const MAX_FRAME: u64 = 8 * 1024 * 1024 * 1024;

const KIND_TEXT: u8 = 0;
const KIND_FILE: u8 = 1;

const BEACON_MAGIC: &[u8; 6] = b"LCHAT1";

/// Команды от UI к сети (плюс внутренние сообщения между потоками).
pub enum ToNet {
    Connect(String),
    SendText(String),
    SendFile(PathBuf),
    SetPsk([u8; 32]),
    Disconnect,
    // --- внутренние ---
    Inbound(TcpStream),
    ConnReady {
        gen: u64,
        cmd_tx: Sender<(u8, Vec<u8>)>,
        stream: TcpStream,
        peer: String,
    },
    ConnClosed {
        gen: u64,
    },
}

/// События от сети к UI.
pub enum FromNet {
    Status(String),
    Connected(String),
    Disconnected,
    Text(String),
    File { name: String, path: PathBuf, size: u64 },
    Discovered { name: String, ip: String },
    Error(String),
}

/// Хэширует пароль в 32-байтный PSK. Пустой пароль тоже валиден (обе стороны
/// должны совпадать). Домен-префикс отделяет от других применений хэша.
pub fn derive_psk(pass: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"LChat-noise-psk-v1");
    h.update(pass.as_bytes());
    let mut out = [0u8; 32];
    out.copy_from_slice(&h.finalize());
    out
}

/// Запускает сетевые потоки: слушатель TCP, менеджер соединения, автопоиск.
pub fn spawn(downloads: PathBuf, psk: [u8; 32]) -> (Sender<ToNet>, Receiver<FromNet>) {
    let (to_tx, to_rx) = channel::<ToNet>();
    let (from_tx, from_rx) = channel::<FromNet>();

    spawn_listener(to_tx.clone(), from_tx.clone());
    spawn_discovery(from_tx.clone());
    spawn_manager(to_tx.clone(), to_rx, from_tx, downloads, psk);

    (to_tx, from_rx)
}

/// Поток-слушатель входящих TCP-подключений.
fn spawn_listener(to_tx: Sender<ToNet>, from_tx: Sender<FromNet>) {
    thread::spawn(move || match TcpListener::bind(("0.0.0.0", PORT)) {
        Ok(listener) => {
            let _ = from_tx.send(FromNet::Status(format!(
                "Ожидаю подключений на порту {PORT}"
            )));
            for stream in listener.incoming().flatten() {
                if to_tx.send(ToNet::Inbound(stream)).is_err() {
                    break;
                }
            }
        }
        Err(e) => {
            let _ = from_tx.send(FromNet::Error(format!(
                "Не удалось занять порт {PORT}: {e}. Возможно, приложение уже запущено."
            )));
        }
    });
}

/// Поток-менеджер: одно активное соединение, маршрутизация отправки.
fn spawn_manager(
    to_tx: Sender<ToNet>,
    to_rx: Receiver<ToNet>,
    from_tx: Sender<FromNet>,
    downloads: PathBuf,
    initial_psk: [u8; 32],
) {
    struct Active {
        gen: u64,
        cmd_tx: Sender<(u8, Vec<u8>)>,
        stream: TcpStream,
    }

    thread::spawn(move || {
        let mut current: Option<Active> = None;
        let mut gen_counter: u64 = 0;
        let mut psk = initial_psk;

        for msg in to_rx.iter() {
            match msg {
                ToNet::SetPsk(p) => psk = p,

                ToNet::Connect(addr) => {
                    gen_counter += 1;
                    if let Some(old) = current.take() {
                        let _ = old.stream.shutdown(Shutdown::Both);
                        let _ = from_tx.send(FromNet::Disconnected);
                    }
                    spawn_connection(
                        gen_counter,
                        true,
                        ConnSource::Addr(addr),
                        psk,
                        from_tx.clone(),
                        to_tx.clone(),
                        downloads.clone(),
                    );
                }

                ToNet::Inbound(stream) => {
                    gen_counter += 1;
                    if let Some(old) = current.take() {
                        let _ = old.stream.shutdown(Shutdown::Both);
                        let _ = from_tx.send(FromNet::Disconnected);
                    }
                    spawn_connection(
                        gen_counter,
                        false,
                        ConnSource::Stream(stream),
                        psk,
                        from_tx.clone(),
                        to_tx.clone(),
                        downloads.clone(),
                    );
                }

                ToNet::ConnReady {
                    gen,
                    cmd_tx,
                    stream,
                    peer,
                } => {
                    if gen == gen_counter {
                        current = Some(Active {
                            gen,
                            cmd_tx,
                            stream,
                        });
                        let _ = from_tx.send(FromNet::Connected(peer));
                    } else {
                        // Устаревшая попытка — закрываем.
                        let _ = stream.shutdown(Shutdown::Both);
                    }
                }

                ToNet::ConnClosed { gen } => {
                    if current.as_ref().map(|a| a.gen) == Some(gen) {
                        if let Some(a) = current.take() {
                            let _ = a.stream.shutdown(Shutdown::Both);
                        }
                        let _ = from_tx.send(FromNet::Disconnected);
                    }
                }

                ToNet::SendText(t) => match &current {
                    Some(a) => {
                        let _ = a.cmd_tx.send((KIND_TEXT, t.into_bytes()));
                    }
                    None => {
                        let _ = from_tx
                            .send(FromNet::Error("Нет активного соединения".to_string()));
                    }
                },

                ToNet::SendFile(path) => {
                    if current.is_none() {
                        let _ = from_tx
                            .send(FromNet::Error("Нет активного соединения".to_string()));
                        continue;
                    }
                    match std::fs::read(&path) {
                        Ok(data) => {
                            let name = path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| "file.bin".to_string());
                            let name_bytes = name.as_bytes();
                            let mut payload =
                                Vec::with_capacity(2 + name_bytes.len() + data.len());
                            payload.extend_from_slice(&(name_bytes.len() as u16).to_be_bytes());
                            payload.extend_from_slice(name_bytes);
                            payload.extend_from_slice(&data);
                            if let Some(a) = &current {
                                let _ = a.cmd_tx.send((KIND_FILE, payload));
                                let _ = from_tx.send(FromNet::Status(format!(
                                    "Файл отправляется: {name} ({})",
                                    human_size(data.len() as u64)
                                )));
                            }
                        }
                        Err(e) => {
                            let _ = from_tx.send(FromNet::Error(format!(
                                "Не удалось прочитать файл: {e}"
                            )));
                        }
                    }
                }

                ToNet::Disconnect => {
                    if let Some(a) = current.take() {
                        let _ = a.stream.shutdown(Shutdown::Both);
                    }
                    let _ = from_tx.send(FromNet::Disconnected);
                }
            }
        }
    });
}

enum ConnSource {
    Addr(String),
    Stream(TcpStream),
}

/// Устанавливает соединение (рукопожатие Noise) в отдельном потоке и, при успехе,
/// запускает потоки чтения/записи и уведомляет менеджера через `ConnReady`.
fn spawn_connection(
    gen: u64,
    initiator: bool,
    source: ConnSource,
    psk: [u8; 32],
    from_tx: Sender<FromNet>,
    to_tx: Sender<ToNet>,
    downloads: PathBuf,
) {
    thread::spawn(move || {
        let mut stream = match source {
            ConnSource::Addr(addr) => {
                let full = if addr.contains(':') {
                    addr.clone()
                } else {
                    format!("{addr}:{PORT}")
                };
                let _ = from_tx.send(FromNet::Status(format!("Подключаюсь к {full}…")));
                match TcpStream::connect(&full) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = from_tx
                            .send(FromNet::Error(format!("Не удалось подключиться: {e}")));
                        let _ = to_tx.send(ToNet::ConnClosed { gen });
                        return;
                    }
                }
            }
            ConnSource::Stream(s) => s,
        };

        let _ = stream.set_nodelay(true);
        // Ограничим время рукопожатия, чтобы не зависнуть на «не том» подключении.
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));

        let transport = if initiator {
            handshake(&mut stream, &psk, true)
        } else {
            handshake(&mut stream, &psk, false)
        };

        let transport = match transport {
            Ok(t) => t,
            Err(e) => {
                let _ = from_tx.send(FromNet::Error(format!(
                    "Рукопожатие не удалось ({e}). Проверьте, что пароль совпадает на обоих компьютерах."
                )));
                let _ = to_tx.send(ToNet::ConnClosed { gen });
                let _ = stream.shutdown(Shutdown::Both);
                return;
            }
        };

        let _ = stream.set_read_timeout(None);
        let peer = stream
            .peer_addr()
            .map(|a| a.to_string())
            .unwrap_or_else(|_| "неизвестно".to_string());

        let noise = Arc::new(Mutex::new(transport));
        let (cmd_tx, cmd_rx) = channel::<(u8, Vec<u8>)>();

        // Поток записи.
        match stream.try_clone() {
            Ok(wstream) => {
                let wn = noise.clone();
                let wto = to_tx.clone();
                let wfrom = from_tx.clone();
                thread::spawn(move || writer_loop(wstream, wn, cmd_rx, gen, wto, wfrom));
            }
            Err(e) => {
                let _ = from_tx.send(FromNet::Error(format!("Ошибка соединения: {e}")));
                let _ = to_tx.send(ToNet::ConnClosed { gen });
                return;
            }
        }

        // Поток чтения.
        match stream.try_clone() {
            Ok(rstream) => {
                let rn = noise.clone();
                let rfrom = from_tx.clone();
                let rto = to_tx.clone();
                let dl = downloads.clone();
                thread::spawn(move || reader_loop(rstream, rn, rfrom, dl, gen, rto));
            }
            Err(e) => {
                let _ = from_tx.send(FromNet::Error(format!("Ошибка соединения: {e}")));
                let _ = to_tx.send(ToNet::ConnClosed { gen });
                return;
            }
        }

        let _ = to_tx.send(ToNet::ConnReady {
            gen,
            cmd_tx,
            stream,
            peer,
        });
    });
}

/// Рукопожатие Noise NNpsk0 (2 сообщения). `initiator` определяет роль.
fn handshake(stream: &mut TcpStream, psk: &[u8; 32], initiator: bool) -> Result<TransportState, String> {
    let params = NOISE_PARAMS.parse().map_err(|e| format!("params: {e}"))?;
    let builder = Builder::new(params)
        .psk(0, psk)
        .map_err(|e| format!("psk: {e}"))?;
    let mut hs = if initiator {
        builder.build_initiator()
    } else {
        builder.build_responder()
    }
    .map_err(|e| format!("build: {e}"))?;

    let mut buf = vec![0u8; MAX_NOISE_MSG];
    if initiator {
        // -> e
        let n = hs.write_message(&[], &mut buf).map_err(|e| e.to_string())?;
        write_lp(stream, &buf[..n]).map_err(|e| e.to_string())?;
        // <- e, ee
        let msg = read_lp(stream).map_err(|e| e.to_string())?;
        hs.read_message(&msg, &mut buf).map_err(|e| e.to_string())?;
    } else {
        // -> e
        let msg = read_lp(stream).map_err(|e| e.to_string())?;
        hs.read_message(&msg, &mut buf).map_err(|e| e.to_string())?;
        // <- e, ee
        let n = hs.write_message(&[], &mut buf).map_err(|e| e.to_string())?;
        write_lp(stream, &buf[..n]).map_err(|e| e.to_string())?;
    }
    hs.into_transport_mode().map_err(|e| e.to_string())
}

/// Поток записи: берёт (тип, payload), режет на чанки, шифрует, отправляет.
fn writer_loop(
    mut stream: TcpStream,
    noise: Arc<Mutex<TransportState>>,
    cmd_rx: Receiver<(u8, Vec<u8>)>,
    gen: u64,
    to_tx: Sender<ToNet>,
    from_tx: Sender<FromNet>,
) {
    for (kind, payload) in cmd_rx.iter() {
        if let Err(e) = send_frame(&mut stream, &noise, kind, &payload) {
            let _ = from_tx.send(FromNet::Error(format!("Ошибка отправки: {e}")));
            let _ = to_tx.send(ToNet::ConnClosed { gen });
            break;
        }
    }
}

/// Формирует логический кадр [тип][длина][payload], шифрует по чанкам и пишет в сокет.
fn send_frame(
    stream: &mut TcpStream,
    noise: &Arc<Mutex<TransportState>>,
    kind: u8,
    payload: &[u8],
) -> io::Result<()> {
    let mut plain = Vec::with_capacity(9 + payload.len());
    plain.push(kind);
    plain.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    plain.extend_from_slice(payload);

    let mut ct = vec![0u8; MAX_NOISE_MSG];
    for chunk in plain.chunks(CHUNK) {
        let n = {
            let mut guard = noise.lock().unwrap();
            guard
                .write_message(chunk, &mut ct)
                .map_err(io::Error::other)?
        };
        write_lp(stream, &ct[..n])?;
    }
    Ok(())
}

/// Поток чтения: расшифровывает чанки, собирает логические кадры, шлёт события в UI.
fn reader_loop(
    mut stream: TcpStream,
    noise: Arc<Mutex<TransportState>>,
    from_tx: Sender<FromNet>,
    downloads: PathBuf,
    gen: u64,
    to_tx: Sender<ToNet>,
) {
    let mut acc: Vec<u8> = Vec::new();
    let mut pt = vec![0u8; MAX_NOISE_MSG];

    'outer: loop {
        let ct = match read_lp(&mut stream) {
            Ok(v) => v,
            Err(_) => break,
        };
        let n = {
            let mut guard = noise.lock().unwrap();
            match guard.read_message(&ct, &mut pt) {
                Ok(n) => n,
                Err(_) => break,
            }
        };
        acc.extend_from_slice(&pt[..n]);

        loop {
            if acc.len() < 9 {
                break;
            }
            let len = u64::from_be_bytes(acc[1..9].try_into().unwrap());
            if len > MAX_FRAME {
                let _ = from_tx.send(FromNet::Error("Получен некорректный кадр".to_string()));
                break 'outer;
            }
            let total = 9 + len as usize;
            if acc.len() < total {
                break;
            }
            let kind = acc[0];
            let payload = acc[9..total].to_vec();
            acc.drain(..total);
            handle_frame(kind, payload, &from_tx, &downloads);
        }
    }
    let _ = to_tx.send(ToNet::ConnClosed { gen });
}

/// Обрабатывает один расшифрованный логический кадр.
fn handle_frame(kind: u8, payload: Vec<u8>, from_tx: &Sender<FromNet>, downloads: &PathBuf) {
    match kind {
        KIND_TEXT => {
            let _ = from_tx.send(FromNet::Text(String::from_utf8_lossy(&payload).to_string()));
        }
        KIND_FILE => {
            if payload.len() < 2 {
                return;
            }
            let name_len = u16::from_be_bytes([payload[0], payload[1]]) as usize;
            if payload.len() < 2 + name_len {
                return;
            }
            let name = String::from_utf8_lossy(&payload[2..2 + name_len]).to_string();
            let data = &payload[2 + name_len..];
            let safe = sanitize(&name);
            let path = unique_path(downloads, &safe);
            let res =
                std::fs::create_dir_all(downloads).and_then(|_| std::fs::write(&path, data));
            match res {
                Ok(_) => {
                    let _ = from_tx.send(FromNet::File {
                        name,
                        path,
                        size: data.len() as u64,
                    });
                }
                Err(e) => {
                    let _ = from_tx
                        .send(FromNet::Error(format!("Не удалось сохранить файл: {e}")));
                }
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Автопоиск (UDP broadcast)
// ---------------------------------------------------------------------------

/// Поток автопоиска: периодически рассылает маячок и слушает чужие.
fn spawn_discovery(from_tx: Sender<FromNet>) {
    let socket = match make_discovery_socket() {
        Ok(s) => s,
        Err(_) => return, // без автопоиска, но чат работает по ручному вводу IP
    };
    let id = instance_id();
    let name = hostname();

    // Отправитель маячков.
    {
        let socket = match socket.try_clone() {
            Ok(s) => s,
            Err(_) => return,
        };
        let name = name.clone();
        thread::spawn(move || {
            let mut pkt = Vec::new();
            pkt.extend_from_slice(BEACON_MAGIC);
            pkt.extend_from_slice(&PORT.to_be_bytes());
            pkt.extend_from_slice(&id.to_be_bytes());
            pkt.extend_from_slice(name.as_bytes());
            loop {
                let _ = socket.send_to(&pkt, ("255.255.255.255", DISCOVERY_PORT));
                thread::sleep(Duration::from_secs(2));
            }
        });
    }

    // Приёмник маячков.
    thread::spawn(move || {
        let mut buf = [0u8; 512];
        loop {
            let (n, src) = match socket.recv_from(&mut buf) {
                Ok(v) => v,
                Err(_) => {
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }
            };
            if let Some((peer_id, peer_name)) = parse_beacon(&buf[..n]) {
                if peer_id == id {
                    continue; // собственный маячок
                }
                let _ = from_tx.send(FromNet::Discovered {
                    name: peer_name,
                    ip: src.ip().to_string(),
                });
            }
        }
    });
}

fn make_discovery_socket() -> io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, SockAddr, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    socket.set_broadcast(true)?;
    let addr: std::net::SocketAddr =
        std::net::SocketAddr::from(([0, 0, 0, 0], DISCOVERY_PORT));
    socket.bind(&SockAddr::from(addr))?;
    Ok(socket.into())
}

fn parse_beacon(data: &[u8]) -> Option<(u64, String)> {
    if data.len() < 6 + 2 + 8 || &data[..6] != BEACON_MAGIC {
        return None;
    }
    let id = u64::from_be_bytes(data[8..16].try_into().ok()?);
    let name_bytes = &data[16..data.len().min(16 + 64)];
    let name = String::from_utf8_lossy(name_bytes).trim().to_string();
    let name = if name.is_empty() {
        "неизвестный".to_string()
    } else {
        name
    };
    Some((id, name))
}

fn instance_id() -> u64 {
    let pid = std::process::id() as u64;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    pid ^ nanos.rotate_left(17)
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "компьютер".to_string())
}

// ---------------------------------------------------------------------------
// Общие помощники
// ---------------------------------------------------------------------------

/// Записывает кадр с 2-байтным префиксом длины (для рукопожатия и чанков).
fn write_lp(s: &mut TcpStream, data: &[u8]) -> io::Result<()> {
    debug_assert!(data.len() <= u16::MAX as usize);
    s.write_all(&(data.len() as u16).to_be_bytes())?;
    s.write_all(data)?;
    s.flush()
}

/// Читает кадр с 2-байтным префиксом длины.
fn read_lp(s: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 2];
    s.read_exact(&mut len)?;
    let n = u16::from_be_bytes(len) as usize;
    let mut buf = vec![0u8; n];
    s.read_exact(&mut buf)?;
    Ok(buf)
}

/// Оставляет только имя файла без путей и опасных символов.
fn sanitize(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "file.bin".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Подбирает не занятое имя вида "name (1).ext".
fn unique_path(dir: &PathBuf, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (name.to_string(), String::new()),
    };
    let mut i = 1u32;
    loop {
        let cand = dir.join(format!("{stem} ({i}){ext}"));
        if !cand.exists() {
            return cand;
        }
        i += 1;
    }
}

/// Человекочитаемый размер.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["Б", "КБ", "МБ", "ГБ", "ТБ"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    fn transport_pair(pass_a: &str, pass_b: &str) -> Option<(TransportState, TransportState)> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let psk_b = derive_psk(pass_b);
        let srv = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            handshake(&mut s, &psk_b, false).ok().map(|t| (t, s))
        });
        let mut c = TcpStream::connect(addr).unwrap();
        let psk_a = derive_psk(pass_a);
        let cli = handshake(&mut c, &psk_a, true).ok();
        let server = srv.join().unwrap();
        match (cli, server) {
            (Some(ci), Some((se, _))) => Some((ci, se)),
            _ => None,
        }
    }

    #[test]
    fn handshake_and_encrypted_roundtrip() {
        let (ci, se) = transport_pair("секрет", "секрет").expect("рукопожатие");
        let ci = Arc::new(Mutex::new(ci));
        let se = Arc::new(Mutex::new(se));

        // Клиент шлёт большой файл (несколько чанков) + текст; сервер читает.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let se2 = se.clone();
        let (tx, rx) = channel::<FromNet>();
        let dir = std::env::temp_dir().join(format!("lchat-test-{}", std::process::id()));
        let dir2 = dir.clone();
        let srv = std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            reader_loop(s, se2, tx, dir2, 1, channel().0);
        });
        let mut c = TcpStream::connect(addr).unwrap();
        let blob: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
        let mut file_payload = Vec::new();
        let fname = "большой.bin";
        file_payload.extend_from_slice(&(fname.len() as u16).to_be_bytes());
        file_payload.extend_from_slice(fname.as_bytes());
        file_payload.extend_from_slice(&blob);
        send_frame(&mut c, &ci, KIND_FILE, &file_payload).unwrap();
        send_frame(&mut c, &ci, KIND_TEXT, "привет".as_bytes()).unwrap();
        c.shutdown(Shutdown::Both).unwrap();
        srv.join().unwrap();

        let mut got_file = None;
        let mut got_text = None;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                FromNet::File { size, path, .. } => got_file = Some((size, path)),
                FromNet::Text(t) => got_text = Some(t),
                _ => {}
            }
        }
        assert_eq!(got_text.as_deref(), Some("привет"));
        let (size, path) = got_file.expect("файл получен");
        assert_eq!(size, blob.len() as u64);
        assert_eq!(std::fs::read(&path).unwrap(), blob);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wrong_password_fails_handshake() {
        assert!(transport_pair("правильный", "неправильный").is_none());
    }

    #[test]
    fn beacon_roundtrip() {
        let mut pkt = Vec::new();
        pkt.extend_from_slice(BEACON_MAGIC);
        pkt.extend_from_slice(&PORT.to_be_bytes());
        pkt.extend_from_slice(&42u64.to_be_bytes());
        pkt.extend_from_slice("мойпк".as_bytes());
        let (id, name) = parse_beacon(&pkt).unwrap();
        assert_eq!(id, 42);
        assert_eq!(name, "мойпк");
        assert!(parse_beacon(b"garbage").is_none());
    }

    #[test]
    fn sanitize_strips_paths_and_bad_chars() {
        assert_eq!(sanitize("../../etc/passwd"), "passwd");
        assert_eq!(sanitize("C:\\Windows\\secret.txt"), "secret.txt");
        assert_eq!(sanitize("a:b*c?.txt"), "abc.txt");
        assert_eq!(sanitize("   "), "file.bin");
    }

    #[test]
    fn human_size_units() {
        assert_eq!(human_size(512), "512 Б");
        assert_eq!(human_size(1024), "1.0 КБ");
        assert_eq!(human_size(1536), "1.5 КБ");
    }
}
