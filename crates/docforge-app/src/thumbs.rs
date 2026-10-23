//! Thumbnails of the images embedded in a PDF, decoded in the background so
//! the inspector stays responsive with large scans.

use std::{
    collections::HashMap,
    sync::{Arc, mpsc},
};

use docforge_core::{PdfDocument, RasterImage};
use docforge_nodes::pdf_input::images;
use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};

/// Longest side of a thumbnail, in pixels.
const THUMB_SIDE: u32 = 192;
/// Documents whose thumbnails are kept.
const KEEP: usize = 4;

pub struct Thumb {
    pub key: String,
    pub texture: Option<TextureHandle>,
    pub error: Option<String>,
}

type Batch = (usize, Vec<(String, Result<RasterImage, String>)>);

pub struct Thumbnails {
    requests: mpsc::Sender<Arc<PdfDocument>>,
    results: mpsc::Receiver<Batch>,
    ready: HashMap<usize, Vec<Thumb>>,
    pending: Option<usize>,
    order: Vec<usize>,
    /// Keeps documents alive while their pointer serves as cache key.
    docs: HashMap<usize, Arc<PdfDocument>>,
}

impl Thumbnails {
    pub fn new(ctx: egui::Context) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<Arc<PdfDocument>>();
        let (res_tx, res_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("docforge-thumbnails".into())
            .spawn(move || {
                while let Ok(mut doc) = req_rx.recv() {
                    while let Ok(newer) = req_rx.try_recv() {
                        doc = newer;
                    }
                    let keys: Vec<String> = images::list(&doc).into_iter().map(|e| e.key).collect();
                    let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
                    let thumbs = images::extract(&doc, &refs, Some(THUMB_SIDE));
                    let batch = (
                        Arc::as_ptr(&doc) as usize,
                        keys.into_iter().zip(thumbs).collect(),
                    );
                    if res_tx.send(batch).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            })
            .expect("failed to start the thumbnail thread");
        Self {
            requests: req_tx,
            results: res_rx,
            ready: HashMap::new(),
            pending: None,
            order: Vec::new(),
            docs: HashMap::new(),
        }
    }

    pub fn is_busy(&self) -> bool {
        self.pending.is_some()
    }

    /// Thumbnails for `doc`, or `None` while they are being made.
    pub fn get(&mut self, ctx: &egui::Context, doc: &Arc<PdfDocument>) -> Option<&[Thumb]> {
        for (id, batch) in self.results.try_iter().collect::<Vec<_>>() {
            // Only for documents still held: a dropped one's address may
            // already belong to a different document.
            if !self.docs.contains_key(&id) {
                continue;
            }
            let thumbs = batch
                .into_iter()
                .map(|(key, result)| match result {
                    Ok(img) => Thumb {
                        texture: Some(ctx.load_texture(
                            format!("thumb {key}"),
                            ColorImage::from_rgba_unmultiplied(
                                [img.width() as usize, img.height() as usize],
                                img.as_raw(),
                            ),
                            TextureOptions::LINEAR,
                        )),
                        key,
                        error: None,
                    },
                    Err(error) => Thumb {
                        key,
                        texture: None,
                        error: Some(error),
                    },
                })
                .collect();
            self.ready.insert(id, thumbs);
            self.order.push(id);
            if self.pending == Some(id) {
                self.pending = None;
            }
        }
        // Forget the oldest documents.
        while self.order.len() > KEEP {
            let old = self.order.remove(0);
            self.ready.remove(&old);
            self.docs.remove(&old);
        }

        let id = Arc::as_ptr(doc) as usize;
        if !self.ready.contains_key(&id) && self.pending != Some(id) {
            if let Some(stale) = self.pending.take()
                && !self.ready.contains_key(&stale)
            {
                self.docs.remove(&stale);
            }
            self.docs.insert(id, Arc::clone(doc));
            let _ = self.requests.send(Arc::clone(doc));
            self.pending = Some(id);
        }
        self.ready.get(&id).map(Vec::as_slice)
    }
}
