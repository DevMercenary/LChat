//! Сетевой слой: один активный TCP-пир, обмен кадрами (текст / файл).
//! Работает в фоновых потоках, общается с UI через каналы std::sync::mpsc.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;

/// Порт, который слушают оба экземпляра приложения.
pub const PORT: u16 = 9009;

/// Максимальный размер одного кадра (защита от мусора / зависания) — 4 ГиБ.
const MAX_FRAME: u64 = 4 * 1024 * 1024 * 1024;

const KIND_TEXT: u8 = 0;
const KIND_FILE: u8 = 1;

/// Команды от UI к сетевому потоку (плюс внутренние входящие подключения).
pub enum ToNet {
    /// Входящее соединение, пойманное слушающим потоком.
    Inbound(TcpStream),
    /// Подключиться к адресу ("ip" или "ip:port").
    Connect(String),
    SendText(String),
    SendFile(PathBuf),
    Disconnect,
}

/// События от сети к UI.
pub enum FromNet {
    Status(String),
    Connected(String),
    Disconnected,
    Text(String),
    File { name: String, path: PathBuf, size: u64 },
    Error(String),
}

/// Запускает сетевые потоки и возвращает пару каналов (в сеть / из сети).
pub fn spawn(downloads: PathBuf) -> (Sender<ToNet>, Receiver<FromNet>) {
    let (to_tx, to_rx) = channel::<ToNet>();
    let (from_tx, from_rx) = channel::<FromNet>();

    // Поток-слушатель: принимает входящие соединения и заворачивает их в ToNet::Inbound.
    {
        let to_tx = to_tx.clone();
        let from_tx = from_tx.clone();
        thread::spawn(move || match TcpListener::bind(("0.0.0.0", PORT)) {
            Ok(listener) => {
                let _ = from_tx.send(FromNet::Status(format!("Ожидаю подключений на порту {PORT}")));
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

    // Поток-менеджер: единственное активное соединение, отправка данных.
    {
        let from_tx = from_tx.clone();
        thread::spawn(move || {
            let mut current: Option<TcpStream> = None;
            for msg in to_rx.iter() {
                match msg {
                    ToNet::Inbound(s) => set_connection(s, &mut current, &from_tx, &downloads),
                    ToNet::Connect(addr) => {
                        let full = if addr.contains(':') {
                            addr.clone()
                        } else {
                            format!("{addr}:{PORT}")
                        };
                        let _ = from_tx.send(FromNet::Status(format!("Подключаюсь к {full}…")));
                        match TcpStream::connect(&full) {
                            Ok(s) => set_connection(s, &mut current, &from_tx, &downloads),
                            Err(e) => {
                                let _ = from_tx
                                    .send(FromNet::Error(format!("Не удалось подключиться: {e}")));
                            }
                        }
                    }
                    ToNet::SendText(text) => {
                        send_frame(&mut current, KIND_TEXT, text.as_bytes(), &from_tx);
                    }
                    ToNet::SendFile(path) => match std::fs::read(&path) {
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
                            if send_frame(&mut current, KIND_FILE, &payload, &from_tx) {
                                let _ = from_tx.send(FromNet::Status(format!(
                                    "Файл отправлен: {name} ({})",
                                    human_size(data.len() as u64)
                                )));
                            }
                        }
                        Err(e) => {
                            let _ = from_tx
                                .send(FromNet::Error(format!("Не удалось прочитать файл: {e}")));
                        }
                    },
                    ToNet::Disconnect => {
                        if let Some(old) = current.take() {
                            let _ = old.shutdown(Shutdown::Both);
                        }
                        let _ = from_tx.send(FromNet::Disconnected);
                    }
                }
            }
        });
    }

    (to_tx, from_rx)
}

/// Устанавливает новое активное соединение и запускает для него поток чтения.
fn set_connection(
    s: TcpStream,
    current: &mut Option<TcpStream>,
    from_tx: &Sender<FromNet>,
    downloads: &PathBuf,
) {
    if let Some(old) = current.take() {
        let _ = old.shutdown(Shutdown::Both);
    }
    let peer = s
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| "неизвестно".to_string());
    match s.try_clone() {
        Ok(read_half) => {
            *current = Some(s);
            let _ = from_tx.send(FromNet::Connected(peer));
            let tx = from_tx.clone();
            let dl = downloads.clone();
            thread::spawn(move || reader_loop(read_half, tx, dl));
        }
        Err(e) => {
            let _ = from_tx.send(FromNet::Error(format!("Ошибка соединения: {e}")));
        }
    }
}

/// Отправляет кадр в текущее соединение. Возвращает true при успехе.
fn send_frame(
    current: &mut Option<TcpStream>,
    kind: u8,
    payload: &[u8],
    from_tx: &Sender<FromNet>,
) -> bool {
    let Some(s) = current.as_mut() else {
        let _ = from_tx.send(FromNet::Error("Нет активного соединения".to_string()));
        return false;
    };
    match write_frame(s, kind, payload) {
        Ok(()) => true,
        Err(e) => {
            *current = None;
            let _ = from_tx.send(FromNet::Error(format!("Ошибка отправки: {e}")));
            let _ = from_tx.send(FromNet::Disconnected);
            false
        }
    }
}

/// Цикл чтения кадров из соединения.
fn reader_loop(mut s: TcpStream, tx: Sender<FromNet>, downloads: PathBuf) {
    loop {
        match read_frame(&mut s) {
            Ok((KIND_TEXT, payload)) => {
                let _ = tx.send(FromNet::Text(String::from_utf8_lossy(&payload).to_string()));
            }
            Ok((KIND_FILE, payload)) => {
                if payload.len() < 2 {
                    continue;
                }
                let name_len = u16::from_be_bytes([payload[0], payload[1]]) as usize;
                if payload.len() < 2 + name_len {
                    continue;
                }
                let name = String::from_utf8_lossy(&payload[2..2 + name_len]).to_string();
                let data = &payload[2 + name_len..];
                let safe = sanitize(&name);
                let path = unique_path(&downloads, &safe);
                let res = std::fs::create_dir_all(&downloads)
                    .and_then(|_| std::fs::write(&path, data));
                match res {
                    Ok(_) => {
                        let _ = tx.send(FromNet::File {
                            name,
                            path,
                            size: data.len() as u64,
                        });
                    }
                    Err(e) => {
                        let _ = tx.send(FromNet::Error(format!("Не удалось сохранить файл: {e}")));
                    }
                }
            }
            Ok(_) => {} // неизвестный тип кадра — пропускаем
            Err(_) => {
                let _ = tx.send(FromNet::Disconnected);
                break;
            }
        }
    }
}

fn write_frame(s: &mut TcpStream, kind: u8, payload: &[u8]) -> io::Result<()> {
    s.write_all(&[kind])?;
    s.write_all(&(payload.len() as u64).to_be_bytes())?;
    s.write_all(payload)?;
    s.flush()
}

fn read_frame(s: &mut TcpStream) -> io::Result<(u8, Vec<u8>)> {
    let mut hdr = [0u8; 9];
    s.read_exact(&mut hdr)?;
    let kind = hdr[0];
    let len = u64::from_be_bytes(hdr[1..9].try_into().unwrap());
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "кадр слишком большой"));
    }
    let mut buf = vec![0u8; len as usize];
    s.read_exact(&mut buf)?;
    Ok((kind, buf))
}

/// Оставляет только имя файла без путей и управляющих символов.
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

    #[test]
    fn frame_roundtrip_text_and_binary() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let a = read_frame(&mut s).unwrap();
            let b = read_frame(&mut s).unwrap();
            (a, b)
        });
        let mut c = TcpStream::connect(addr).unwrap();
        write_frame(&mut c, KIND_TEXT, "привет, мир".as_bytes()).unwrap();
        let blob: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
        write_frame(&mut c, KIND_FILE, &blob).unwrap();
        let ((k1, p1), (k2, p2)) = handle.join().unwrap();
        assert_eq!(k1, KIND_TEXT);
        assert_eq!(String::from_utf8(p1).unwrap(), "привет, мир");
        assert_eq!(k2, KIND_FILE);
        assert_eq!(p2, blob);
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
