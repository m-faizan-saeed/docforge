//! Tool #3 — Enhance Image: a configurable sequence of CamScanner-style
//! document effects and overlays.

mod document;
mod filters;
mod overlay;

use std::{borrow::Cow, sync::Arc};

use docforge_core::{EvalContext, Inputs, Node, NodeError, NodeType, PortSpec, RasterImage, Value};
use serde::{Deserialize, Serialize};

pub use document::{Quad, detect_page};
pub use overlay::{Line, Rectangle, Watermark};

use crate::runtime::RuntimeState;

pub const INPUT: &str = "image";
pub const OUTPUT: &str = "image";

const INPUTS: &[PortSpec] = &[PortSpec::image(INPUT)];
const OUTPUTS: &[PortSpec] = &[PortSpec::image(OUTPUT)];

/// Find the page in a photo and flatten its perspective ("crop to subject").
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DetectDocument {
    /// Manual corners (top-left, top-right, bottom-right, bottom-left) as
    /// fractions of the image size. Detected automatically when `None`; if
    /// detection finds no page the image passes through unchanged.
    pub corners: Option<Quad>,
}

/// Trim a uniform border (flatbed scans).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoCrop {
    /// Per-channel difference from the border colour that counts as content.
    pub tolerance: u8,
    /// Pixels kept around the content.
    pub padding: u32,
}

impl Default for AutoCrop {
    fn default() -> Self {
        Self {
            tolerance: 40,
            padding: 10,
        }
    }
}

/// Trim fixed fractions of the width/height from each side.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Crop {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rotate {
    /// Clockwise quarter turns (1 = 90°).
    pub quarter_turns: u8,
}

/// Even out lighting, whiten the paper and boost ink and colour.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MagicColor {
    /// 0..1
    pub strength: f32,
}

impl Default for MagicColor {
    fn default() -> Self {
        Self { strength: 0.6 }
    }
}

/// Unsharp mask.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sharpen {
    /// 1 = unchanged, 1.5 = moderate, 3 = strong.
    pub factor: f32,
    /// Blur radius in pixels that defines the detail size being sharpened.
    pub radius: f32,
}

