//! Visual editing of tool geometry on the preview: which settings can be
//! edited with handles, on which image, and how they map to handle shapes.
//!
//! Adding a tool means adding a case to [`editable`] and [`edit_effect`]
//! (or a sibling function for a new node kind); the handles themselves live
//! in [`crate::handles`].

use docforge_nodes::{
    Effect,
    enhance::{Crop, DetectDocument, Line, Rectangle, Watermark},
};
use eframe::egui::{Color32, Id, Ui};

use crate::handles::{self, ImageFrame};

/// Which image the handles of an Enhance step are drawn on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Frame {
    /// The image the step receives: for steps that change the geometry
    /// (crops), whose settings refer to the uncropped image.
    StepInput,
    /// The image the step produces: for overlays, so the drawn result is
    /// visible under its handles.
    StepOutput,
}

/// Whether an effect has geometry to edit on the preview, and on which image.
pub fn editable(effect: &Effect) -> Option<Frame> {
    match effect {
        Effect::DetectDocument(_) | Effect::Crop(_) => Some(Frame::StepInput),
        Effect::Line(_) | Effect::Rectangle(_) | Effect::Watermark(_) => Some(Frame::StepOutput),
        _ => None,
    }
}

/// Short instructions shown above the preview while editing.
pub fn hint(effect: &Effect) -> &'static str {
    match effect {
        Effect::DetectDocument(DetectDocument { corners: None }) => {
            "Dashed: the automatically detected page. Drag a corner to set it by hand."
        }
        Effect::DetectDocument(_) => "Drag the four page corners.",
        Effect::Crop(_) => "Drag the edges or corners to crop; drag inside to move.",
        Effect::Line(_) => "Drag the ends; drag the middle grip to move the line.",
        Effect::Rectangle(_) => "Drag the edges or corners to resize; drag inside to move.",
        Effect::Watermark(_) => "Drag the cross to move the text.",
        _ => "",
    }
}

/// Draws handles for `effect` and applies any drag to it. `detected` is the
/// automatically found page (normalised), shown while corners are automatic.
pub fn edit_effect(
    ui: &mut Ui,
    frame: &ImageFrame,
    id: Id,
    effect: &mut Effect,
    detected: Option<[[f32; 2]; 4]>,
) -> bool {
    let accent = Color32::from_rgb(80, 200, 255);
    match effect {
        Effect::DetectDocument(DetectDocument { corners }) => {
            let automatic = corners.is_none();
            let mut quad = corners.or(detected).unwrap_or([
                [0.05, 0.05],
                [0.95, 0.05],
                [0.95, 0.95],
                [0.05, 0.95],
            ]);
            // Dragging a suggested corner switches to manual corners.
            if handles::quad(ui, frame, id, &mut quad, accent, automatic) {
                *corners = Some(quad);
                return true;
            }
            false
        }
        Effect::Crop(Crop {
            left,
            top,
            right,
            bottom,
        }) => {
            let mut r = [*left, *top, 1.0 - *right, 1.0 - *bottom];
            if handles::rect(ui, frame, id, &mut r, accent, true) {
                [*left, *top, *right, *bottom] = [r[0], r[1], 1.0 - r[2], 1.0 - r[3]];
                return true;
            }
            false
        }
        Effect::Line(Line { from, to, .. }) => handles::line(ui, frame, id, from, to, accent),
        Effect::Rectangle(Rectangle { min, max, .. }) => {
            let mut r = handles::ordered([min[0], min[1], max[0], max[1]]);
            if handles::rect(ui, frame, id, &mut r, accent, false) {
                *min = [r[0], r[1]];
                *max = [r[2], r[3]];
                return true;
            }
            false
        }
        Effect::Watermark(Watermark { position, .. }) => {
            handles::point(ui, frame, id, position, accent)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_effects_are_editable_on_the_right_image() {
        for effect in Effect::catalogue() {
            let expected = match effect {
                Effect::DetectDocument(_) | Effect::Crop(_) => Some(Frame::StepInput),
                Effect::Line(_) | Effect::Rectangle(_) | Effect::Watermark(_) => {
                    Some(Frame::StepOutput)
                }
                _ => None,
            };
            assert_eq!(editable(&effect), expected, "{}", effect.name());
            assert_eq!(
                hint(&effect).is_empty(),
                expected.is_none(),
                "{}",
                effect.name()
            );
        }
    }
}
