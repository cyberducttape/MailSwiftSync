use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use eframe::{
    egui,
    egui::{Color32, RichText, Stroke},
};

const INK: Color32 = Color32::from_rgb(23, 30, 38);
const PAPER: Color32 = Color32::from_rgb(246, 244, 238);
const MOSS: Color32 = Color32::from_rgb(33, 111, 83);
const CLAY: Color32 = Color32::from_rgb(195, 84, 54);
const MUTED: Color32 = Color32::from_rgb(109, 117, 120);

fn git(binary: &str, dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new(binary)
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

#[derive(Clone)]
struct AppConfig {
    git_binary: String,
    auto_refresh: bool,
    confirm_push: bool,
    audit_enabled: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            git_binary: "git".into(),
            auto_refresh: true,
            confirm_push: true,
            audit_enabled: true,
        }
    }
}

impl AppConfig {
    fn path() -> PathBuf {
        dirs_next::config_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("forgepad/settings.conf")
    }
    fn audit_path() -> PathBuf {
        Self::path().with_file_name("audit.log")
    }
    fn load() -> Self {
        let mut result = Self::default();
        let Ok(text) = fs::read_to_string(Self::path()) else {
            return result;
        };
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key {
                "git_binary" if !value.trim().is_empty() => result.git_binary = value.trim().into(),
                "auto_refresh" => result.auto_refresh = value.trim() == "true",
                "confirm_push" => result.confirm_push = value.trim() == "true",
                "audit_enabled" => result.audit_enabled = value.trim() == "true",
                _ => {}
            }
        }
        result
    }
    fn save(&self) -> Result<(), String> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::write(
            path,
            format!(
                "git_binary={}\nauto_refresh={}\nconfirm_push={}\naudit_enabled={}\n",
                self.git_binary, self.auto_refresh, self.confirm_push, self.audit_enabled
            ),
        )
        .map_err(|e| e.to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Change {
    code: String,
    path: String,
    staged: bool,
}

#[derive(Default)]
struct Repository {
    root: Option<PathBuf>,
    branch: String,
    changes: Vec<Change>,
    commits: Vec<String>,
    branches: Vec<String>,
    remote: String,
    sync_state: String,
}

impl Repository {
    fn load(&mut self, binary: &str, path: PathBuf) -> Result<(), String> {
        let root = git(binary, &path, &["rev-parse", "--show-toplevel"])?;
        self.root = Some(PathBuf::from(root));
        self.refresh(binary)
    }
    fn refresh(&mut self, binary: &str) -> Result<(), String> {
        let dir = self.root.as_ref().ok_or("Choose a repository first")?;
        self.branch = git(binary, dir, &["branch", "--show-current"])?;
        self.remote = git(binary, dir, &["remote", "get-url", "origin"])
            .unwrap_or_else(|_| "No origin remote".into());
        self.sync_state = match git(
            binary,
            dir,
            &["rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
        ) {
            Ok(counts) => {
                let values: Vec<_> = counts.split_whitespace().collect();
                match values.as_slice() {
                    [behind, ahead] => format!("{ahead} ahead · {behind} behind"),
                    _ => "Sync status unavailable".into(),
                }
            }
            Err(_) => "No upstream branch".into(),
        };
        self.branches = git(binary, dir, &["branch", "--format=%(refname:short)"])
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect();
        self.commits = git(
            binary,
            dir,
            &["log", "--pretty=format:%h  %s · %ar", "-n", "12"],
        )
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect();
        let status = git(binary, dir, &["status", "--porcelain"]).unwrap_or_default();
        self.changes = changes_from_porcelain(&status);
        Ok(())
    }
}

/// Turn Git's stable porcelain status into separate index and worktree entries.
/// A file modified both before and after staging is intentionally shown twice.
fn changes_from_porcelain(status: &str) -> Vec<Change> {
    let mut changes = Vec::new();
    for line in status.lines().filter(|line| line.len() >= 4) {
        let index = line.as_bytes()[0] as char;
        let worktree = line.as_bytes()[1] as char;
        let path = line[3..].to_owned();
        if index == '?' && worktree == '?' {
            changes.push(Change {
                code: "??".into(),
                path,
                staged: false,
            });
            continue;
        }
        if index != ' ' {
            changes.push(Change {
                code: index.to_string(),
                path: path.clone(),
                staged: true,
            });
        }
        if worktree != ' ' {
            changes.push(Change {
                code: worktree.to_string(),
                path,
                staged: false,
            });
        }
    }
    changes
}

enum Page {
    Workbench,
    Timeline,
    Branches,
    Settings,
}

enum Confirmation {
    Discard(String),
    UndoLastCommit,
    Push,
}

struct Forgepad {
    repo: Repository,
    page: Page,
    repo_input: String,
    commit_message: String,
    activity: Vec<String>,
    notice: String,
    last_refresh: Instant,
    inspected: Option<Change>,
    diff_text: String,
    confirmation: Option<Confirmation>,
    config: AppConfig,
}

impl Default for Forgepad {
    fn default() -> Self {
        let config = AppConfig::load();
        let mut app = Self {
            repo: Repository::default(),
            page: Page::Workbench,
            repo_input: String::new(),
            commit_message: String::new(),
            activity: vec!["Forgepad is ready. Select a local Git repository to begin.".into()],
            notice: String::new(),
            last_refresh: Instant::now(),
            inspected: None,
            diff_text: String::new(),
            confirmation: None,
            config,
        };
        if let Ok(dir) = std::env::current_dir() {
            let _ = app.open(dir);
        }
        app
    }
}

impl Forgepad {
    fn open(&mut self, dir: PathBuf) -> bool {
        match self.repo.load(&self.config.git_binary, dir.clone()) {
            Ok(()) => {
                self.repo_input = dir.display().to_string();
                self.note(format!("Opened {}", self.repo.branch));
                true
            }
            Err(e) => {
                self.notice = format!("Not a Git repository: {e}");
                false
            }
        }
    }
    fn note(&mut self, value: String) {
        self.activity.insert(0, value.clone());
        self.notice = value;
    }
    fn run(&mut self, args: &[&str], label: &str) {
        let Some(dir) = self.repo.root.clone() else {
            self.notice = "Choose a repository first.".into();
            return;
        };
        match git(&self.config.git_binary, &dir, args) {
            Ok(_) => {
                self.audit(args, "ok");
                self.note(label.into());
                let _ = self.repo.refresh(&self.config.git_binary);
            }
            Err(e) => {
                self.audit(args, "failed");
                self.notice = e
            }
        }
    }
    fn audit(&self, args: &[&str], outcome: &str) {
        if !self.config.audit_enabled {
            return;
        }
        let path = AppConfig::audit_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let command = if args.first() == Some(&"commit") {
            "git commit -m [message redacted]".into()
        } else {
            format!("git {}", args.join(" "))
        };
        let repo = self
            .repo
            .root
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "unknown".into());
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{stamp}\t{outcome}\t{repo}\t{command}");
        }
    }
    fn inspect(&mut self, change: Change) {
        let Some(dir) = self.repo.root.as_ref() else {
            return;
        };
        let args = if change.staged {
            vec!["diff", "--cached", "--", change.path.as_str()]
        } else {
            vec!["diff", "--", change.path.as_str()]
        };
        self.diff_text = git(&self.config.git_binary, dir, &args)
            .unwrap_or_else(|e| format!("Could not show diff: {e}"));
        if self.diff_text.is_empty() && change.code == "??" {
            self.diff_text =
                "This is a new, untracked file. Stage it to include it in a commit.".into();
        } else if self.diff_text.is_empty() {
            self.diff_text = "No textual diff is available for this file.".into();
        }
        self.inspected = Some(change);
    }
    fn confirmation_dialog(&mut self, ctx: &egui::Context) {
        let Some(action) = self.confirmation.as_ref() else {
            return;
        };
        let (title, explanation, action_label) = match action {
            Confirmation::Discard(path) => (
                "Discard local changes?",
                format!("This restores {path} to its last committed version. This cannot be undone from Forgepad."),
                "Discard changes",
            ),
            Confirmation::UndoLastCommit => (
                "Undo the most recent commit?",
                "The commit will be removed but its changes will remain staged, ready to revise and recommit.".into(),
                "Undo commit",
            ),
            Confirmation::Push => (
                "Push this branch?",
                format!("Forgepad will push {} to its configured origin. No force options will be used.", self.repo.branch),
                "Push branch",
            ),
        };
        let mut proceed = false;
        let mut cancel = false;
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_min_width(370.0);
                ui.label(explanation);
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    if ui
                        .add(egui::Button::new(RichText::new(action_label).color(PAPER)).fill(CLAY))
                        .clicked()
                    {
                        proceed = true;
                    }
                });
            });
        if cancel {
            self.confirmation = None;
        }
        if proceed {
            let action = self.confirmation.take().unwrap();
            match action {
                Confirmation::Discard(path) => {
                    self.run(&["restore", "--", &path], "Local changes discarded")
                }
                Confirmation::UndoLastCommit => self.run(
                    &["reset", "--soft", "HEAD~1"],
                    "Last commit undone; changes remain staged",
                ),
                Confirmation::Push => {
                    self.run(&["push", "-u", "origin", "HEAD"], "Pushed current branch")
                }
            }
        }
    }
    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("FORGEPAD").strong().size(22.0).color(INK));
            ui.label(RichText::new("a calm Git workbench").italics().color(MUTED));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("↻  Refresh").clicked()
                    && let Err(e) = self.repo.refresh(&self.config.git_binary)
                {
                    self.notice = e;
                }
                ui.label(RichText::new(&self.notice).color(MOSS));
            });
        });
        ui.add_space(8.0);
        ui.separator();
    }
    fn nav(&mut self, ui: &mut egui::Ui) {
        ui.set_min_width(178.0);
        ui.add_space(14.0);
        for (label, target) in [
            ("◈  Workbench", 0),
            ("◷  Timeline", 1),
            ("⌘  Branches", 2),
            ("⚙  Settings", 3),
        ] {
            let active = matches!(
                (&self.page, target),
                (Page::Workbench, 0)
                    | (Page::Timeline, 1)
                    | (Page::Branches, 2)
                    | (Page::Settings, 3)
            );
            if ui
                .selectable_label(active, RichText::new(label).size(15.0))
                .clicked()
            {
                self.page = match target {
                    0 => Page::Workbench,
                    1 => Page::Timeline,
                    2 => Page::Branches,
                    _ => Page::Settings,
                };
            }
        }
        ui.add_space(20.0);
        ui.separator();
        ui.add_space(10.0);
        ui.label(RichText::new("REPOSITORY").size(10.0).color(MUTED));
        ui.label(
            RichText::new(if self.repo.branch.is_empty() {
                "No repository"
            } else {
                &self.repo.branch
            })
            .strong()
            .color(MOSS),
        );
        if let Some(root) = &self.repo.root {
            ui.label(
                RichText::new(root.file_name().unwrap_or_default().to_string_lossy()).color(MUTED),
            );
        }
        if self.repo.root.is_some() {
            ui.add_space(6.0);
            ui.label(RichText::new(&self.repo.sync_state).size(11.0).color(MUTED));
        }
    }
    fn repo_picker(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Local folder");
            ui.text_edit_singleline(&mut self.repo_input);
            if ui.button("Browse…").clicked()
                && let Some(path) = rfd::FileDialog::new().pick_folder()
            {
                self.open(path);
            }
            if ui.button("Open").clicked() {
                self.open(PathBuf::from(&self.repo_input));
            }
        });
        ui.add_space(12.0);
    }
    fn workbench(&mut self, ui: &mut egui::Ui) {
        ui.heading("Workbench");
        ui.label(
            RichText::new("Shape your next commit with deliberate, small steps.").color(MUTED),
        );
        ui.add_space(14.0);
        self.repo_picker(ui);
        if self.repo.root.is_none() {
            return;
        }
        ui.columns(2, |columns| {
            columns[0].group(|ui| {
                ui.heading("Changes");
                ui.label(
                    RichText::new(format!("{} files in motion", self.repo.changes.len()))
                        .color(MUTED),
                );
                ui.add_space(8.0);
                if self.repo.changes.is_empty() {
                    ui.label("Your working tree is clean.");
                }
                for change in self.repo.changes.clone() {
                    ui.horizontal(|ui| {
                        let col = if change.staged { MOSS } else { CLAY };
                        ui.label(RichText::new(&change.code).monospace().color(col));
                        ui.label(&change.path);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Diff").clicked() {
                                self.inspect(change.clone());
                            }
                            let label = if change.staged { "Unstage" } else { "Stage" };
                            if ui.small_button(label).clicked() {
                                let path = change.path.as_str();
                                if change.staged {
                                    self.run(&["restore", "--staged", "--", path], "File unstaged");
                                } else {
                                    self.run(&["add", "--", path], "File staged");
                                }
                            }
                        });
                    });
                    if !change.staged && change.code != "??" {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .small_button(RichText::new("Discard").color(CLAY))
                                .clicked()
                            {
                                self.confirmation =
                                    Some(Confirmation::Discard(change.path.clone()));
                            }
                        });
                    }
                }
            });
            columns[1].group(|ui| {
                ui.heading("Commit note");
                ui.label(RichText::new("Only staged files will be included.").color(MUTED));
                ui.add_space(8.0);
                ui.add_sized(
                    [ui.available_width(), 86.0],
                    egui::TextEdit::multiline(&mut self.commit_message)
                        .hint_text("Describe the intent of this change…"),
                );
                ui.add_space(6.0);
                let ready = !self.commit_message.trim().is_empty()
                    && self.repo.changes.iter().any(|c| c.staged);
                if ui
                    .add_enabled(
                        ready,
                        egui::Button::new(RichText::new("Record commit  →").color(PAPER)),
                    )
                    .clicked()
                {
                    let msg = self.commit_message.trim().to_string();
                    self.run(&["commit", "-m", &msg], "Commit recorded");
                    self.commit_message.clear();
                }
                ui.add_space(14.0);
                ui.separator();
                ui.add_space(8.0);
                ui.label(RichText::new("REMOTE").size(10.0).color(MUTED));
                ui.label(RichText::new(&self.repo.remote).monospace().size(11.0));
                ui.label(RichText::new(&self.repo.sync_state).color(MUTED));
                if ui.button("Push current branch").clicked() {
                    if self.config.confirm_push {
                        self.confirmation = Some(Confirmation::Push);
                    } else {
                        self.run(&["push", "-u", "origin", "HEAD"], "Pushed current branch");
                    }
                }
            });
        });
        if let Some(change) = self.inspected.clone() {
            ui.add_space(14.0);
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.heading("Inspector");
                    ui.label(RichText::new(&change.path).monospace().color(MUTED));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Close").clicked() {
                            self.inspected = None;
                        }
                    });
                });
                ui.label(
                    RichText::new(if change.staged {
                        "Staged diff"
                    } else {
                        "Working-tree diff"
                    })
                    .size(11.0)
                    .color(MOSS),
                );
                egui::ScrollArea::vertical()
                    .max_height(190.0)
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(&self.diff_text).monospace().size(12.0))
                                .wrap(),
                        );
                    });
            });
        }
    }
    fn timeline(&mut self, ui: &mut egui::Ui) {
        ui.heading("Timeline");
        ui.label(RichText::new("Recent work on this repository.").color(MUTED));
        ui.add_space(16.0);
        for (i, commit) in self.repo.commits.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(if i == 0 { "●" } else { "│" })
                        .color(MOSS)
                        .size(18.0),
                );
                ui.label(RichText::new(commit).monospace().size(14.0));
            });
        }
        if self.repo.commits.is_empty() {
            ui.label("No commits found.");
        } else {
            ui.add_space(16.0);
            if ui
                .button(RichText::new("Undo most recent commit…").color(CLAY))
                .clicked()
            {
                self.confirmation = Some(Confirmation::UndoLastCommit);
            }
        }
    }
    fn branches(&mut self, ui: &mut egui::Ui) {
        ui.heading("Branches");
        ui.label(
            RichText::new("Move between lines of work without leaving your desk.").color(MUTED),
        );
        ui.add_space(16.0);
        for branch in self.repo.branches.clone() {
            ui.horizontal(|ui| {
                ui.label(RichText::new("⌘").color(MOSS));
                ui.label(RichText::new(&branch).strong());
                if branch == self.repo.branch {
                    ui.label(RichText::new("CURRENT").size(10.0).color(MOSS));
                } else if ui.button("Switch").clicked() {
                    self.run(&["switch", &branch], "Switched branch");
                }
            });
        }
        ui.add_space(18.0);
        ui.separator();
        ui.add_space(8.0);
        ui.label(RichText::new("RECENT ACTIVITY").size(10.0).color(MUTED));
        for item in self.activity.iter().take(5) {
            ui.label(RichText::new(item).color(MUTED));
        }
    }
    fn settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("Settings");
        ui.label(RichText::new("Local controls for predictable, private Git work.").color(MUTED));
        ui.add_space(16.0);
        ui.group(|ui| {
            ui.heading("Git execution");
            ui.label(RichText::new("Use an absolute path to pin a managed Git installation, or use ‘git’ to follow PATH.").color(MUTED));
            ui.horizontal(|ui| { ui.label("Git executable"); ui.text_edit_singleline(&mut self.config.git_binary); });
        });
        ui.add_space(10.0);
        ui.group(|ui| {
            ui.heading("Network and safety");
            ui.checkbox(&mut self.config.auto_refresh, "Refresh local repository state every 30 seconds");
            ui.checkbox(&mut self.config.confirm_push, "Confirm before every push");
            ui.label(RichText::new("Forgepad has no telemetry and only contacts remotes when you initiate a Git network operation.").size(12.0).color(MUTED));
        });
        ui.add_space(10.0);
        ui.group(|ui| {
            ui.heading("Audit trail");
            ui.checkbox(
                &mut self.config.audit_enabled,
                "Record Git operations locally",
            );
            ui.label(
                RichText::new(format!(
                    "Log: {}\nCommit messages are redacted; credentials are never stored.",
                    AppConfig::audit_path().display()
                ))
                .size(12.0)
                .color(MUTED),
            );
        });
        ui.add_space(14.0);
        if ui.button("Save settings").clicked() {
            match self.config.save() {
                Ok(()) => {
                    self.note("Settings saved".into());
                    let _ = self.repo.refresh(&self.config.git_binary);
                }
                Err(e) => self.notice = format!("Could not save settings: {e}"),
            }
        }
    }
}