impl Default for Sharpen {
    fn default() -> Self {
        Self {
            factor: 1.5,
            radius: 1.5,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BrightnessContrast {
    /// −1..1
    pub brightness: f32,
    /// −1..1
    pub contrast: f32,
}

/// Adaptive threshold to pure black and white.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BlackAndWhite {
    /// Neighbourhood size as a fraction of the shorter side.
    pub window: f32,
    /// How much darker than its surroundings a pixel must be to turn black, 0..1.
    pub sensitivity: f32,
}

impl Default for BlackAndWhite {
    fn default() -> Self {
        Self {
            window: 0.04,
            sensitivity: 0.15,
        }
    }
}

/// One processing step.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "effect", rename_all = "snake_case")]
pub enum Effect {
    DetectDocument(DetectDocument),
    AutoCrop(AutoCrop),
    Crop(Crop),
    Rotate(Rotate),
    MagicColor(MagicColor),
    Sharpen(Sharpen),
    BrightnessContrast(BrightnessContrast),
    Grayscale,
    BlackAndWhite(BlackAndWhite),
    Watermark(Watermark),
    Line(Line),
    Rectangle(Rectangle),
}

impl Effect {
    /// One default instance of every effect, in menu order.
    pub fn catalogue() -> Vec<Effect> {
        vec![
            Self::DetectDocument(Default::default()),
            Self::AutoCrop(Default::default()),
            Self::Crop(Default::default()),
            Self::Rotate(Rotate { quarter_turns: 1 }),
            Self::MagicColor(Default::default()),
            Self::Sharpen(Default::default()),
            Self::BrightnessContrast(Default::default()),
            Self::Grayscale,
            Self::BlackAndWhite(Default::default()),
            Self::Watermark(Default::default()),
            Self::Line(Default::default()),
            Self::Rectangle(Default::default()),
        ]
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::DetectDocument(_) => "Crop to document",
            Self::AutoCrop(_) => "Auto-crop border",
            Self::Crop(_) => "Crop",
            Self::Rotate(_) => "Rotate",
            Self::MagicColor(_) => "Magic colour",
            Self::Sharpen(_) => "Sharpen",
            Self::BrightnessContrast(_) => "Brightness / contrast",
            Self::Grayscale => "Grayscale",
            Self::BlackAndWhite(_) => "Black & white",
            Self::Watermark(_) => "Text watermark",
            Self::Line(_) => "Line",
            Self::Rectangle(_) => "Rectangle",
        }
    }

    pub fn apply(&self, img: &RasterImage) -> Result<RasterImage, NodeError> {
        Ok(match self {
            Self::DetectDocument(p) => {
                let (w, h) = (img.width() as f32, img.height() as f32);
                let quad = match p.corners {
                    Some(c) => Some(c.map(|[x, y]| [x * w, y * h])),
                    None => detect_page(img),
                };
                quad.and_then(|q| document::warp(img, &q))
                    .unwrap_or_else(|| img.clone())
            }
            Self::AutoCrop(p) => filters::auto_crop(img, p.tolerance, p.padding),
            Self::Crop(p) => filters::crop_fractions(img, [p.left, p.top, p.right, p.bottom])?,
            Self::Rotate(p) => filters::rotate(img, p.quarter_turns),
            Self::MagicColor(p) => filters::magic_color(img, p.strength),
            Self::Sharpen(p) => filters::sharpen(img, p.factor, p.radius),
            Self::BrightnessContrast(p) => {
                filters::brightness_contrast(img, p.brightness, p.contrast)
            }
            Self::Grayscale => filters::grayscale(img),
            Self::BlackAndWhite(p) => filters::black_and_white(img, p.window, p.sensitivity),
            Self::Watermark(p) => {
                let mut out = img.clone();
                overlay::draw_watermark(&mut out, p)?;
                out
            }
            Self::Line(p) => {
                let mut out = img.clone();
                overlay::draw_line(&mut out, p);
                out
            }
            Self::Rectangle(p) => {
                let mut out = img.clone();
                overlay::draw_rectangle(&mut out, p);
                out
            }
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Step {
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    #[serde(flatten)]
    pub effect: Effect,
}

fn enabled_by_default() -> bool {
    true
}

impl From<Effect> for Step {
    fn from(effect: Effect) -> Self {
        Self {
            enabled: true,
            effect,
        }
    }
}

/// Applies its steps in order. Disabled steps are skipped.
///
/// The result of every step is cached: changing step *n* re-runs only steps
/// *n* onwards, which keeps live previews responsive. The cache costs one
/// image per enabled step and is dropped together with the node.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EnhanceImage {
    pub steps: Vec<Step>,
    #[serde(skip)]
    cache: RuntimeState<StepCache>,
}

#[derive(Clone, Debug, Default)]
struct StepCache {
    input: Option<Arc<RasterImage>>,
    /// The step as it was run and the image after it.
    done: Vec<(Step, Arc<RasterImage>)>,
}

impl Default for EnhanceImage {
    /// The classic document-scan recipe.
    fn default() -> Self {
        Self::new([
            Effect::DetectDocument(Default::default()),
            Effect::MagicColor(Default::default()),
            Effect::Sharpen(Default::default()),
        ])
    }
}

impl EnhanceImage {
    pub fn new(effects: impl IntoIterator<Item = Effect>) -> Self {
        Self {
            steps: effects.into_iter().map(Step::from).collect(),
            cache: RuntimeState::default(),
        }
    }

    /// Runs the steps on `input`, reusing every cached step whose input and
    /// settings are unchanged.
    pub fn process(
        &self,
        input: &Arc<RasterImage>,
        ctx: &EvalContext,
    ) -> Result<Arc<RasterImage>, NodeError> {
        let cache = self.cache.get();
        let reusable = match &cache.input {
            Some(prev) if Arc::ptr_eq(prev, input) => cache
                .done
                .iter()
                .zip(&self.steps)
                .take_while(|((ran, _), step)| ran == *step)
                .count(),
            _ => 0,
        };
        let mut done = cache.done;
        done.truncate(reusable);
        let mut current = done
            .last()
            .map_or_else(|| Arc::clone(input), |(_, img)| Arc::clone(img));

        for (i, step) in self.steps.iter().enumerate().skip(reusable) {
            if ctx.is_cancelled() {
                // Keep finished steps so the retry resumes from here.
                self.cache.set(StepCache {
                    input: Some(Arc::clone(input)),
                    done,
                });
                return Err(NodeError::Cancelled);
            }
            if step.enabled {
                let out = step.effect.apply(&current).map_err(|e| {
                    NodeError::Other(format!("step {} ({}): {e}", i + 1, step.effect.name()))
                })?;
                current = Arc::new(out);
            }
            done.push((step.clone(), Arc::clone(&current)));
        }
        self.cache.set(StepCache {
            input: Some(Arc::clone(input)),
            done,
        });
        Ok(current)
    }
}

impl Node for EnhanceImage {
    fn kind(&self) -> &'static str {
        Self::KIND
    }

    fn inputs(&self) -> Cow<'_, [PortSpec]> {
        Cow::Borrowed(INPUTS)
    }

    fn outputs(&self) -> Cow<'_, [PortSpec]> {
        Cow::Borrowed(OUTPUTS)
    }

    fn evaluate(&self, inputs: &Inputs, ctx: &EvalContext) -> Result<Vec<Value>, NodeError> {
        let input = inputs.image(INPUT)?;
        Ok(vec![Value::Image(self.process(input, ctx)?)])
    }

    fn inherit_state(&mut self, previous: &dyn Node) {
        if let Some(prev) = previous.downcast_ref::<EnhanceImage>() {
            self.cache = prev.cache.clone();
        }
    }
}

impl NodeType for EnhanceImage {
    const KIND: &'static str = "enhance_image";
    const DISPLAY_NAME: &'static str = "Enhance Image";
    const CATEGORY: &'static str = "Processing";
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use docforge_core::{Graph, GraphDoc};

