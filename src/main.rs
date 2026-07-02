// LChat — локальный P2P-чат для обмена текстом и файлами по локальной сети.
// Прячем консольное окно на Windows в release-сборке.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod net;

use std::collections::HashMap;
use std::path::PathBuf;
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

struct Entry {
    kind: Kind,
    text: String,
    path: Option<PathBuf>,
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
        }
    }

    fn push(&mut self, kind: Kind, text: String, path: Option<PathBuf>) {
        self.log.push(Entry { kind, text, path });
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
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let _ = self.to_net.send(ToNet::SendFile(path.clone()));
            self.push(Kind::FileOut, format!("Отправка файла: {name}"), Some(path));
        }
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
                    for e in &self.log {
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
                        ui.horizontal(|ui| {
                            if matches!(e.kind, Kind::Me | Kind::Peer)
                                && ui.small_button("Копировать").clicked()
                            {
                                copy = Some(e.text.clone());
                            }
                            if let Some(p) = &e.path {
                                if matches!(e.kind, Kind::FileIn)
                                    && ui.small_button("Открыть файл").clicked()
                                {
                                    open = Some(p.clone());
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
