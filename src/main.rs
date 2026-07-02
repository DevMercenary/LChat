// LChat — локальный P2P-чат для обмена текстом и файлами по локальной сети.
// Прячем консольное окно на Windows в release-сборке.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod net;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui;
use net::{derive_psk, human_size, FromNet, ToNet, PORT};

/// Сколько секунд держим найденный пир в списке без нового маячка.
const PEER_TTL: Duration = Duration::from_secs(8);

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([720.0, 620.0])
            .with_min_inner_size([460.0, 380.0])
            .with_drag_and_drop(true) // важно для Windows
            .with_title("LChat — локальный чат"),
        ..Default::default()
    };
    eframe::run_native(
        "LChat",
        options,
        Box::new(|cc| {
            setup_fonts(&cc.egui_ctx);
            Ok(Box::new(App::new()))
        }),
    )
}

/// Подхватываем системный шрифт с кириллицей (Fedora / Windows), чтобы русский текст
/// точно отображался, не завися от встроенных шрифтов egui.
fn setup_fonts(ctx: &egui::Context) {
    const CANDIDATES: [&str; 6] = [
        "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/liberation-sans/LiberationSans-Regular.ttf",
        "C:\\Windows\\Fonts\\segoeui.ttf",
        "C:\\Windows\\Fonts\\arial.ttf",
    ];
    for path in CANDIDATES {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert(
                "sys".to_owned(),
                std::sync::Arc::new(egui::FontData::from_owned(bytes)),
            );
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .insert(0, "sys".to_owned());
            fonts
                .families
                .entry(egui::FontFamily::Monospace)
                .or_default()
                .push("sys".to_owned());
            ctx.set_fonts(fonts);
            break;
        }
    }
}

enum Kind {
    Me,
    Peer,
    System,
    Error,
    FileIn,
    FileOut,
}

/// Тип встроенного предпросмотра для файла.
enum Preview {
    None,
    Image,
    Video,
    Text { body: String, truncated: bool },
}

struct Entry {
    kind: Kind,
    text: String,
    path: Option<PathBuf>,
    preview: Preview,
}

struct App {
    to_net: Sender<ToNet>,
    from_net: Receiver<FromNet>,
    peer_ip: String,
    input: String,
    log: Vec<Entry>,
    status: String,
    connected: Option<String>,
    local_ips: Vec<String>,
    downloads: PathBuf,
    passphrase: String,
    last_psk: String,
    peers: HashMap<String, (String, Instant)>,
    /// Кэш текстур предпросмотра (картинки и кадры видео). None = не удалось.
    previews_tex: HashMap<PathBuf, Option<egui::TextureHandle>>,
}

impl App {
    fn new() -> Self {
        let downloads = default_downloads();
        let (to_net, from_net) = net::spawn(downloads.clone(), derive_psk(""));
        App {
            to_net,
            from_net,
            peer_ip: String::new(),
            input: String::new(),
            log: Vec::new(),
            status: "Запуск…".to_string(),
            connected: None,
            local_ips: list_local_ips(),
            downloads,
            passphrase: String::new(),
            last_psk: String::new(),
            peers: HashMap::new(),
            previews_tex: HashMap::new(),
        }
    }

    fn push(&mut self, kind: Kind, text: String, path: Option<PathBuf>) {
        let preview = match (&kind, &path) {
            (Kind::FileIn | Kind::FileOut, Some(p)) => classify_preview(p),
            _ => Preview::None,
        };
        self.log.push(Entry {
            kind,
            text,
            path,
            preview,
        });
    }

    fn send_text(&mut self) {
        let text = self.input.trim_end_matches(['\n', '\r']).to_string();
        if text.is_empty() {
            return;
        }
        if self.connected.is_none() {
            self.push(Kind::Error, "Нет соединения — сначала подключитесь".into(), None);
            return;
        }
        let _ = self.to_net.send(ToNet::SendText(text.clone()));
        self.push(Kind::Me, text, None);
        self.input.clear();
    }

    fn pick_and_send_file(&mut self) {
        if self.connected.is_none() {
            self.push(Kind::Error, "Нет соединения — сначала подключитесь".into(), None);
            return;
        }
        if let Some(path) = rfd::FileDialog::new().pick_file() {
            self.send_file_path(path);
        }
    }