    use super::*;

    fn sample() -> Arc<RasterImage> {
        let data = (0..120u32 * 90)
            .flat_map(|i| [(i % 200) as u8 + 30, 180, 140, 255])
            .collect();
        Arc::new(RasterImage::from_rgba8(120, 90, data).unwrap())
    }

    #[test]
    fn steps_run_in_order_and_skip_disabled() {
        let ctx = EvalContext::new();
        let mut node = EnhanceImage::new([
            Effect::Rotate(Rotate { quarter_turns: 1 }),
            Effect::Crop(Crop {
                left: 0.5,
                ..Crop::default()
            }),
        ]);
        let out = node.process(&sample(), &ctx).unwrap();
        // Rotated to 90x120, then the left half removed.
        assert_eq!(out.dimensions(), (45, 120));

        node.steps[0].enabled = false;
        assert_eq!(
            node.process(&sample(), &ctx).unwrap().dimensions(),
            (60, 90)
        );
    }

    #[test]
    fn editing_a_late_step_reuses_earlier_results() {
        let ctx = EvalContext::new();
        let input = sample();
        let mut node = EnhanceImage::new([
            Effect::MagicColor(Default::default()),
            Effect::Sharpen(Default::default()),
            Effect::Watermark(Default::default()),
        ]);
        node.process(&input, &ctx).unwrap();
        let after_first = Arc::clone(&node.cache.get().done[0].1);

        if let Effect::Watermark(w) = &mut node.steps[2].effect {
            w.text = "COPY".into();
        }
        node.process(&input, &ctx).unwrap();
        let cache = node.cache.get();
        assert!(
            Arc::ptr_eq(&cache.done[0].1, &after_first),
            "step 1 was recomputed"
        );

        // A different input invalidates everything.
        node.process(&sample(), &ctx).unwrap();
        assert!(!Arc::ptr_eq(&node.cache.get().done[0].1, &after_first));
    }

