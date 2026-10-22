use std::borrow::Cow;

use docforge_core::*;
use serde::{Deserialize, Serialize};

// ----- test nodes ------------------------------------------------------------

const IMAGE_OUT: &[PortSpec] = &[PortSpec::image("image")];
const IMAGE_IN: &[PortSpec] = &[PortSpec::image("image")];

/// Source: a 2x2 image of one colour.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Solid {
    color: [u8; 4],
}

impl Node for Solid {
    fn kind(&self) -> &'static str {
        Self::KIND
    }
    fn outputs(&self) -> Cow<'_, [PortSpec]> {
        Cow::Borrowed(IMAGE_OUT)
    }
    fn evaluate(&self, _: &Inputs, _: &EvalContext) -> Result<Vec<Value>, NodeError> {
        Ok(vec![RasterImage::new(2, 2, self.color).into()])
    }
}

impl NodeType for Solid {
    const KIND: &'static str = "solid";
    const DISPLAY_NAME: &'static str = "Solid";
}

#[derive(Default, Serialize, Deserialize)]
struct Invert;

impl Node for Invert {
    fn kind(&self) -> &'static str {
        Self::KIND
    }
    fn inputs(&self) -> Cow<'_, [PortSpec]> {
        Cow::Borrowed(IMAGE_IN)
    }
    fn outputs(&self) -> Cow<'_, [PortSpec]> {
        Cow::Borrowed(IMAGE_OUT)
    }
    fn evaluate(&self, inputs: &Inputs, _: &EvalContext) -> Result<Vec<Value>, NodeError> {
        let mut img = RasterImage::clone(inputs.image("image")?);
        for px in img.pixels_mut() {
            for c in &mut px[..3] {
                *c = 255 - *c;
            }
        }
        Ok(vec![img.into()])
    }
}

impl NodeType for Invert {
    const KIND: &'static str = "invert";
    const DISPLAY_NAME: &'static str = "Invert";
}

/// Multi-input: emits a 1xN strip of each input's first pixel, in order.
#[derive(Default, Serialize, Deserialize)]
struct Strip;

const STRIP_IN: &[PortSpec] = &[PortSpec::image("images").multi()];

impl Node for Strip {
    fn kind(&self) -> &'static str {
        "strip"
    }
    fn inputs(&self) -> Cow<'_, [PortSpec]> {
        Cow::Borrowed(STRIP_IN)
    }
    fn outputs(&self) -> Cow<'_, [PortSpec]> {
        Cow::Borrowed(IMAGE_OUT)
    }
    fn evaluate(&self, inputs: &Inputs, _: &EvalContext) -> Result<Vec<Value>, NodeError> {
        let images = inputs.images("images")?;
        let data = images.iter().flat_map(|i| i.pixels()[0]).collect();
        Ok(vec![
            RasterImage::from_rgba8(images.len() as u32, 1, data)?.into(),
        ])
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Panicky;

impl Node for Panicky {
    fn kind(&self) -> &'static str {
        "panicky"
    }
    fn outputs(&self) -> Cow<'_, [PortSpec]> {
        Cow::Borrowed(IMAGE_OUT)
    }
    fn evaluate(&self, _: &Inputs, _: &EvalContext) -> Result<Vec<Value>, NodeError> {
        panic!("boom")
    }
}

fn registry() -> NodeRegistry {
    let mut r = NodeRegistry::with_builtins();
    r.register::<Solid>().register::<Invert>();
    r
}

fn first_pixel(graph: &Graph, id: NodeId) -> [u8; 4] {
    graph
        .output(id, "image")
        .unwrap()
        .as_image()
        .unwrap()
        .pixels()[0]
}

// ----- tests -----------------------------------------------------------------

#[test]
fn chain_evaluates_and_caches() {
    let mut g = Graph::new();
    let src = g.add(Solid {
        color: [10, 20, 30, 255],
    });
    let inv = g.add(Invert);
    g.connect(src, "image", inv, "image").unwrap();
    let ctx = EvalContext::new();

    let report = g.evaluate(&ctx);
    assert!(report.is_ok());
    assert_eq!(report.recomputed, [src, inv]);
    assert_eq!(first_pixel(&g, inv), [245, 235, 225, 255]);

    // Nothing changed: everything served from cache, same allocation.
    let before = g.output(inv, "image").unwrap().clone();
    assert!(g.evaluate(&ctx).recomputed.is_empty());
    assert!(before.ptr_eq(g.output(inv, "image").unwrap()));

    // Editing the source recomputes it and everything downstream.
    g.edit_node::<Solid, _>(src, |s| s.color = [0, 0, 0, 255])
        .unwrap();
    assert_eq!(g.evaluate(&ctx).recomputed, [src, inv]);
    assert_eq!(first_pixel(&g, inv), [255, 255, 255, 255]);

    // Editing only the tail leaves the source cached.
    g.invalidate(inv);
    assert_eq!(g.evaluate(&ctx).recomputed, [inv]);
}

