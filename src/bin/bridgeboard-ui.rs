#![cfg_attr(windows, windows_subsystem = "windows")]

use bridgeboard::core::{self, BridgeEnv, PortRow};
use eframe::egui;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::Duration;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1120.0, 680.0])
            .with_min_inner_size([860.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Bridgeboard",
        options,
        Box::new(|_cc| Ok(Box::new(BridgeboardApp::new()))),
    )
}

#[derive(Clone, Copy)]
enum ActionKind {
    Open,
    Up,
    RemoteUp,
    Down,
    Restart,
    Logs,
}

enum UiMessage {
    Refresh(Result<Vec<PortRow>, String>),
    Action(String, ActionKind, Result<String, String>),
}

struct BridgeboardApp {
    env: Result<BridgeEnv, String>,
    rows: Vec<PortRow>,
    status: String,
    log_text: String,
    refresh_pending: bool,
    tx: Sender<UiMessage>,
    rx: Receiver<UiMessage>,
}

impl BridgeboardApp {
    fn new() -> Self {
        let (tx, rx) = channel();
        let mut app = Self {
            env: BridgeEnv::discover().map_err(|err| err.to_string()),
            rows: Vec::new(),
            status: "Loading services...".into(),
            log_text: String::new(),
            refresh_pending: false,
            tx,
            rx,
        };
        app.request_refresh();
        app
    }