    /// Отправляет один файл по пути (из диалога или drag-&-drop).
    fn send_file_path(&mut self, path: PathBuf) {
        if self.connected.is_none() {
            self.push(Kind::Error, "Нет соединения — сначала подключитесь".into(), None);
            return;
        }
        if path.is_dir() {
            self.push(
                Kind::Error,
                format!("Папки отправлять нельзя: {}", path.display()),
                None,
            );
            return;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let _ = self.to_net.send(ToNet::SendFile(path.clone()));
        self.push(Kind::FileOut, format!("Отправка файла: {name}"), Some(path));
    }

    fn drain_net(&mut self) {
        while let Ok(ev) = self.from_net.try_recv() {
            match ev {
                FromNet::Status(s) => self.status = s,
                FromNet::Connected(peer) => {
                    self.connected = Some(peer.clone());
                    self.status = format!("Соединено с {peer}");
                    self.push(Kind::System, format!("Соединение установлено: {peer}"), None);
                }
                FromNet::Disconnected => {
                    if self.connected.take().is_some() {
                        self.push(Kind::System, "Соединение разорвано".into(), None);
                    }
                    self.status = format!("Ожидаю подключений на порту {PORT}");
                }
                FromNet::Text(t) => self.push(Kind::Peer, t, None),
                FromNet::File { name, path, size } => self.push(
                    Kind::FileIn,
                    format!("Получен файл: {name} ({})", human_size(size)),
                    Some(path),
                ),
                FromNet::Discovered { name, ip } => {
                    self.peers.insert(ip, (name, Instant::now()));
                }
                FromNet::Error(e) => {
                    self.push(Kind::Error, e.clone(), None);
                    self.status = e;
                }
            }
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_net();
        // Регулярно перерисовываемся, чтобы входящие сообщения появлялись без действий пользователя.
        ctx.request_repaint_after(Duration::from_millis(150));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Пароль изменился -> пересобираем PSK для будущих соединений.
        if self.passphrase != self.last_psk {
            let _ = self.to_net.send(ToNet::SetPsk(derive_psk(&self.passphrase)));
            self.last_psk = self.passphrase.clone();
        }
        // Убираем протухшие найденные пиры.
        let now = Instant::now();
        self.peers
            .retain(|_, (_, seen)| now.duration_since(*seen) < PEER_TTL);

        // Drag-&-drop: отправляем брошенные в окно файлы.
        let dropped: Vec<PathBuf> = ui.ctx().input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        for path in dropped {
            self.send_file_path(path);
        }
        let files_hovering = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());

        egui::Panel::top("conn").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("IP собеседника:");
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.peer_ip)
                        .hint_text("например 192.168.1.42")
                        .desired_width(160.0),
                );
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("Подключиться").clicked() || enter {
                    let ip = self.peer_ip.trim().to_string();
                    if !ip.is_empty() {
                        let _ = self.to_net.send(ToNet::Connect(ip));
                    }
                }
                if self.connected.is_some() && ui.button("Отключиться").clicked() {
                    let _ = self.to_net.send(ToNet::Disconnect);
                }
            });
            ui.horizontal(|ui| {
                ui.label("Пароль (одинаковый на обоих):");
                ui.add(
                    egui::TextEdit::singleline(&mut self.passphrase)
                        .password(true)
                        .desired_width(180.0)
                        .hint_text("общий секрет для шифрования"),
                );
            });
            ui.add_space(2.0);
            ui.horizontal_wrapped(|ui| {
                let dot = if self.connected.is_some() { "🟢" } else { "⚪" };
                ui.label(format!("{dot} {}", self.status));
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("Мои адреса:");
                if self.local_ips.is_empty() {
                    ui.label("не определены");
                } else {
                    ui.monospace(format!("{}  (порт {PORT})", self.local_ips.join(", ")));
                }
            });
            let mut found: Vec<(String, String)> = self
                .peers
                .iter()
                .map(|(ip, (name, _))| (ip.clone(), name.clone()))
                .collect();
            found.sort();
            if !found.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Найдены в сети:");
                    for (ip, name) in &found {
                        if ui.button(format!("🖧 {name} ({ip})")).clicked() {
                            let _ = self.to_net.send(ToNet::Connect(ip.clone()));
                        }
                    }
                });
            }
            ui.add_space(4.0);
        });

        egui::Panel::bottom("compose").show(ui, |ui| {
            ui.add_space(4.0);
            let resp = ui.add(
                egui::TextEdit::multiline(&mut self.input)
                    .hint_text("Текст для отправки… (Ctrl+V — вставить, Ctrl+Enter — отправить)")
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
            let ctrl_enter = resp.has_focus()
                && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command);
            ui.horizontal(|ui| {
                if ui.button("Отправить").clicked() || ctrl_enter {
                    self.send_text();
                }
                if ui.button("📎 Файл…").clicked() {
                    self.pick_and_send_file();
                }
                ui.separator();
                if ui.button("🗀 Папка приёма").clicked() {
                    open_path(&self.downloads);
                }
            });
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    let mut copy: Option<String> = None;
                    let mut open: Option<PathBuf> = None;
                    for (idx, e) in self.log.iter().enumerate() {
                        let (tag, color) = match e.kind {
                            Kind::Me => ("Я", egui::Color32::from_rgb(90, 170, 255)),
                            Kind::Peer => ("Собеседник", egui::Color32::from_rgb(120, 220, 120)),
                            Kind::System => ("Система", egui::Color32::GRAY),
                            Kind::Error => ("Ошибка", egui::Color32::from_rgb(240, 120, 120)),
                            Kind::FileIn => ("Файл ↓", egui::Color32::from_rgb(230, 190, 90)),
                            Kind::FileOut => ("Файл ↑", egui::Color32::from_rgb(230, 190, 90)),
                        };
                        ui.horizontal_wrapped(|ui| {
                            ui.colored_label(color, format!("[{tag}]"));
                            ui.add(egui::Label::new(&e.text).wrap());
                        });

                        // Встроенный предпросмотр файла.
                        match &e.preview {
                            Preview::Image | Preview::Video => {
                                if let Some(p) = &e.path {
                                    let is_video = matches!(e.preview, Preview::Video);
                                    let tex = self
                                        .previews_tex
                                        .entry(p.clone())
                                        .or_insert_with(|| {
                                            load_preview_texture(ui.ctx(), p, is_video)
                                        });
                                    if let Some(t) = tex {
                                        let resp = ui
                                            .add(
                                                egui::Image::new(&*t)
                                                    .max_width(360.0)
                                                    .max_height(280.0)
                                                    .corner_radius(4.0),
                                            )
                                            .interact(egui::Sense::click());
                                        let resp = if is_video {
                                            ui.weak("▶ кадр видео — клик открывает в плеере");
                                            resp
                                        } else {
                                            resp.on_hover_text("Открыть полностью")
                                        };
                                        if resp.clicked() {
                                            open = Some(p.clone());
                                        }
                                    }
                                }
                            }
                            Preview::Text { body, truncated } => {
                                egui::CollapsingHeader::new("📄 Предпросмотр текста")
                                    .id_salt(idx)
                                    .default_open(true)
                                    .show(ui, |ui| {
                                        egui::ScrollArea::vertical()
                                            .id_salt(idx)
                                            .max_height(220.0)
                                            .auto_shrink([false, true])
                                            .show(ui, |ui| {
                                                ui.add(
                                                    egui::Label::new(
                                                        egui::RichText::new(body).monospace(),
                                                    )
                                                    .wrap(),
                                                );
                                            });
                                        if *truncated {
                                            ui.weak("… показано только начало файла");
                                        }
                                    });
                            }
                            Preview::None => {}
                        }

                        ui.horizontal(|ui| {
                            if matches!(e.kind, Kind::Me | Kind::Peer)
                                && ui.small_button("Копировать").clicked()
                            {
                                copy = Some(e.text.clone());
                            }
                            if let Some(p) = &e.path {
                                if matches!(e.kind, Kind::FileIn | Kind::FileOut) {
                                    let label = if matches!(e.preview, Preview::Video) {
                                        "▶ Открыть в плеере"
                                    } else {
                                        "Открыть"
                                    };
                                    if ui.small_button(label).clicked() {
                                        open = Some(p.clone());
                                    }
                                }
                            }
                        });
                        ui.separator();
                    }
                    if let Some(t) = copy {
                        ui.ctx().copy_text(t);
                    }
                    if let Some(p) = open {
                        open_path(&p);
                    }
                });

            // Подсветка зоны при перетаскивании файлов в окно.
            if files_hovering {
                let rect = ui.clip_rect();
                let painter = ui.painter();
                painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(160));
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "📥 Отпустите файлы — отправлю собеседнику",
                    egui::FontId::proportional(22.0),
                    egui::Color32::WHITE,
                );
            }
        });
    }
}

