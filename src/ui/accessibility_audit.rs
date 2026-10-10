//! Automated accessibility audit of the real desktop UI.
//!
//! Every workspace page and every dialog is rendered headlessly through the
//! same `eframe::App` entry points the desktop uses, with AccessKit enabled,
//! at 100% and 200% scale and in the high-contrast theme. The audit then
//! checks the emitted accessibility tree and keyboard focus traversal:
//!
//! - every interactive control (buttons, links, check boxes, radio buttons,
//!   text fields, combo boxes, sliders, spin buttons) has an accessible name,
//!   from its own label or an explicit labelled-by relation;
//! - Tab moves focus through every enabled control without trapping, and the
//!   primary action of each page is reachable by keyboard alone.
//!
//! This complements, and does not replace, the manual screen-reader passes
//! required by docs/release-readiness.md.

use crate::App;
use crate::ui::WorkspaceView;
use eframe::App as EframeApp;
use eframe::egui::{self, Context, Event, Key, Modifiers, Pos2, RawInput, Rect, accesskit};
use std::collections::{BTreeSet, HashMap};

const SCREEN: egui::Vec2 = egui::Vec2::new(1_280.0, 900.0);

struct Harness {
    app: App,
    context: Context,
    frame: eframe::Frame,
    state_directory: std::path::PathBuf,
}

impl Harness {
    fn new(scale: f32, high_contrast: bool) -> Self {
        let state_directory = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(format!("mailswiftsync-a11y-{}", uuid::Uuid::new_v4()));
        crate::credentials::ensure_private_directory(&state_directory).unwrap();
        let mut app = App::from_state_path(Some(&state_directory.join("state.db")));
        app.ui_scale = scale;
        if high_contrast {
            app.theme = crate::ui::ThemeKind::HighContrast;
        }
        let context = Context::default();
        context.enable_accesskit();
        crate::ui::install_style(&context);
        Self {
            app,
            context,
            frame: eframe::Frame::_new_kittest(),
            state_directory,
        }
    }

    fn frame(&mut self, events: Vec<Event>) -> egui::FullOutput {
        let app = &mut self.app;
        let frame = &mut self.frame;
        self.context.run_ui(
            RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SCREEN)),
                events,
                ..Default::default()
            },
            |ui| {
                EframeApp::logic(app, ui.ctx(), frame);
                EframeApp::ui(app, ui, frame);
            },
        )
    }

    /// Render until layout settles and return the full accessibility tree.
    fn tree(&mut self) -> HashMap<accesskit::NodeId, accesskit::Node> {
        self.tree_and_focus().0
    }

    fn tree_and_focus(
        &mut self,
    ) -> (
        HashMap<accesskit::NodeId, accesskit::Node>,
        Option<accesskit::NodeId>,
    ) {
        let mut nodes = HashMap::new();
        let mut focus = None;
        for _ in 0..4 {
            if let Some(update) = self.frame(Vec::new()).platform_output.accesskit_update {
                nodes = update.nodes.into_iter().collect();
                focus = Some(update.focus);
            }
        }
        (nodes, focus)
    }

    /// Press Tab `count` times and record the focused node after each press.
    fn tab_order(&mut self, count: usize) -> Vec<Option<accesskit::NodeId>> {
        let tab = || Event::Key {
            key: Key::Tab,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        (0..count)
            .map(|_| {
                self.frame(vec![tab()])
                    .platform_output
                    .accesskit_update
                    .map(|update| update.focus)
            })
            .collect()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.state_directory);
    }
}

fn is_interactive(role: accesskit::Role) -> bool {
    use accesskit::Role;
    matches!(
        role,
        Role::Button
            | Role::DefaultButton
            | Role::Link
            | Role::CheckBox
            | Role::RadioButton
            | Role::Switch
            | Role::TextInput
            | Role::MultilineTextInput
            | Role::SearchInput
            | Role::PasswordInput
            | Role::ComboBox
            | Role::Slider
            | Role::SpinButton
            | Role::Tab
            | Role::MenuItem
    )
}

