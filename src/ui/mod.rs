//! Presentation primitives shared by the egui views.

mod theme;

pub(crate) use theme::ThemeColors;
#[cfg(test)]
pub(crate) use theme::contrast_ratio;