/// Папка по умолчанию для принятых файлов: ~/LChat-received.
fn default_downloads() -> PathBuf {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    match home {
        Some(h) => PathBuf::from(h).join("LChat-received"),
        None => PathBuf::from("LChat-received"),
    }
}

/// Список локальных IPv4-адресов (без loopback), чтобы сообщить их собеседнику.
fn list_local_ips() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(ifaces) = local_ip_address::list_afinet_netifas() {
        for (_name, ip) in ifaces {
            if let std::net::IpAddr::V4(v4) = ip {
                if !v4.is_loopback() && !v4.is_link_local() {
                    let s = v4.to_string();
                    if !out.contains(&s) {
                        out.push(s);
                    }
                }
            }
        }
    }
    out
}

/// Определяет тип предпросмотра по расширению файла (для текста — читает содержимое).
fn classify_preview(path: &Path) -> Preview {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    const IMG: &[&str] = &["png", "jpg", "jpeg", "gif", "bmp", "webp", "ico", "tiff", "tif"];
    const VID: &[&str] = &["mp4", "mkv", "webm", "mov", "avi", "m4v", "wmv", "flv"];
    const TXT: &[&str] = &[
        "txt", "md", "markdown", "log", "json", "csv", "tsv", "xml", "html", "htm", "toml",
        "yaml", "yml", "ini", "cfg", "conf", "rs", "py", "c", "h", "cpp", "hpp", "cc", "js",
        "ts", "jsx", "tsx", "go", "java", "kt", "rb", "php", "sh", "bash", "zsh", "sql", "css",
        "scss", "tex", "srt", "vtt",
    ];
    if IMG.contains(&ext.as_str()) {
        return Preview::Image;
    }
    if VID.contains(&ext.as_str()) {
        return Preview::Video;
    }
    if TXT.contains(&ext.as_str()) {
        const LIMIT: usize = 64 * 1024;
        if let Ok(bytes) = std::fs::read(path) {
            let truncated = bytes.len() > LIMIT;
            let end = bytes.len().min(LIMIT);
            let body = String::from_utf8_lossy(&bytes[..end]).to_string();
            return Preview::Text { body, truncated };
        }
    }
    Preview::None
}