    #[test]
    fn cache_survives_parameter_edits_through_graph_sync() {
        let reg = crate::registry();
        let ctx = EvalContext::new();
        let mut doc_graph = Graph::new();
        let enhance = doc_graph.add(EnhanceImage::new([
            Effect::MagicColor(Default::default()),
            Effect::Grayscale,
        ]));
        let mut eval = Graph::from_doc(doc_graph.to_doc().unwrap(), &reg).unwrap();

        // Drive the node directly with a fixed input to read its cache.
        let input = sample();
        eval.node_as::<EnhanceImage>(enhance)
            .unwrap()
            .process(&input, &ctx)
            .unwrap();
        let first = Arc::clone(
            &eval
                .node_as::<EnhanceImage>(enhance)
                .unwrap()
                .cache
                .get()
                .done[0]
                .1,
        );

        doc_graph
            .edit_node::<EnhanceImage, _>(enhance, |n| n.steps[1].enabled = false)
            .unwrap();
        eval.sync_from_doc(&doc_graph.to_doc().unwrap(), &reg)
            .unwrap();
        let node = eval.node_as::<EnhanceImage>(enhance).unwrap();
        assert!(!node.steps[1].enabled);
        node.process(&input, &ctx).unwrap();
        assert!(Arc::ptr_eq(&node.cache.get().done[0].1, &first));
    }

    #[test]
    fn step_errors_name_the_step() {
        let node = EnhanceImage::new([
            Effect::Grayscale,
            Effect::Crop(Crop {
                left: 0.7,
                right: 0.7,
                ..Crop::default()
            }),
        ]);
        let err = node.process(&sample(), &EvalContext::new()).unwrap_err();
        assert!(err.to_string().contains("step 2 (Crop)"), "{err}");
    }

    #[test]
    fn serialises_readably_and_roundtrips() {
        let mut g = Graph::new();
        let mut node = EnhanceImage::default();
        node.steps
            .push(Effect::Watermark(Watermark::default()).into());
        node.steps[0].enabled = false;
        let id = g.add(node);

        let doc: GraphDoc = g.to_doc().unwrap();
        let steps = &doc.nodes[0].params["steps"];
        assert_eq!(steps[0]["effect"], "detect_document");
        assert_eq!(steps[0]["enabled"], false);
        assert_eq!(steps[3]["text"], "DRAFT");

        let back = Graph::from_doc(doc, &crate::registry()).unwrap();
        assert_eq!(
            back.node_as::<EnhanceImage>(id),
            g.node_as::<EnhanceImage>(id)
        );

        // Hand-written steps may omit `enabled` and any parameter.
        let node: EnhanceImage = serde_json::from_str(
            r#"{"steps":[{"effect":"sharpen","factor":2.0},{"effect":"grayscale"}]}"#,
        )
        .unwrap();
        assert_eq!(
            node.steps[0].effect,
            Effect::Sharpen(Sharpen {
                factor: 2.0,
                radius: 1.5
            })
        );
        assert!(node.steps[1].enabled);
    }

    #[test]
    fn every_effect_runs_on_a_real_sized_image() {
        let img = Arc::new(RasterImage::new(1200, 900, [200, 190, 180, 255]));
        for effect in Effect::catalogue() {
            let start = Instant::now();
            let out = effect
                .apply(&img)
                .unwrap_or_else(|e| panic!("{}: {e}", effect.name()));
            assert!(!out.is_empty(), "{}", effect.name());
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "{} too slow",
                effect.name()
            );
        }
    }

    #[test]
    fn cancellation_keeps_progress() {
        use std::sync::atomic::AtomicBool;
        let input = sample();
        let node = EnhanceImage::new([Effect::Grayscale, Effect::Grayscale]);
        let cancelled = EvalContext::new().with_cancel_flag(Arc::new(AtomicBool::new(true)));
        assert!(matches!(
            node.process(&input, &cancelled),
            Err(NodeError::Cancelled)
        ));
        assert!(node.process(&input, &EvalContext::new()).is_ok());
    }
}
