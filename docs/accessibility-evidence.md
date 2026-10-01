# Desktop accessibility evidence

This record supports the [desktop accessibility release gate](release-readiness.md#desktop-accessibility-release-gate).
Automated checks run in every `make test`; the manual screen-reader passes
below must be completed by a person on each desktop platform before a stable
release. Automated results do **not** replace them.

## Automated checks (run on every build)

`src/ui/accessibility_audit.rs` renders the real desktop UI headlessly through
the same `eframe::App` entry points as the application, with AccessKit
enabled, and inspects the emitted accessibility tree.

| Check | Scenes | Test |
|---|---|---|
| Every interactive control (button, link, check box, radio button, text field, password field, combo box, slider, spin button, tab, menu item) has an accessible name from its own label or a labelled-by relation | Overview (first run, batch), Plan (generic, Google → Microsoft 365 with inline sign-in), Mailboxes, Activity (running), Verification, Settings, Projects, Credentials, Engine, Advanced, Command preview, Clear queue, Live migration, Stop migration, Delete credential | `every_interactive_control_has_an_accessible_name` |
| The same at 200% scale in the High Contrast theme | Same scenes | `controls_stay_named_at_double_scale_in_high_contrast` |
| Tab moves focus through many controls without trapping and reaches each page's primary action | Overview first run, Plan, Mailboxes, Engine dialog | `keyboard_tab_order_reaches_primary_actions_without_traps` |
| Safety confirmations are exposed as named modal dialogs, open with focus on the safe choice, and keep Tab inside the modal | Stop migration, Clear queue, Delete credential, Live migration | `confirmation_dialogs_are_named_start_safe_and_keep_focus_inside` |

The form helper enforces naming at compile time: `form_row` requires the field
closure to return the field's response, which is then labelled by the visible
caption (`LabelledField`). Modals call `name_modal` so assistive technology is
told a dialog opened and what it is about.

### Defects found and fixed by the first audit (2026-09-30)

- About 40 text fields, password fields, combo boxes, and spin buttons across
  Plan, Mailboxes, Activity, Settings, Projects, Credentials, and Advanced had
  no accessible name: visible captions were drawn but never linked.
- Modal confirmations were exposed only as unnamed generic containers; they
  are now `Dialog` nodes marked modal and labelled by their heading.
- The mailbox state filter had no visible or accessible label; it is now
  named "Mailbox state filter".

## Visual review at 200% scale and High Contrast (2026-09-30)

Every workspace page and the engine dialog were captured at 200% interface
scale in the High Contrast theme (1280×1040 window) using the debug scenes in
`src/ui/debug_scene.rs`.

| Scene | Result |
|---|---|
| Overview | **Fixed:** lifecycle step names broke mid-word ("Discover / y"); the stepper now switches to a vertical list when its columns are too narrow |
| Activity (running) | **Fixed:** stat tile text was justified across the column ("M a i l b o x e s"); tiles are now fixed-width, left-aligned, and wrap to further rows |
| Plan, Mailboxes, Verification, Engine dialog | No clipping found; content scrolls vertically |

State is never communicated by colour alone: status badges pair a glyph and
text, the maintenance-window check pairs ✓ / ! / ✕ / ○ with words, and the
lifecycle uses ✔ / ▶ / step numbers.

## Manual screen-reader passes (required before stable release)

Record one row per platform. Use a release build, a clean profile, and the
scenes listed. A failure in a safety confirmation, an unreachable control, a
missing name/role/state, or clipped content at 200% is a release blocker.

| Platform & version | Screen reader & version | Tester | Date | Result |
|---|---|---|---|---|
| Windows 11 | Narrator | | | not yet run |
| Windows 11 | NVDA | | | not yet run |
| macOS | VoiceOver | | | not yet run |
| Linux (GNOME, Wayland) | Orca | | | not yet run |

For each platform, using only the keyboard:

1. Launch, reach **Plan**, choose providers, enter users, and reach **Connect … account** and **Assess configuration**; every field announces its caption.
2. Open **Choose migration engine**; **Check installed engine** and **Install qualified imapsync** are announced and operable.
3. Import a mailbox list on **Mailboxes**; search, the state filter, row check boxes, and **Select visible** are announced; open **Review selected**.
4. Start a dry preflight; on **Activity**, the operations tiles, maintenance-window status, and the latest failure are readable.
5. Trigger each confirmation (live migration, stop, clear queue, delete credential); the dialog is announced with its heading, focus starts on the safe choice, and Tab stays inside it.
6. Repeat steps 1 and 5 at 200% scale in High Contrast; nothing is clipped or unreachable.
