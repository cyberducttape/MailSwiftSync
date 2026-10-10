//! Typography and font coverage for the desktop UI.
//!
//! The interface is set in IBM Plex Sans (regular for body text, medium and
//! semibold for labels and headings) and IBM Plex Mono, bundled unmodified
//! under the SIL Open Font License (see `assets/fonts/OFL.txt`) so the app
//! looks the same on every platform. egui only renders the weight a family
//! contains, so the heavier weights are separate named families.
//!
//! Behind Plex come egui's own fonts (its icon glyphs) and, at startup, the
//! first readable platform font from each fallback group, for symbols such as
//! arrows and check marks and for CJK mailbox folder names that arrive from
//! real accounts. Missing glyphs would otherwise render as empty boxes.
use eframe::egui::{self, FontData, FontDefinitions, FontFamily};
use std::path::{Path, PathBuf};

const PLEX_SANS: &[u8] = include_bytes!("../../assets/fonts/IBMPlexSans-Regular.ttf");
const PLEX_SANS_MEDIUM: &[u8] = include_bytes!("../../assets/fonts/IBMPlexSans-Medium.ttf");
const PLEX_SANS_SEMIBOLD: &[u8] = include_bytes!("../../assets/fonts/IBMPlexSans-SemiBold.ttf");
const PLEX_MONO: &[u8] = include_bytes!("../../assets/fonts/IBMPlexMono-Regular.ttf");

/// Name of the medium-weight family (labels, buttons, emphasis).
pub(crate) const MEDIUM: &str = "medium";
/// Name of the semibold family (headings and titles).
pub(crate) const SEMIBOLD: &str = "semibold";

/// Name of the family for status glyphs (✓ ● ○ ⚠ …). It is egui's own font
/// stack, so every status symbol shares one visual weight; Plex's check mark
/// is a thin stroke that reads lighter than the neighbouring icons.
pub(crate) const SYMBOLS: &str = "symbols";

/// The status-glyph family.
pub(crate) fn symbols() -> FontFamily {
    FontFamily::Name(SYMBOLS.into())
}

/// The medium-weight interface family.
pub(crate) fn medium() -> FontFamily {
    FontFamily::Name(MEDIUM.into())
}

/// The semibold interface family.
pub(crate) fn semibold() -> FontFamily {
    FontFamily::Name(SEMIBOLD.into())
}

/// Refuse to load unexpectedly large files; CJK collections are ~20 MiB.
const MAX_FONT_BYTES: u64 = 64 * 1024 * 1024;

/// One fallback group: the first candidate that exists and parses is used.
struct FallbackGroup {
    name: &'static str,
    candidates: Vec<(PathBuf, u32)>,
}

fn fallback_groups() -> Vec<FallbackGroup> {
    let symbols: &[&str];
    let cjk: &[&str];
    #[cfg(target_os = "windows")]
    {
        symbols = &["seguisym.ttf", "segoeui.ttf"];
        cjk = &["msyh.ttc", "YuGothM.ttc", "meiryo.ttc", "malgun.ttf"];
    }
    #[cfg(target_os = "macos")]
    {
        symbols = &[
            "/System/Library/Fonts/Apple Symbols.ttf",
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        ];
        cjk = &[
            "/System/Library/Fonts/PingFang.ttc",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        ];
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        symbols = &[
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/share/fonts/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/noto/NotoSansSymbols2-Regular.ttf",
            "/usr/share/fonts/noto/NotoSansSymbols2-Regular.ttf",
        ];
        cjk = &[
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
            "/usr/share/fonts/wenquanyi/wqy-microhei/wqy-microhei.ttc",
        ];
    }
    let resolve = |name: &&str| -> (PathBuf, u32) { (platform_font_path(name), 0) };
    vec![
        FallbackGroup {
            name: "system-symbols",
            candidates: symbols.iter().map(resolve).collect(),
        },
        FallbackGroup {
            name: "system-cjk",
            candidates: cjk.iter().map(resolve).collect(),
        },
    ]
}

#[cfg(target_os = "windows")]
fn platform_font_path(name: &str) -> PathBuf {
    let windows = std::env::var_os("WINDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    windows.join("Fonts").join(name)
}

#[cfg(not(target_os = "windows"))]
fn platform_font_path(name: &str) -> PathBuf {
    PathBuf::from(name)
}

/// Read a font only if it is a regular file within the size bound and the
/// same parser egui uses accepts it; egui panics on unparsable font data.
fn load_font(path: &Path, index: u32) -> Option<Vec<u8>> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_FONT_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    parses(&bytes, index).then_some(bytes)
}

fn parses(bytes: &[u8], index: u32) -> bool {
    skrifa::FontRef::from_index(bytes, index).is_ok()
}