    fn request_refresh(&mut self) {
        if self.refresh_pending {
            return;
        }
        let Ok(env) = self.env.clone() else {
            self.status = self.env.as_ref().err().cloned().unwrap_or_default();
            return;
        };
        self.refresh_pending = true;
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = core::port_rows(&env, true).map_err(|err| err.to_string());
            let _ = tx.send(UiMessage::Refresh(result));
        });
    }

    fn run_action(&mut self, kind: ActionKind, id: &str) {
        let Ok(env) = self.env.clone() else {
            self.status = self.env.as_ref().err().cloned().unwrap_or_default();
            return;
        };
        let id = id.to_string();
        let label = match kind {
            ActionKind::Open => format!("Open {id}"),
            ActionKind::Up => format!("Up {id}"),
            ActionKind::RemoteUp => format!("Start owner {id}"),
            ActionKind::Down => format!("Down {id}"),
            ActionKind::Restart => format!("Restart {id}"),
            ActionKind::Logs => format!("Logs {id}"),
        };
        self.status = format!("{label}...");
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = match kind {
                ActionKind::Open => core::open(&env, &id),
                ActionKind::Up => core::up(&env, &id).map(|lines| lines.join("\n")),
                ActionKind::RemoteUp => core::remote_up(&env, &id).map(|lines| lines.join("\n")),
                ActionKind::Down => core::down(&env, &id).map(|lines| lines.join("\n")),
                ActionKind::Restart => core::restart(&env, &id).map(|lines| lines.join("\n")),
                ActionKind::Logs => core::log_tail(&env, &id, 160),
            }
            .map_err(|err| err.to_string());
            let _ = tx.send(UiMessage::Action(label, kind, result));
        });
    }

    fn poll_messages(&mut self, ctx: &egui::Context) {
        while let Ok(message) = self.rx.try_recv() {
            match message {
                UiMessage::Refresh(result) => {
                    self.refresh_pending = false;
                    match result {
                        Ok(rows) => {
                            self.rows = rows;
                            self.status = format!("{} service(s)", self.rows.len());
                        }
                        Err(err) => {
                            self.status = err;
                        }
                    }
                }
                UiMessage::Action(label, kind, result) => match result {
                    Ok(text) => {
                        if matches!(kind, ActionKind::Logs) {
                            self.log_text = text;
                        } else if !text.trim().is_empty() {
                            self.status = text.lines().next().unwrap_or(&label).to_string();
                        } else {
                            self.status = format!("{label} done");
                        }
                        if !matches!(kind, ActionKind::Logs) {
                            self.request_refresh();
                        }
                    }
                    Err(err) => {
                        self.status = format!("{label} failed: {err}");
                    }
                },
            }
        }
        if self.refresh_pending {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn draw_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Bridgeboard");
            ui.separator();
            if ui.button("Refresh").clicked() {
                self.request_refresh();
            }
            if self.refresh_pending {
                ui.spinner();
            }
            ui.label(&self.status);
        });
    }

    fn draw_rows(&mut self, ui: &mut egui::Ui) {
        let mut action: Option<(ActionKind, String)> = None;
        let local_machine_id = self
            .env
            .as_ref()
            .map(|env| env.machine_id.clone())
            .unwrap_or_default();
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("bridgeboard_ports")
                    .striped(true)
                    .num_columns(12)
                    .spacing([10.0, 6.0])
                    .show(ui, |ui| {
                        table_header(ui, "Port");
                        table_header(ui, "ID");
                        table_header(ui, "Owner");
                        table_header(ui, "Mode");
                        table_header(ui, "Startup");
                        table_header(ui, "Restart");
                        table_header(ui, "Desired");
                        table_header(ui, "Tunnel");
                        table_header(ui, "Status");
                        table_header(ui, "URL");
                        table_header(ui, "Notes");
                        table_header(ui, "Actions");
                        ui.end_row();

                        for row in &self.rows {
                            ui.label(row.port.to_string());
                            ui.label(&row.id);
                            ui.label(&row.owner_host);
                            ui.label(&row.service_mode);
                            ui.label(&row.startup_policy);
                            ui.label(&row.restart_policy);
                            ui.label(&row.desired_state);
                            ui.label(&row.tunnel_modes);
                            ui.label(&row.runtime_status);
                            ui.hyperlink_to(shorten(&row.url, 34), &row.url);
                            ui.label(row.notes.as_deref().unwrap_or(""));
                            ui.horizontal(|ui| {
                                if ui.small_button("Open").clicked() {
                                    action = Some((ActionKind::Open, row.id.clone()));
                                }
                                if row.source_machine != local_machine_id {
                                    if ui.small_button("Start on Owner").clicked() {
                                        action = Some((ActionKind::RemoteUp, row.id.clone()));
                                    }
                                } else {
                                    if ui.small_button("Up").clicked() {
                                        action = Some((ActionKind::Up, row.id.clone()));
                                    }
                                    if ui.small_button("Down").clicked() {
                                        action = Some((ActionKind::Down, row.id.clone()));
                                    }
                                    if ui.small_button("Restart").clicked() {
                                        action = Some((ActionKind::Restart, row.id.clone()));
                                    }
                                    if ui.small_button("Logs").clicked() {
                                        action = Some((ActionKind::Logs, row.id.clone()));
                                    }
                                }
                            });
                            ui.end_row();
                        }
                    });
            });
        if let Some((kind, id)) = action {
            self.run_action(kind, &id);
        }
    }
}

impl eframe::App for BridgeboardApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_messages(ctx);
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            self.draw_toolbar(ui);
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.rows.is_empty() && !self.refresh_pending {
                ui.label("No registered services.");
            } else {
                self.draw_rows(ui);
            }
        });
        if !self.log_text.is_empty() {
            egui::TopBottomPanel::bottom("logs")
                .resizable(true)
                .default_height(180.0)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.strong("Logs");
                        if ui.button("Clear").clicked() {
                            self.log_text.clear();
                        }
                    });
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut self.log_text)
                                .font(egui::TextStyle::Monospace)
                                .desired_rows(8)
                                .lock_focus(true),
                        );
                    });
                });
        }
    }
}

fn table_header(ui: &mut egui::Ui, text: &str) {
    ui.strong(text);
}

fn shorten(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_string();
    }
    let mut out: String = value.chars().take(width.saturating_sub(3)).collect();
    out.push_str("...");
    out
}
