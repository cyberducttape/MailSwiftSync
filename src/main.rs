use eframe::{
    egui,
    egui::{Color32, RichText, Stroke},
};
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
};

const NAVY: Color32 = Color32::from_rgb(17, 26, 43);
const BLUE: Color32 = Color32::from_rgb(45, 113, 205);
const TEAL: Color32 = Color32::from_rgb(24, 158, 166);
const SKY: Color32 = Color32::from_rgb(235, 243, 252);
const MUTED: Color32 = Color32::from_rgb(103, 119, 139);
const ALERT: Color32 = Color32::from_rgb(193, 74, 61);

#[derive(Default, Serialize, Deserialize)]
struct Profile {
    name: String,
    source_host: String,
    source_user: String,
    destination_host: String,
    destination_user: String,
    imapsync_path: String,
    automap: bool,
    addheader: bool,
    justfolders: bool,
    extra_options: String,
}
struct Form {
    profile: Profile,
    source_password: String,
    destination_password: String,
    dry_run: bool,
}
impl Default for Form {
    fn default() -> Self {
        Self {
            profile: Profile {
                name: "New migration".into(),
                imapsync_path: "imapsync".into(),
                automap: true,
                ..Default::default()
            },
            source_password: String::new(),
            destination_password: String::new(),
            dry_run: true,
        }
    }
}
impl Form {
    fn path() -> std::path::PathBuf {
        dirs_next::config_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("sourcecraft-imapsync/profile.toml")
    }
    fn load() -> Self {
        let mut form = Self::default();
        if let Ok(text) = std::fs::read_to_string(Self::path())
            && let Ok(profile) = toml::from_str(&text)
        {
            form.profile = profile;
        }
        form
    }
    fn save(&self) -> Result<(), String> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(
            path,
            toml::to_string_pretty(&self.profile).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    }
    fn validate(&self) -> Result<(), String> {
        for (label, value) in [
            ("Source IMAP host", &self.profile.source_host),
            ("Source username", &self.profile.source_user),
            ("Destination IMAP host", &self.profile.destination_host),
            ("Destination username", &self.profile.destination_user),
            ("Source password", &self.source_password),
            ("Destination password", &self.destination_password),
        ] {
            if value.trim().is_empty() {
                return Err(format!("{label} is required."));
            }
        }
        Ok(())
    }
    fn args(&self, redact: bool) -> Vec<String> {
        let p1 = if redact {
            "••••••••"
        } else {
            &self.source_password
        };
        let p2 = if redact {
            "••••••••"
        } else {
            &self.destination_password
        };
        let mut a = vec![
            "--host1".into(),
            self.profile.source_host.clone(),
            "--user1".into(),
            self.profile.source_user.clone(),
            "--password1".into(),
            p1.into(),
            "--host2".into(),
            self.profile.destination_host.clone(),
            "--user2".into(),
            self.profile.destination_user.clone(),
            "--password2".into(),
            p2.into(),
        ];
        if self.profile.automap {
            a.push("--automap".into());
        }
        if self.profile.addheader {
            a.push("--addheader".into());
        }
        if self.profile.justfolders {
            a.push("--justfolders".into());
        }
        if self.dry_run {
            a.push("--dry".into());
        }
        a.extend(
            self.profile
                .extra_options
                .split_whitespace()
                .map(str::to_owned),
        );
        a
    }
}
enum Event {
    Line(String),
    Finished(Result<(), String>),
}
struct App {
    form: Form,
    output: Vec<String>,
    receiver: Option<Receiver<Event>>,
    status: String,
    preview: bool,
}
impl Default for App {
    fn default() -> Self {
        Self {
            form: Form::load(),
            output: vec!["Ready. Start with a dry run against a test destination mailbox.".into()],
            receiver: None,
            status: "Idle".into(),
            preview: false,
        }
    }
}
impl App {
    fn running(&self) -> bool {
        self.receiver.is_some()
    }
    fn start(&mut self) {
        if let Err(e) = self.form.validate() {
            self.status = e;
            return;
        }
        let exe = self.form.profile.imapsync_path.clone();
        let args = self.form.args(false);
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        self.status = if self.form.dry_run {
            "Dry run in progress".into()
        } else {
            "Sync in progress".into()
        };
        self.output = vec![format!(
            "Starting {}…",
            if self.form.dry_run {
                "safe dry run"
            } else {
                "synchronization"
            }
        )];
        thread::spawn(move || {
            let mut child = match Command::new(&exe)
                .args(&args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(Event::Finished(Err(format!("Could not start {exe}: {e}"))));
                    return;
                }
            };
            let out = child.stdout.take().expect("piped");
            let err = child.stderr.take().expect("piped");
            let a = tx.clone();
            let t1 = thread::spawn(move || {
                for l in BufReader::new(out).lines().map_while(Result::ok) {
                    let _ = a.send(Event::Line(l));
                }
            });
            let b = tx.clone();
            let t2 = thread::spawn(move || {
                for l in BufReader::new(err).lines().map_while(Result::ok) {
                    let _ = b.send(Event::Line(format!("[stderr] {l}")));
                }
            });
            let result = child.wait().map_err(|e| e.to_string()).and_then(|s| {
                if s.success() {
                    Ok(())
                } else {
                    Err(format!("imapsync exited with {s}"))
                }
            });
            let _ = t1.join();
            let _ = t2.join();
            let _ = tx.send(Event::Finished(result));
        });
    }
    fn poll(&mut self) {
        let mut done = None;
        if let Some(rx) = &self.receiver {
            while let Ok(event) = rx.try_recv() {
                match event {
                    Event::Line(s) => self.output.push(s),
                    Event::Finished(r) => done = Some(r),
                }
            }
        }
        if let Some(r) = done {
            self.status = match r {
                Ok(()) => "Completed successfully".into(),
                Err(e) => format!("Failed: {e}"),
            };
            self.receiver = None;
        }
    }
    fn account(
        ui: &mut egui::Ui,
        title: &str,
        host: &mut String,
        user: &mut String,
        password: &mut String,
        color: Color32,
    ) {
        ui.group(|ui| {
            ui.heading(RichText::new(title).color(color));
            ui.label(RichText::new("IMAP connection").size(11.0).color(MUTED));
            ui.horizontal(|ui| {
                ui.label("Server");
                ui.text_edit_singleline(host);
            });
            ui.horizontal(|ui| {
                ui.label("User");
                ui.text_edit_singleline(user);
            });
            ui.horizontal(|ui| {
                ui.label("Password");
                ui.add(egui::TextEdit::singleline(password).password(true));
            });
        });
    }
    fn preview(&mut self, ctx: &egui::Context) {
        if !self.preview {
            return;
        }
        egui::Window::new("Command preview")
            .open(&mut self.preview)
            .default_width(670.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "Passwords are redacted. They are never written to the saved profile.",
                    )
                    .color(MUTED),
                );
                let mut cmd = format!(
                    "{} {}",
                    self.form.profile.imapsync_path,
                    self.form.args(true).join(" ")
                );
                ui.add(
                    egui::TextEdit::multiline(&mut cmd)
                        .code_editor()
                        .desired_rows(10)
                        .interactive(false),
                );
            });
    }
}
impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.poll();
        let mut v = egui::Visuals::light();
        v.panel_fill = SKY;
        v.window_fill = Color32::WHITE;
        v.widgets.active.bg_fill = BLUE;
        v.widgets.hovered.bg_fill = Color32::from_rgb(215, 230, 248);
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(205, 219, 235));
        ctx.set_visuals(v);
        egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(Color32::WHITE)
                    .inner_margin(egui::Margin::symmetric(24, 15)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("SOURCECRAFT").strong().size(23.0).color(NAVY));
                    ui.label(
                        RichText::new("IMAP migration console")
                            .italics()
                            .color(MUTED),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(&self.status).color(
                            if self.status.starts_with("Failed") {
                                ALERT
                            } else {
                                BLUE
                            },
                        ));
                        ui.separator();
                        ui.label(
                            RichText::new(if self.form.dry_run {
                                "SAFE MODE"
                            } else {
                                "LIVE MODE"
                            })
                            .strong()
                            .color(if self.form.dry_run {
                                TEAL
                            } else {
                                ALERT
                            }),
                        );
                    });
                });
            });
        egui::CentralPanel::default().frame(egui::Frame::new().fill(SKY).inner_margin(egui::Margin::same(24))).show(ctx, |ui| { ui.heading("Migration plan"); ui.label(RichText::new("Configure two IMAP accounts, validate safely, then run a deliberate synchronization.").color(MUTED)); ui.add_space(14.0); ui.horizontal(|ui| { ui.label("Profile"); ui.text_edit_singleline(&mut self.form.profile.name); ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| if ui.button("Save non-secret profile").clicked() { self.status = match self.form.save() { Ok(()) => "Profile saved; passwords were not saved".into(), Err(e) => format!("Could not save profile: {e}") }; }); }); ui.add_space(10.0); ui.columns(2, |c| { Self::account(&mut c[0], "01  SOURCE", &mut self.form.profile.source_host, &mut self.form.profile.source_user, &mut self.form.source_password, BLUE); Self::account(&mut c[1], "02  DESTINATION", &mut self.form.profile.destination_host, &mut self.form.profile.destination_user, &mut self.form.destination_password, TEAL); }); ui.add_space(14.0); ui.group(|ui| { ui.heading("03  SYNC RULES"); ui.checkbox(&mut self.form.dry_run, "Dry run — validate credentials and folder mapping without modifying destination"); ui.horizontal(|ui| { ui.checkbox(&mut self.form.profile.automap, "Map standard folders automatically"); ui.checkbox(&mut self.form.profile.justfolders, "Folders only"); ui.checkbox(&mut self.form.profile.addheader, "Add Message-ID header when needed"); }); ui.horizontal(|ui| { ui.label("Extra imapsync options"); ui.text_edit_singleline(&mut self.form.profile.extra_options); }); ui.horizontal(|ui| { ui.label("imapsync executable"); ui.text_edit_singleline(&mut self.form.profile.imapsync_path); }); }); ui.add_space(14.0); ui.horizontal(|ui| { if ui.button("Preview redacted command").clicked() { self.preview = true; } let label = if self.form.dry_run { "Run dry validation  →" } else { "Run synchronization  →" }; let start_clicked = ui.add_enabled(!self.running(), egui::Button::new(RichText::new(label).color(Color32::WHITE)).fill(if self.form.dry_run { BLUE } else { ALERT })).clicked(); if start_clicked { self.start(); } else if !self.form.dry_run { ui.label(RichText::new("Live mode can add mail to the destination.").color(ALERT)); } }); ui.add_space(14.0); ui.group(|ui| { ui.horizontal(|ui| { ui.heading("Execution journal"); ui.label(RichText::new(if self.running() { "streaming output" } else { "waiting" }).color(MUTED)); }); egui::ScrollArea::vertical().stick_to_bottom(true).max_height(180.0).show(ui, |ui| for line in &self.output { ui.label(RichText::new(line).monospace().size(12.0)); }); }); ui.add_space(8.0); ui.label(RichText::new("Passwords never enter the saved profile. imapsync receives them only for the active process.").size(11.0).color(MUTED)); });
        self.preview(ctx);
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
}
fn main() -> eframe::Result<()> {
    eframe::run_native(
        "Sourcecraft IMAP Sync",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1040.0, 760.0])
                .with_min_inner_size([800.0, 620.0]),
            ..Default::default()
        },
        Box::new(|_| Ok(Box::<App>::default())),
    )
}
