#[allow(dead_code)] // The control-plane API is consumed by the next orchestration UI layer.
mod core;

use calamine::{Reader, open_workbook_auto};
use eframe::{
    egui,
    egui::{Color32, RichText, Stroke},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
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

#[derive(Clone, Default, Serialize, Deserialize)]
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
    sync_internaldates: bool,
    useuid: bool,
    usecache: bool,
    fastio1: bool,
    fastio2: bool,
    allowsizemismatch: bool,
    delete2: bool,
    extra_options: String,
}
#[derive(Clone)]
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
        for (enabled, flag) in [
            (self.profile.sync_internaldates, "--syncinternaldates"),
            (self.profile.useuid, "--useuid"),
            (self.profile.usecache, "--usecache"),
            (self.profile.fastio1, "--fastio1"),
            (self.profile.fastio2, "--fastio2"),
            (self.profile.allowsizemismatch, "--allowsizemismatch"),
            (self.profile.delete2, "--delete2"),
        ] {
            if enabled {
                a.push(flag.into());
            }
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
#[derive(Clone)]
struct BulkJob {
    label: String,
    form: Form,
    state: String,
}
struct App {
    form: Form,
    output: Vec<String>,
    receiver: Option<Receiver<Event>>,
    status: String,
    preview: bool,
    bulk_jobs: Vec<BulkJob>,
    bulk_open: bool,
    bulk_message: String,
    advanced_open: bool,
    store: core::StateStore,
    project_id: Option<String>,
    cockpit_open: bool,
    preflight: Vec<(String, String, bool)>,
}
impl Default for App {
    fn default() -> Self {
        let state_path = dirs_next::data_local_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("sourcecraft-imap-migrator/state.db");
        if let Some(parent) = state_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let store = core::StateStore::open(&state_path)
            .or_else(|_| core::StateStore::in_memory())
            .expect("SQLite state store must be available");
        Self {
            form: Form::load(),
            output: vec!["Ready. Start with a dry run against a test destination mailbox.".into()],
            receiver: None,
            status: "Idle".into(),
            preview: false,
            bulk_jobs: Vec::new(),
            bulk_open: false,
            bulk_message: "Import a CSV, XLS, or XLSX file to build a reviewable queue.".into(),
            advanced_open: false,
            store,
            project_id: None,
            cockpit_open: false,
            preflight: Vec::new(),
        }
    }
}
impl App {
    fn assess_plan(&mut self) {
        self.preflight = vec![
            (
                "Source endpoint".into(),
                if self.form.profile.source_host.is_empty() {
                    "Missing source server".into()
                } else {
                    self.form.profile.source_host.clone()
                },
                !self.form.profile.source_host.is_empty(),
            ),
            (
                "Destination endpoint".into(),
                if self.form.profile.destination_host.is_empty() {
                    "Missing destination server".into()
                } else {
                    self.form.profile.destination_host.clone()
                },
                !self.form.profile.destination_host.is_empty(),
            ),
            (
                "Safety mode".into(),
                if self.form.dry_run {
                    "Dry run enabled — destination will not be changed".into()
                } else {
                    "Live mode enabled — destination may be changed".into()
                },
                self.form.dry_run,
            ),
            (
                "Destructive options".into(),
                if self.form.profile.delete2 {
                    "--delete2 enabled: destination-only messages may be removed".into()
                } else {
                    "No destination deletion option selected".into()
                },
                !self.form.profile.delete2,
            ),
            (
                "Credential persistence".into(),
                "Passwords are excluded from saved profiles and the SQLite ledger".into(),
                true,
            ),
        ];
    }
    fn create_project(&mut self) {
        self.assess_plan();
        if self.form.profile.source_host.trim().is_empty()
            || self.form.profile.destination_host.trim().is_empty()
        {
            self.status = "Enter source and destination hosts before creating a project.".into();
            return;
        }
        match self.store.create_project(
            &self.form.profile.name,
            &self.form.profile.source_host,
            &self.form.profile.destination_host,
        ) {
            Ok(project) => match self.store.add_mailbox(
                &project.id,
                &self.form.profile.source_user,
                &self.form.profile.destination_user,
            ) {
                Ok(_) => {
                    self.project_id = Some(project.id);
                    self.status = "Project created; ready for preflight review".into();
                }
                Err(e) => self.status = format!("Could not create mailbox job: {e}"),
            },
            Err(e) => self.status = format!("Could not create project: {e}"),
        }
    }
    fn cockpit(&mut self, ctx: &egui::Context) {
        if !self.cockpit_open {
            return;
        }
        let mut open = self.cockpit_open;
        egui::Window::new("Migration Project Cockpit").open(&mut open).default_width(820.0).default_height(560.0).show(ctx, |ui| {
            ui.heading("Operator view"); ui.label(RichText::new("A durable migration project records phases and evidence independently of the desktop session.").color(MUTED)); ui.add_space(10.0);
            if self.project_id.is_none() && ui.button("Create project from current migration plan").clicked() { self.create_project(); }
            if let Some(id) = &self.project_id {
                match self.store.project(id) {
                    Ok(Some(project)) => {
                        ui.group(|ui| { ui.horizontal(|ui| { ui.heading(&project.name); ui.label(RichText::new(format!("ID {}", &project.id[..8])).monospace().color(MUTED)); }); ui.label(format!("{}  →  {}", project.source_endpoint, project.destination_endpoint)); });
                        ui.add_space(10.0); ui.label(RichText::new("MIGRATION PHASE").size(11.0).color(MUTED));
                        ui.horizontal_wrapped(|ui| for phase in [core::Phase::Discovery, core::Phase::Preflight, core::Phase::Pilot, core::Phase::Seed, core::Phase::CatchUp, core::Phase::FinalDelta, core::Phase::Verification, core::Phase::Complete] { let active = phase == project.phase; ui.label(RichText::new(format!("{} {phase:?}", if active { "●" } else { "○" })).strong().color(if active { TEAL } else { MUTED })); });
                        ui.add_space(10.0); if project.phase == core::Phase::Discovery && ui.button("Accept preflight review").clicked() { let _ = self.store.transition(&project.id, core::Phase::Preflight); self.status = "Phase advanced to Preflight".into(); }
                    }
                    Ok(None) => { self.project_id = None; }, Err(e) => self.status = format!("Could not read project: {e}"),
                }
            }
            ui.add_space(12.0); ui.separator(); ui.heading("Preflight assessment"); if ui.button("Refresh assessment").clicked() { self.assess_plan(); }
            egui::Grid::new("preflight").striped(true).show(ui, |ui| { ui.strong("Check"); ui.strong("Result"); ui.end_row(); for (name, detail, pass) in &self.preflight { ui.label(RichText::new(if *pass { "✓" } else { "!" }).color(if *pass { TEAL } else { ALERT })); ui.label(RichText::new(name).strong()); ui.label(detail); ui.end_row(); } });
            ui.add_space(10.0); ui.label(RichText::new("Next engine milestones: server capability negotiation, folder discovery, UIDVALIDITY-aware checkpoints, and message-level verification evidence.").size(11.0).color(MUTED));
        });
        self.cockpit_open = open;
    }
    fn job_from_values(
        values: &HashMap<String, String>,
        base: &Form,
        row: usize,
    ) -> Result<BulkJob, String> {
        let get = |key: &str| {
            values
                .get(key)
                .map(String::as_str)
                .unwrap_or("")
                .trim()
                .to_owned()
        };
        let mut form = base.clone();
        form.profile.source_host = get("source_host");
        form.profile.source_user = get("source_user");
        form.source_password = get("source_password");
        form.profile.destination_host = get("destination_host");
        form.profile.destination_user = get("destination_user");
        form.destination_password = get("destination_password");
        if let Some(value) = values.get("extra_options") {
            form.profile.extra_options = value.clone();
        }
        form.validate().map_err(|e| format!("Row {row}: {e}"))?;
        let label = values
            .get("name")
            .filter(|v| !v.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| {
                format!(
                    "Row {row}: {} → {}",
                    form.profile.source_user, form.profile.destination_user
                )
            });
        Ok(BulkJob {
            label,
            form,
            state: "Ready".into(),
        })
    }
    fn import_bulk(&mut self, path: &std::path::Path) {
        let ext = path
            .extension()
            .and_then(|x| x.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let result = if ext == "csv" {
            Self::read_csv(path, &self.form)
        } else if ext == "xls" || ext == "xlsx" {
            Self::read_sheet(path, &self.form)
        } else {
            Err("Choose a .csv, .xls, or .xlsx file.".into())
        };
        match result {
            Ok(jobs) => {
                self.bulk_message = format!(
                    "Imported {} ready jobs. Review the queue before running.",
                    jobs.len()
                );
                self.bulk_jobs = jobs;
            }
            Err(e) => self.bulk_message = e,
        }
    }
    fn read_csv(path: &std::path::Path, base: &Form) -> Result<Vec<BulkJob>, String> {
        let mut reader = csv::Reader::from_path(path).map_err(|e| e.to_string())?;
        let headers = reader
            .headers()
            .map_err(|e| e.to_string())?
            .iter()
            .map(|s| s.trim().to_ascii_lowercase())
            .collect::<Vec<_>>();
        let mut jobs = Vec::new();
        for (index, record) in reader.records().enumerate() {
            let record = record.map_err(|e| e.to_string())?;
            let values = headers
                .iter()
                .zip(record.iter())
                .map(|(h, v)| (h.clone(), v.to_owned()))
                .collect();
            jobs.push(Self::job_from_values(&values, base, index + 2)?);
        }
        if jobs.is_empty() {
            return Err("The file has no migration rows.".into());
        }
        Ok(jobs)
    }
    fn read_sheet(path: &std::path::Path, base: &Form) -> Result<Vec<BulkJob>, String> {
        let mut book = open_workbook_auto(path).map_err(|e| e.to_string())?;
        let range = book
            .worksheet_range_at(0)
            .ok_or("The workbook has no worksheets.")?
            .map_err(|e| e.to_string())?;
        let mut rows = range.rows();
        let headers = rows
            .next()
            .ok_or("The worksheet is empty.")?
            .iter()
            .map(|x| x.to_string().trim().to_ascii_lowercase())
            .collect::<Vec<_>>();
        let mut jobs = Vec::new();
        for (index, row) in rows.enumerate() {
            if row.iter().all(|cell| cell.to_string().trim().is_empty()) {
                continue;
            }
            let values = headers
                .iter()
                .zip(row.iter())
                .map(|(h, v)| (h.clone(), v.to_string()))
                .collect();
            jobs.push(Self::job_from_values(&values, base, index + 2)?);
        }
        if jobs.is_empty() {
            return Err("The worksheet has no migration rows.".into());
        }
        Ok(jobs)
    }
    fn start_bulk(&mut self) {
        if self.bulk_jobs.is_empty() {
            self.bulk_message = "Import a file before starting the queue.".into();
            return;
        }
        let jobs = self.bulk_jobs.clone();
        for job in &mut self.bulk_jobs {
            job.state = "Queued".into();
        }
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        self.status = format!("Batch validation: {} jobs", jobs.len());
        self.output.clear();
        thread::spawn(move || {
            for (index, job) in jobs.into_iter().enumerate() {
                let _ = tx.send(Event::Line(format!(
                    "══ Job {}: {} ══",
                    index + 1,
                    job.label
                )));
                let exe = job.form.profile.imapsync_path.clone();
                let output = Command::new(&exe).args(job.form.args(false)).output();
                match output {
                    Ok(result) => {
                        for line in String::from_utf8_lossy(&result.stdout).lines() {
                            let _ = tx.send(Event::Line(format!("[{}] {line}", index + 1)));
                        }
                        if !result.status.success() {
                            let _ = tx.send(Event::Line(format!(
                                "[{}] failed: {}",
                                index + 1,
                                String::from_utf8_lossy(&result.stderr).trim()
                            )));
                        }
                    }
                    Err(e) => {
                        let _ =
                            tx.send(Event::Line(format!("[{}] could not start: {e}", index + 1)));
                    }
                }
            }
            let _ = tx.send(Event::Finished(Ok(())));
        });
    }
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
    fn bulk_dialog(&mut self, ctx: &egui::Context) {
        if !self.bulk_open {
            return;
        }
        let mut open = self.bulk_open;
        egui::Window::new("Batch migration queue").open(&mut open).default_width(850.0).default_height(540.0).show(ctx, |ui| {
            ui.heading("Import → review → validate");
            ui.label(RichText::new(&self.bulk_message).color(MUTED));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Import CSV / XLSX…").clicked() && let Some(path) = rfd::FileDialog::new().add_filter("Migration lists", &["csv", "xls", "xlsx"]).pick_file() { self.import_bulk(&path); }
                if ui.button("Clear queue").clicked() { self.bulk_jobs.clear(); self.bulk_message = "Queue cleared.".into(); }
                let label = format!("Run {} dry validations", self.bulk_jobs.len());
                if ui.add_enabled(!self.running() && !self.bulk_jobs.is_empty(), egui::Button::new(RichText::new(label).color(Color32::WHITE)).fill(BLUE)).clicked() { self.start_bulk(); }
            });
            ui.add_space(10.0);
            ui.label(RichText::new("Required columns: source_host, source_user, source_password, destination_host, destination_user, destination_password. Optional: name, extra_options.").size(11.0).color(MUTED));
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Grid::new("bulk_jobs").striped(true).min_col_width(120.0).show(ui, |ui| {
                    ui.strong("#"); ui.strong("Migration"); ui.strong("Source"); ui.strong("Destination"); ui.strong("Status"); ui.end_row();
                    for (index, job) in self.bulk_jobs.iter().enumerate() { ui.label((index + 1).to_string()); ui.label(&job.label); ui.label(format!("{}\n{}", job.form.profile.source_host, job.form.profile.source_user)); ui.label(format!("{}\n{}", job.form.profile.destination_host, job.form.profile.destination_user)); ui.label(RichText::new(&job.state).color(TEAL)); ui.end_row(); }
                });
            });
            ui.add_space(8.0); ui.label(RichText::new("Imported passwords are used only for this open queue. Saving a profile never saves them.").size(11.0).color(ALERT));
        });
        self.bulk_open = open;
    }
    fn advanced_dialog(&mut self, ctx: &egui::Context) {
        if !self.advanced_open {
            return;
        }
        egui::Window::new("Advanced imapsync options").open(&mut self.advanced_open).default_width(620.0).show(ctx, |ui| {
            ui.label(RichText::new("These controls add documented imapsync flags to the preview and active run. Keep Dry run on while testing changes.").color(MUTED));
            ui.add_space(8.0);
            ui.group(|ui| { ui.heading("Reliability and metadata"); ui.checkbox(&mut self.form.profile.sync_internaldates, "Sync internal dates  (--syncinternaldates)"); ui.checkbox(&mut self.form.profile.useuid, "Use message UIDs when available  (--useuid)"); ui.checkbox(&mut self.form.profile.usecache, "Use imapsync cache  (--usecache)"); ui.checkbox(&mut self.form.profile.allowsizemismatch, "Allow message-size mismatch  (--allowsizemismatch)"); });
            ui.add_space(8.0);
            ui.group(|ui| { ui.heading("Performance"); ui.checkbox(&mut self.form.profile.fastio1, "Fast I/O for source  (--fastio1)"); ui.checkbox(&mut self.form.profile.fastio2, "Fast I/O for destination  (--fastio2)"); });
            ui.add_space(8.0);
            ui.group(|ui| { ui.heading(RichText::new("Destructive destination option").color(ALERT)); ui.checkbox(&mut self.form.profile.delete2, "Delete destination messages missing from source  (--delete2)"); ui.label(RichText::new("Use only for an intentionally exact backup after a tested dry run. This can remove destination mail.").size(11.0).color(ALERT)); });
            ui.add_space(8.0); ui.label("For any other documented flag, use the Extra imapsync options field in the migration plan. Each option is passed as separate whitespace-delimited arguments.");
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
                    if ui.button("Batch queue").clicked() {
                        self.bulk_open = true;
                    }
                    if ui.button("Advanced options").clicked() {
                        self.advanced_open = true;
                    }
                    if ui.button("Project cockpit").clicked() {
                        self.cockpit_open = true;
                        self.assess_plan();
                    }
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
        self.bulk_dialog(ctx);
        self.advanced_dialog(ctx);
        self.cockpit(ctx);
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