#[test]
fn evaluate_for_only_touches_ancestors() {
    let mut g = Graph::new();
    let a = g.add(Solid::default());
    let a_inv = g.add(Invert);
    let b = g.add(Solid::default());
    g.connect(a, "image", a_inv, "image").unwrap();

    let report = g.evaluate_for(&EvalContext::new(), &[a_inv]);
    assert_eq!(report.recomputed, [a, a_inv]);
    assert!(matches!(g.status(b), Some(NodeStatus::Pending)));
}

#[test]
fn connect_validates() {
    let mut g = Graph::new();
    let a = g.add(Invert);
    let b = g.add(Invert);
    g.connect(a, "image", b, "image").unwrap();

    assert!(matches!(
        g.connect(b, "image", a, "image"),
        Err(GraphError::WouldCycle)
    ));
    assert!(matches!(
        g.connect(a, "image", a, "image"),
        Err(GraphError::WouldCycle)
    ));
    assert!(matches!(
        g.connect(a, "nope", b, "image"),
        Err(GraphError::UnknownPort { dir: "output", .. })
    ));
    assert!(matches!(
        g.connect(a, "image", NodeId(99), "image"),
        Err(GraphError::UnknownNode(NodeId(99)))
    ));
}

#[test]
fn single_inputs_replace_multi_inputs_accumulate() {
    let mut g = Graph::new();
    let red = g.add(Solid {
        color: [255, 0, 0, 255],
    });
    let green = g.add(Solid {
        color: [0, 255, 0, 255],
    });
    let inv = g.add(Invert);
    let strip = g.add(Strip);

    g.connect(red, "image", inv, "image").unwrap();
    g.connect(green, "image", inv, "image").unwrap();
    assert_eq!(g.edges().iter().filter(|e| e.to.node == inv).count(), 1);

    g.connect(green, "image", strip, "images").unwrap();
    g.connect(red, "image", strip, "images").unwrap();
    g.evaluate(&EvalContext::new());
    let out = g.output(strip, "image").unwrap().as_image().unwrap();
    assert_eq!(out.pixels(), [[0, 255, 0, 255], [255, 0, 0, 255]]);
}

#[test]
fn failures_are_isolated() {
    let mut g = Graph::new();
    let dangling = g.add(Invert);
    let downstream = g.add(Invert);
    let panicky = g.add(Panicky);
    let healthy = g.add(Solid::default());
    g.connect(dangling, "image", downstream, "image").unwrap();

    let report = g.evaluate(&EvalContext::new());
    assert!(!report.is_ok());
    assert!(matches!(
        g.status(dangling),
        Some(NodeStatus::Failed(e)) if matches!(**e, NodeError::MissingInput(_))
    ));
    assert!(matches!(g.status(downstream), Some(NodeStatus::Blocked)));
    assert!(matches!(
        g.status(panicky),
        Some(NodeStatus::Failed(e)) if matches!(&**e, NodeError::Panicked(m) if m == "boom")
    ));
    assert!(g.outputs(healthy).is_some());

    // Fixing the input unblocks the chain.
    let src = g.add(Solid::default());
    g.connect(src, "image", dangling, "image").unwrap();
    g.evaluate(&EvalContext::new());
    assert!(g.outputs(downstream).is_some());
}

#[test]
fn remove_node_drops_its_edges() {
    let mut g = Graph::new();
    let a = g.add(Solid::default());
    let b = g.add(Invert);
    g.connect(a, "image", b, "image").unwrap();
    assert!(g.remove_node(a).is_some());
    assert!(g.edges().is_empty());
}

#[test]
fn json_roundtrip() {
    let mut g = Graph::new();
    let src = g.add(Solid {
        color: [1, 2, 3, 4],
    });
    let inv = g.add(Invert);
    g.connect(src, "image", inv, "image").unwrap();
    g.meta_mut(inv).unwrap().label = Some("negative".into());

    let json = g.to_json().unwrap();
    let mut back = Graph::from_json(&json, &registry()).unwrap();
    assert_eq!(back.to_json().unwrap(), json);
    assert_eq!(back.node_as::<Solid>(src).unwrap().color, [1, 2, 3, 4]);

    back.evaluate(&EvalContext::new());
    assert_eq!(first_pixel(&back, inv), [254, 253, 252, 4]);

    // New nodes never collide with loaded ids.
    assert!(back.add(Invert) > inv);
}

