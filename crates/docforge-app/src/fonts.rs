//! Fonts for the app's symbols (arrows, icons).
//!
//! egui bundles fonts for text, and on desktop it falls back to the system's
//! fonts for anything else. Which symbols those have differs by system:
//! Windows, for example, lacks several the app uses, and a web page has no
//! system fonts at all. So every symbol in the UI comes from a bundled font:
//! egui's emoji fonts (the `monochrome_emoji_fonts` feature) plus
//! `assets/docforge-symbols.ttf`, a small subset of DejaVu Sans
//! (`assets/LICENSE-DejaVu.txt`) with the few symbols those lack.

use eframe::egui::{
    self, FontData, FontFamily,
    epaint::text::{FontInsert, FontPriority, InsertFontFamily},
};

const SYMBOLS: &[u8] = include_bytes!("../assets/docforge-symbols.ttf");

/// Adds the symbol font as a last resort before any system font.
pub fn install(ctx: &egui::Context) {
    ctx.add_font(FontInsert::new(
        "docforge-symbols",
        FontData::from_static(SYMBOLS),
        [FontFamily::Proportional, FontFamily::Monospace]
            .into_iter()
            .map(|family| InsertFontFamily {
                family,
                priority: FontPriority::Lowest,
            })
            .collect(),
    ));
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use eframe::egui::{Context, FontId, RawInput};

    use super::*;

    /// The non-ASCII characters inside string literals (comments skipped).
    fn literal_chars(source: &str) -> BTreeSet<char> {
        let mut found = BTreeSet::new();
        for line in source.lines() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            let (mut inside, mut escaped) = (false, false);
            for c in line.chars() {
                match c {
                    _ if escaped => escaped = false,
                    '\\' if inside => escaped = true,
                    '"' => inside = !inside,
                    c if inside && !c.is_ascii() => {
                        found.insert(c);
                    }
                    _ => {}
                }
            }
        }
        found
    }

    /// Without system fonts (as on the web, or a Windows that lacks them),
    /// every character the UI shows must still have a glyph.
    #[test]
    fn every_symbol_in_the_ui_comes_from_a_bundled_font() {
        let sources = [
            include_str!("app.rs"),
            include_str!("bounds.rs"),
            include_str!("canvas.rs"),
            include_str!("inspector.rs"),
            include_str!("pdf_view.rs"),
            include_str!("viewport.rs"),
        ];
        let chars: BTreeSet<char> = sources.iter().flat_map(|s| literal_chars(s)).collect();
        assert!(chars.contains(&'✥'), "the scan finds the UI's symbols");

        // A context with only egui's bundled fonts (no system fonts).
        let ctx = Context::default();
        install(&ctx);
        let mut output = ctx.run_ui(RawInput::default(), |_| {});
        output.textures_delta.clear();

        for family in [FontId::proportional(14.0), FontId::monospace(14.0)] {
            let missing: String = ctx.fonts_mut(|fonts| {
                chars
                    .iter()
                    .filter(|&&c| !fonts.has_glyph(&family, c))
                    .collect()
            });
            assert!(
                missing.is_empty(),
                "no bundled glyph for {missing:?} ({family:?}); add them to \
                 assets/docforge-symbols.ttf or use other symbols"
            );
        }
    }
}