/// egui's default fonts followed by the fallback fonts found on this system.
pub(crate) fn font_definitions() -> FontDefinitions {
    font_definitions_from(
        fallback_groups()
            .into_iter()
            .filter_map(|group| {
                group.candidates.iter().find_map(|(path, index)| {
                    load_font(path, *index).map(|bytes| (group.name, bytes, *index))
                })
            })
            .collect(),
    )
}

fn font_definitions_from(fallbacks: Vec<(&'static str, Vec<u8>, u32)>) -> FontDefinitions {
    let mut definitions = FontDefinitions::default();
    for (name, bytes) in [
        ("plex-sans", PLEX_SANS),
        ("plex-sans-medium", PLEX_SANS_MEDIUM),
        ("plex-sans-semibold", PLEX_SANS_SEMIBOLD),
        ("plex-mono", PLEX_MONO),
    ] {
        definitions.font_data.insert(
            name.to_owned(),
            std::sync::Arc::new(FontData::from_static(bytes)),
        );
    }
    let egui_proportional = definitions
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let egui_monospace = definitions
        .families
        .get(&FontFamily::Monospace)
        .cloned()
        .unwrap_or_default();
    let with_leading = |leading: &str, rest: &[String]| {
        std::iter::once(leading.to_owned())
            .chain(rest.iter().cloned())
            .collect::<Vec<_>>()
    };
    definitions.families.insert(
        FontFamily::Proportional,
        with_leading("plex-sans", &egui_proportional),
    );
    definitions.families.insert(
        FontFamily::Monospace,
        with_leading("plex-mono", &egui_monospace),
    );
    definitions.families.insert(
        medium(),
        with_leading("plex-sans-medium", &egui_proportional),
    );
    definitions.families.insert(
        semibold(),
        with_leading("plex-sans-semibold", &egui_proportional),
    );
    definitions
        .families
        .insert(symbols(), egui_proportional.clone());
    for (name, bytes, index) in fallbacks {
        let mut data = FontData::from_owned(bytes);
        data.index = index;
        definitions
            .font_data
            .insert(name.to_owned(), std::sync::Arc::new(data));
        for family in [
            FontFamily::Proportional,
            FontFamily::Monospace,
            medium(),
            semibold(),
            symbols(),
        ] {
            definitions
                .families
                .entry(family)
                .or_default()
                .push(name.to_owned());
        }
    }
    definitions
}

pub(crate) fn install(ctx: &egui::Context) {
    ctx.set_fonts(font_definitions());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallbacks_follow_the_bundled_fonts() {
        let defaults = FontDefinitions::default();
        let bundled = defaults.families[&FontFamily::Proportional].clone();
        let definitions = font_definitions_from(vec![("system-symbols", Vec::new(), 0)]);
        let proportional = &definitions.families[&FontFamily::Proportional];
        // IBM Plex leads; egui's own fonts (icon glyphs) follow, then fallbacks.
        assert_eq!(proportional[0], "plex-sans");
        assert_eq!(&proportional[1..=bundled.len()], bundled.as_slice());
        assert_eq!(definitions.families[&FontFamily::Monospace][0], "plex-mono");
        assert_eq!(definitions.families[&medium()][0], "plex-sans-medium");
        assert_eq!(definitions.families[&semibold()][0], "plex-sans-semibold");
        for family in [medium(), semibold()] {
            assert_eq!(
                definitions.families[&family].last().map(String::as_str),
                Some("system-symbols")
            );
        }
        assert_eq!(
            proportional.last().map(String::as_str),
            Some("system-symbols")
        );
        assert_eq!(
            definitions.families[&FontFamily::Monospace]
                .last()
                .map(String::as_str),
            Some("system-symbols")
        );
    }

    #[test]
    fn unparsable_or_missing_fonts_are_skipped() {
        assert!(!parses(b"not a font", 0));
        assert!(load_font(Path::new("/nonexistent/font.ttf"), 0).is_none());
        let directory = std::env::temp_dir().join(format!("msync-font-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let bogus = directory.join("bogus.ttf");
        std::fs::write(&bogus, b"\x00\x01\x00\x00garbage").unwrap();
        assert!(load_font(&bogus, 0).is_none());
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn discovered_fonts_render_without_panicking() {
        // Whatever this host provides must be accepted by egui itself.
        let context = egui::Context::default();
        context.set_fonts(font_definitions());
        let _ = context.run_ui(egui::RawInput::default(), |ui| {
            // Glyphs missing from the bundled fonts, plus CJK folder names.
            let sample = "→ ✓ ● ◌ ⊘ 郵件 受信箱";
            ui.label(sample);
        });
    }
}
