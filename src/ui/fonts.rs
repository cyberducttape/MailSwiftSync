//! Font coverage for the desktop UI.
//!
//! egui's bundled fonts cover Latin text and a small icon set, but not common
//! symbols such as arrows and check marks, nor CJK mailbox folder names that
//! arrive from real accounts. Missing glyphs render as empty boxes. At
//! startup the first readable platform font from each fallback group is
//! appended behind egui's own fonts, so bundled glyphs keep priority and the
//! system font only fills gaps. Nothing is bundled into the binary.
use eframe::egui::{self, FontData, FontDefinitions, FontFamily};
use std::path::{Path, PathBuf};

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
    for (name, bytes, index) in fallbacks {
        let mut data = FontData::from_owned(bytes);
        data.index = index;
        definitions
            .font_data
            .insert(name.to_owned(), std::sync::Arc::new(data));
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
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
        assert_eq!(&proportional[..bundled.len()], bundled.as_slice());
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
            ui.label("→ ✓ ● ◌ ⊘ 郵件 受信箱");
        });
    }
}