impl eframe::App for Forgepad {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        if self.config.auto_refresh && self.last_refresh.elapsed().as_secs() > 30 {
            let _ = self.repo.refresh(&self.config.git_binary);
            self.last_refresh = Instant::now();
        }
        let mut visuals = egui::Visuals::light();
        visuals.panel_fill = PAPER;
        visuals.window_fill = PAPER;
        visuals.widgets.active.bg_fill = MOSS;
        visuals.widgets.hovered.bg_fill = Color32::from_rgb(226, 234, 226);
        visuals.widgets.noninteractive.bg_stroke =
            Stroke::new(1.0, Color32::from_rgb(211, 207, 196));
        ctx.set_visuals(visuals);
        egui::TopBottomPanel::top("top")
            .frame(
                egui::Frame::new()
                    .fill(PAPER)
                    .inner_margin(egui::Margin::symmetric(22, 14)),
            )
            .show(ctx, |ui| self.top_bar(ui));
        egui::SidePanel::left("nav")
            .frame(egui::Frame::new().fill(Color32::from_rgb(233, 231, 222)))
            .show(ctx, |ui| self.nav(ui));
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(PAPER)
                    .inner_margin(egui::Margin::same(26)),
            )
            .show(ctx, |ui| match self.page {
                Page::Workbench => self.workbench(ui),
                Page::Timeline => self.timeline(ui),
                Page::Branches => self.branches(ui),
                Page::Settings => self.settings(ui),
            });
        self.confirmation_dialog(ctx);
    }
}

fn main() -> eframe::Result<()> {
    eframe::run_native(
        "Forgepad",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1000.0, 680.0])
                .with_min_inner_size([760.0, 520.0]),
            ..Default::default()
        },
        Box::new(|_| Ok(Box::<Forgepad>::default())),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_preserves_both_index_and_worktree_changes() {
        assert_eq!(
            changes_from_porcelain("MM src/main.rs\n?? notes.txt\n"),
            vec![
                Change {
                    code: "M".into(),
                    path: "src/main.rs".into(),
                    staged: true
                },
                Change {
                    code: "M".into(),
                    path: "src/main.rs".into(),
                    staged: false
                },
                Change {
                    code: "??".into(),
                    path: "notes.txt".into(),
                    staged: false
                },
            ]
        );
    }
}