#[test]
fn loading_rejects_bad_documents() {
    let reg = registry();
    let unknown = r#"{"version":1,"nodes":[{"id":0,"kind":"teleporter"}]}"#;
    assert!(
        matches!(Graph::from_json(unknown, &reg), Err(GraphError::UnknownKind(k)) if k == "teleporter")
    );

    let future = r#"{"version":999}"#;
    assert!(matches!(
        Graph::from_json(future, &reg),
        Err(GraphError::UnsupportedVersion { .. })
    ));

    let cyclic = r#"{"version":1,
        "nodes":[{"id":0,"kind":"invert"},{"id":1,"kind":"invert"}],
        "edges":[{"from":{"node":0,"port":"image"},"to":{"node":1,"port":"image"}},
                 {"from":{"node":1,"port":"image"},"to":{"node":0,"port":"image"}}]}"#;
    assert!(matches!(
        Graph::from_json(cyclic, &reg),
        Err(GraphError::WouldCycle)
    ));

    // Omitted params mean defaults.
    let defaults = r#"{"version":1,"nodes":[{"id":0,"kind":"solid"}]}"#;
    assert!(Graph::from_json(defaults, &reg).is_ok());
}

fn invert_group() -> GroupNode {
    let mut inner = Graph::new();
    let i = inner.add(GroupInput::new("src", PortType::Image));
    let inv = inner.add(Invert);
    let o = inner.add(GroupOutput::new("result", PortType::Image));
    inner.connect(i, GROUP_VALUE_PORT, inv, "image").unwrap();
    inner.connect(inv, "image", o, GROUP_VALUE_PORT).unwrap();
    GroupNode::new(inner)
}

#[test]
fn groups_nest_and_persist() {
    let mut g = Graph::new();
    let src = g.add(Solid {
        color: [0, 0, 0, 255],
    });
    let group = g.add(invert_group());
    let after = g.add(Invert);

    let node = g.node(group).unwrap();
    assert_eq!(node.inputs()[0].name, "src");
    assert_eq!(node.outputs()[0].name, "result");

    g.connect(src, "image", group, "src").unwrap();
    g.connect(group, "result", after, "image").unwrap();
    let ctx = EvalContext::new();
    assert!(g.evaluate(&ctx).is_ok());
    let grouped = g.output(group, "result").unwrap().as_image().unwrap();
    assert_eq!(grouped.pixels()[0], [255, 255, 255, 255]);
    assert_eq!(first_pixel(&g, after), [0, 0, 0, 255]);

    // Upstream edits flow through the group.
    g.edit_node::<Solid, _>(src, |s| s.color = [100, 100, 100, 255])
        .unwrap();
    assert_eq!(g.evaluate(&ctx).recomputed, [src, group, after]);
    assert_eq!(first_pixel(&g, after), [100, 100, 100, 255]);

    // A group inside a group survives a save/load cycle.
    let mut outer_inner = Graph::new();
    let i = outer_inner.add(GroupInput::new("src", PortType::Image));
    let nested = outer_inner.add(invert_group());
    let o = outer_inner.add(GroupOutput::new("result", PortType::Image));
    outer_inner
        .connect(i, GROUP_VALUE_PORT, nested, "src")
        .unwrap();
    outer_inner
        .connect(nested, "result", o, GROUP_VALUE_PORT)
        .unwrap();
    let outer = g.add(GroupNode::new(outer_inner));
    g.connect(src, "image", outer, "src").unwrap();

    let json = g.to_json().unwrap();
    let mut back = Graph::from_json(&json, &registry()).unwrap();
    assert_eq!(back.to_json().unwrap(), json);
    assert!(back.evaluate(&ctx).is_ok());
    assert_eq!(
        back.output(outer, "result")
            .unwrap()
            .as_image()
            .unwrap()
            .pixels()[0],
        [155, 155, 155, 255]
    );
}

#[test]
fn group_errors_surface_with_inner_node() {
    let mut g = Graph::new();
    let mut inner = Graph::new();
    let p = inner.add(Panicky);
    let o = inner.add(GroupOutput::new("out", PortType::Image));
    inner.connect(p, "image", o, GROUP_VALUE_PORT).unwrap();
    let group = g.add(GroupNode::new(inner));

    g.evaluate(&EvalContext::new());
    assert!(matches!(
        g.status(group),
        Some(NodeStatus::Failed(e)) if matches!(**e, NodeError::Subgraph { node, .. } if node == p)
    ));
}

#[test]
fn editing_group_ports_prunes_stale_edges() {
    let mut g = Graph::new();
    let src = g.add(Solid::default());
    let group = g.add(invert_group());
    g.connect(src, "image", group, "src").unwrap();

    g.edit_node::<GroupNode, _>(group, |grp| {
        grp.edit_inner(|inner| {
            let (id, _) = inner
                .nodes()
                .find(|(_, n)| n.kind() == "group_input")
                .unwrap();
            inner
                .edit_node::<GroupInput, _>(id, |gi| gi.name = "renamed".into())
                .unwrap();
        })
    })
    .unwrap();
    assert!(g.edges().is_empty());
    assert_eq!(g.node(group).unwrap().inputs()[0].name, "renamed");
}