/// Interactive controls with no accessible name, as `role "value"` strings.
fn unnamed_controls(nodes: &HashMap<accesskit::NodeId, accesskit::Node>) -> Vec<String> {
    let has_text = |id: &accesskit::NodeId| {
        nodes.get(id).is_some_and(|node| {
            node.label().is_some_and(|label| !label.trim().is_empty())
                || node.value().is_some_and(|value| !value.trim().is_empty())
        })
    };
    let mut unnamed = nodes
        .values()
        .filter(|node| is_interactive(node.role()) && !node.is_hidden())
        .filter(|node| {
            let own = node.label().is_some_and(|label| !label.trim().is_empty());
            let related = node.labelled_by().iter().any(has_text);
            // A button whose visible text is a child label is named by it.
            let child = matches!(
                node.role(),
                accesskit::Role::Button | accesskit::Role::Link | accesskit::Role::MenuItem
            ) && node.children().iter().any(has_text);
            !(own || related || child)
        })
        .map(|node| {
            format!(
                "{:?} value={:?} placeholder={:?}",
                node.role(),
                node.value().unwrap_or_default(),
                node.placeholder().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>();
    unnamed.sort();
    unnamed
}

fn label_of(nodes: &HashMap<accesskit::NodeId, accesskit::Node>, id: accesskit::NodeId) -> String {
    let Some(node) = nodes.get(&id) else {
        return format!("<node {id:?} not in tree>");
    };
    node.label()
        .map(str::to_owned)
        .or_else(|| {
            node.labelled_by()
                .iter()
                .chain(node.children())
                .filter_map(|child| nodes.get(child).and_then(accesskit::Node::label))
                .next()
                .map(str::to_owned)
        })
        .unwrap_or_default()
}

type Setup = fn(&mut App);

fn scenes() -> Vec<(&'static str, Setup)> {
    fn view(app: &mut App, view: WorkspaceView) {
        app.active_view = view;
    }
    vec![
        ("overview first run", |app| {
            view(app, WorkspaceView::Overview)
        }),
        ("overview batch", |app| {
            crate::ui::debug_scene::demo_data(app);
            view(app, WorkspaceView::Overview);
        }),
        ("plan generic", |app| {
            crate::ui::debug_scene::demo_data(app);
            view(app, WorkspaceView::Plan);
        }),
        ("plan google to microsoft with sign-in", |app| {
            crate::ui::debug_scene::providers_demo(app, true);
            view(app, WorkspaceView::Plan);
        }),
        ("mailboxes", |app| {
            crate::ui::debug_scene::demo_data(app);
            view(app, WorkspaceView::Mailboxes);
        }),
        ("activity running", |app| {
            crate::ui::debug_scene::telemetry_demo(app);
            view(app, WorkspaceView::Activity);
        }),
        ("verification", |app| {
            crate::ui::debug_scene::demo_data(app);
            view(app, WorkspaceView::Verification);
        }),
        ("recovery", |app| {
            crate::ui::debug_scene::demo_data(app);
            view(app, WorkspaceView::Recovery);
        }),
        ("settings dialog", |app| app.settings_open = true),
        ("projects dialog", |app| app.projects_open = true),
        ("credentials dialog", |app| app.keyring_open = true),
        ("engine dialog", |app| app.engine_open = true),
        ("advanced dialog", |app| app.advanced_open = true),
        ("command preview", |app| {
            crate::ui::debug_scene::demo_data(app);
            app.preview = true;
        }),
        ("clear queue confirmation", |app| {
            crate::ui::debug_scene::demo_data(app);
            app.bulk_clear_confirm_open = true;
        }),
        ("live migration confirmation", |app| {
            crate::ui::debug_scene::demo_data(app);
            app.live_confirm_open = true;
        }),
        ("stop confirmation", |app| {
            // The stop confirmation exists only while a run is active.
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            std::mem::forget(sender);
            app.receiver = Some(receiver);
            app.stop_confirm_open = true;
        }),
        ("credential delete confirmation", |app| {
            app.form.profile.source_credential_id = "source-entry".into();
            app.credential_delete_confirmation =
                Some(crate::ui::app_state::CredentialDeleteTarget::Password { source: true });
        }),
    ]
}

fn audit(scale: f32, high_contrast: bool) -> Vec<String> {
    let mut failures = Vec::new();
    for (name, setup) in scenes() {
        let mut harness = Harness::new(scale, high_contrast);
        setup(&mut harness.app);
        let nodes = harness.tree();
        assert!(!nodes.is_empty(), "{name}: no accessibility tree emitted");
        for control in unnamed_controls(&nodes) {
            failures.push(format!("[{name} @ {scale}x] unnamed {control}"));
        }
    }
    failures
}

#[test]
fn every_interactive_control_has_an_accessible_name() {
    let failures = audit(1.0, false);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn controls_stay_named_at_double_scale_in_high_contrast() {
    let failures = audit(2.0, true);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Tab reaches each page's primary action, never sticks on one control, and
/// in a confirmation dialog stays inside the modal.
#[test]
fn keyboard_tab_order_reaches_primary_actions_without_traps() {
    let cases: [(&str, Setup, &str); 4] = [
        (
            "overview first run",
            |app| app.active_view = WorkspaceView::Overview,
            "Configure first mailbox",
        ),
        (
            "plan",
            |app| {
                crate::ui::debug_scene::demo_data(app);
                app.active_view = WorkspaceView::Plan;
            },
            "Assess configuration",
        ),
        (
            "mailboxes",
            |app| {
                crate::ui::debug_scene::demo_data(app);
                app.active_view = WorkspaceView::Mailboxes;
            },
            "Select visible",
        ),
        (
            "engine dialog",
            |app| app.engine_open = true,
            "Continue to migration plan",
        ),
    ];
    for (name, setup, primary) in cases {
        let mut harness = Harness::new(1.0, false);
        setup(&mut harness.app);
        let nodes = harness.tree();
        let order = harness.tab_order(400);
        let focused = order.iter().flatten().copied().collect::<Vec<_>>();
        assert!(!focused.is_empty(), "{name}: Tab never moved focus");
        let distinct = focused.iter().collect::<BTreeSet<_>>();
        assert!(
            distinct.len() > 3,
            "{name}: Tab is trapped on {} control(s)",
            distinct.len()
        );
        let labels = focused
            .iter()
            .map(|id| label_of(&nodes, *id))
            .collect::<Vec<_>>();
        assert!(
            labels.iter().any(|label| label.contains(primary)),
            "{name}: Tab never reached \"{primary}\"; reached {:?}",
            labels.iter().collect::<BTreeSet<_>>()
        );
    }
}

/// Focus on the root window node means "no widget focused" in AccessKit.
fn widget_focus(
    nodes: &HashMap<accesskit::NodeId, accesskit::Node>,
    focus: Option<accesskit::NodeId>,
) -> Option<accesskit::NodeId> {
    focus.filter(|id| {
        nodes
            .get(id)
            .is_some_and(|node| node.role() != accesskit::Role::Window)
    })
}

/// Safety confirmations are announced as named modal dialogs, open with
/// focus on the safe choice, and keep Tab inside the modal so keyboard users
/// cannot act on the page behind it.
#[test]
fn confirmation_dialogs_are_named_start_safe_and_keep_focus_inside() {
    let cases: [(&str, Setup, &str, &[&str]); 4] = [
        (
            "stop confirmation",
            |app| {
                // The stop confirmation exists only while a run is active.
                let (sender, receiver) = std::sync::mpsc::sync_channel(1);
                std::mem::forget(sender);
                app.receiver = Some(receiver);
                app.stop_confirm_open = true;
            },
            "Keep running",
            &["Keep running", "Stop migration"],
        ),
        (
            "clear queue confirmation",
            |app| {
                crate::ui::debug_scene::demo_data(app);
                app.bulk_clear_confirm_open = true;
            },
            "Keep queue",
            &["Keep queue", "Clear"],
        ),
        (
            "credential delete confirmation",
            |app| {
                app.form.profile.source_credential_id = "source-entry".into();
                app.credential_delete_confirmation =
                    Some(crate::ui::app_state::CredentialDeleteTarget::Password { source: true });
            },
            "Cancel",
            &["Cancel", "Delete"],
        ),
        (
            "live migration confirmation",
            |app| {
                crate::ui::debug_scene::demo_data(app);
                app.live_confirm_open = true;
            },
            "Cancel",
            &["Cancel", "Start", "I understand", "confirm"],
        ),
    ];
    for (name, setup, safe_choice, modal_words) in cases {
        let mut harness = Harness::new(1.0, false);
        setup(&mut harness.app);
        let (nodes, focus) = harness.tree_and_focus();
        let dialog = nodes
            .values()
            .find(|node| node.role() == accesskit::Role::Dialog && node.is_modal());
        let dialog_name = dialog
            .and_then(|node| node.labelled_by().first())
            .and_then(|id| nodes.get(id))
            .and_then(|node| node.label().or_else(|| node.value()))
            .unwrap_or_default();
        assert!(
            !dialog_name.trim().is_empty(),
            "{name}: no named modal dialog in the accessibility tree"
        );
        let initial = widget_focus(&nodes, focus)
            .map(|id| label_of(&nodes, id))
            .unwrap_or_default();
        assert!(
            initial.contains(safe_choice),
            "{name}: opened with focus on {initial:?}, not the safe choice {safe_choice:?}"
        );
        let focused = harness
            .tab_order(12)
            .into_iter()
            .filter_map(|focus| widget_focus(&nodes, focus))
            .map(|id| label_of(&nodes, id))
            .collect::<Vec<_>>();
        assert!(!focused.is_empty(), "{name}: Tab never focused a control");
        for label in &focused {
            assert!(
                modal_words.iter().any(|word| label.contains(word)),
                "{name}: Tab left the modal and focused {label:?} (sequence {focused:?})"
            );
        }
    }
}

#[test]
fn failed_preflight_remediation_opens_the_plan_from_the_keyboard() {
    let mut harness = Harness::new(1.0, false);
    harness.app.preflight = vec![(
        "Destination endpoint".into(),
        "Missing destination server".into(),
        false,
    )];
    harness.app.active_view = WorkspaceView::Overview;
    let nodes = harness.tree();
    let action_id = nodes
        .iter()
        .find(|(_, node)| {
            node.role() == accesskit::Role::Button
                && node.label().is_some_and(|label| label == "Fix in Plan")
        })
        .map(|(id, _)| *id)
        .expect("failed preflight should expose a named Plan remediation button");
    let tab = || Event::Key {
        key: Key::Tab,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    };
    let mut focused_action = false;
    for _ in 0..128 {
        let focus = harness
            .frame(vec![tab()])
            .platform_output
            .accesskit_update
            .map(|update| update.focus);
        if focus == Some(action_id) {
            focused_action = true;
            break;
        }
    }
    assert!(focused_action, "Fix in Plan should be keyboard reachable");
    harness.frame(vec![Event::Key {
        key: Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    }]);
    assert!(matches!(harness.app.active_view, WorkspaceView::Plan));
}

#[test]
fn live_batch_confirmation_shows_cutover_readiness() {
    let mut harness = Harness::new(1.0, false);
    crate::ui::debug_scene::demo_data(&mut harness.app);
    harness.app.select_all_bulk_rows();
    harness.app.bulk_mode = crate::controller::BatchExecutionMode::Live;
    harness.app.bulk_live_confirm_open = true;
    let nodes = harness.tree();
    let text = nodes
        .values()
        .filter_map(|node| node.label().or_else(|| node.value()))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("Outstanding in this queue:"),
        "the confirmation does not report outstanding queue review:\n{text}"
    );
    assert!(
        text.contains("Destination capacity is not checked per mailbox"),
        "the confirmation does not state the capacity boundary:\n{text}"
    );
}