/// Загружает текстуру предпросмотра: для картинки — сам файл, для видео — кадр из ffmpeg.
fn load_preview_texture(
    ctx: &egui::Context,
    path: &Path,
    is_video: bool,
) -> Option<egui::TextureHandle> {
    let src = if is_video {
        video_thumb(path)?
    } else {
        path.to_path_buf()
    };
    let img = image::ImageReader::open(&src)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    let img = img.thumbnail(1024, 1024);
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
    Some(ctx.load_texture(format!("prev:{}", src.display()), color, egui::TextureOptions::LINEAR))
}

/// Извлекает кадр видео в PNG через ffmpeg (кэшируется во временной папке).
fn video_thumb(path: &Path) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    let out = std::env::temp_dir().join(format!("lchat-thumb-{:x}.png", h.finish()));
    if out.exists() {
        return Some(out);
    }
    for seek in ["1", "0"] {
        let status = std::process::Command::new("ffmpeg")
            .args(["-y", "-ss", seek, "-i"])
            .arg(path)
            .args(["-frames:v", "1", "-vf", "scale=360:-1"])
            .arg(&out)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok()?;
        if status.success() && out.exists() {
            return Some(out);
        }
    }
    None
}

/// Открывает файл или папку системным способом.
fn open_path(path: &std::path::Path) {
    let dir = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    let _ = std::fs::create_dir_all(dir);
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_by_extension() {
        assert!(matches!(classify_preview(Path::new("photo.PNG")), Preview::Image));
        assert!(matches!(classify_preview(Path::new("clip.mp4")), Preview::Video));
        assert!(matches!(classify_preview(Path::new("data.bin")), Preview::None));
    }

    #[test]
    fn text_preview_reads_content() {
        let p = std::env::temp_dir().join("lchat-classify-test.md");
        std::fs::write(&p, "# привет\nмир").unwrap();
        match classify_preview(&p) {
            Preview::Text { body, truncated } => {
                assert!(body.contains("привет"));
                assert!(!truncated);
            }
            _ => panic!("ожидался текстовый предпросмотр"),
        }
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn video_thumbnail_extracted_and_decodable() {
        use std::process::Stdio;
        let vid = std::env::temp_dir().join("lchat-vidsrc-test.mp4");
        let made = std::process::Command::new("ffmpeg")
            .args([
                "-y", "-f", "lavfi", "-i",
                "testsrc=duration=1:size=320x240:rate=10", "-pix_fmt", "yuv420p",
            ])
            .arg(&vid)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !made {
            eprintln!("ffmpeg недоступен — тест пропущен");
            return;
        }
        let thumb = video_thumb(&vid).expect("кадр видео");
        let decoded = image::ImageReader::open(&thumb).unwrap().decode().unwrap();
        assert!(decoded.width() > 0 && decoded.height() > 0);
        let _ = std::fs::remove_file(&vid);
        let _ = std::fs::remove_file(&thumb);
    }
}