/// Records whether it took over a predecessor's state.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Stateful {
    level: u8,
    #[serde(skip)]
    inherited: bool,
}

impl Node for Stateful {
    fn kind(&self) -> &'static str {
        Self::KIND
    }
    fn outputs(&self) -> Cow<'_, [PortSpec]> {
        Cow::Borrowed(IMAGE_OUT)
    }
    fn evaluate(&self, _: &Inputs, _: &EvalContext) -> Result<Vec<Value>, NodeError> {
        Ok(vec![RasterImage::new(1, 1, [self.level; 4]).into()])
    }
    fn inherit_state(&mut self, previous: &dyn Node) {
        self.inherited = previous.downcast_ref::<Stateful>().is_some();
    }
}

impl NodeType for Stateful {
    const KIND: &'static str = "stateful";
    const DISPLAY_NAME: &'static str = "Stateful";
}

#[test]
fn sync_from_doc_keeps_unchanged_caches() {
    let reg = {
        let mut r = registry();
        r.register::<Stateful>();
        r
    };
    let ctx = EvalContext::new();

    // The "editor" document and the "evaluator" copy.
    let mut doc_graph = Graph::new();
    let src = doc_graph.add(Solid {
        color: [0, 0, 0, 255],
    });
    let inv = doc_graph.add(Invert);
    let other = doc_graph.add(Stateful::default());
    doc_graph.connect(src, "image", inv, "image").unwrap();
    let mut eval = Graph::from_doc(doc_graph.to_doc().unwrap(), &reg).unwrap();
    let mut first = eval.evaluate(&ctx).recomputed;
    first.sort();
    assert_eq!(first, [src, inv, other]);

    // Identical snapshot, or only layout changes: nothing recomputes.
    doc_graph.meta_mut(src).unwrap().position = [50.0, 50.0];
    eval.sync_from_doc(&doc_graph.to_doc().unwrap(), &reg)
        .unwrap();
    assert!(eval.evaluate(&ctx).recomputed.is_empty());
    assert_eq!(eval.meta(src).unwrap().position, [50.0, 50.0]);

    // A parameter edit recomputes that node and its dependents only, and the
    // rebuilt node inherits its predecessor's state.
    doc_graph
        .edit_node::<Stateful, _>(other, |s| s.level = 9)
        .unwrap();
    eval.sync_from_doc(&doc_graph.to_doc().unwrap(), &reg)
        .unwrap();
    assert!(eval.node_as::<Stateful>(other).unwrap().inherited);
    assert_eq!(eval.evaluate(&ctx).recomputed, [other]);

    // Rewiring and removal.
    let inv2 = doc_graph.add(Invert);
    doc_graph.connect(inv, "image", inv2, "image").unwrap();
    doc_graph.remove_node(other);
    eval.sync_from_doc(&doc_graph.to_doc().unwrap(), &reg)
        .unwrap();
    assert!(!eval.contains(other));
    assert_eq!(eval.evaluate(&ctx).recomputed, [inv2]);
    assert_eq!(first_pixel(&eval, inv2), [0, 0, 0, 255]);
    assert_eq!(eval.edges(), doc_graph.edges());
}

static CANCEL_NEXT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[derive(Default, Serialize, Deserialize)]
struct Cancellable;

impl Node for Cancellable {
    fn kind(&self) -> &'static str {
        "cancellable"
    }
    fn outputs(&self) -> Cow<'_, [PortSpec]> {
        Cow::Borrowed(IMAGE_OUT)
    }
    fn evaluate(&self, _: &Inputs, _: &EvalContext) -> Result<Vec<Value>, NodeError> {
        if CANCEL_NEXT.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Err(NodeError::Cancelled);
        }
        Ok(vec![RasterImage::new(1, 1, [0; 4]).into()])
    }
}

#[test]
fn cancelled_nodes_are_retried_not_cached() {
    let mut g = Graph::new();
    let c = g.add(Cancellable);
    let after = g.add(Invert);
    g.connect(c, "image", after, "image").unwrap();
    let ctx = EvalContext::new();

    CANCEL_NEXT.store(true, std::sync::atomic::Ordering::SeqCst);
    let report = g.evaluate(&ctx);
    assert!(report.cancelled && report.failed.is_empty());
    assert!(matches!(g.status(c), Some(NodeStatus::Pending)));

    let report = g.evaluate(&ctx);
    assert!(report.is_ok());
    assert_eq!(report.recomputed, [c, after]);

    // A cancel flag raised before the pass stops it before any node runs.
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    g.invalidate(c);
    let report = g.evaluate(&EvalContext::new().with_cancel_flag(flag));
    assert!(report.cancelled && report.recomputed.is_empty());
}
