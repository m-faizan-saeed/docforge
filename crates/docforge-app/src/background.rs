//! Work that must not hold up the UI: evaluation, PDF rendering, thumbnails.
//!
//! On desktop each [`Background`] owns a thread that handles requests as they
//! come, skipping straight to the newest when several are queued. A web page
//! (wasm32) cannot start threads, so there the same handler runs on the UI
//! thread, between frames, whenever the owner calls [`Background::poll`]. A
//! request waits one frame first, so the UI can show "Working…" before the
//! page is busy, and a newer request replaces a waiting one.

use std::sync::mpsc;

pub struct Background<Req> {
    requests: mpsc::Sender<Req>,
    #[cfg(target_arch = "wasm32")]
    inline: Inline<Req>,
}

#[cfg(target_arch = "wasm32")]
struct Inline<Req> {
    pending: mpsc::Receiver<Req>,
    /// The request to handle at the next poll.
    ready: std::cell::RefCell<Option<Req>>,
    handle: std::cell::RefCell<Box<dyn FnMut(Req)>>,
    /// Asks for another frame, so a queued request is handled soon.
    wake: Box<dyn Fn()>,
}

impl<Req: Send + 'static> Background<Req> {
    /// Handles every request with `handle`. `wake` asks the UI for a frame;
    /// handlers call it themselves once they post a result.
    pub fn spawn(
        name: &str,
        wake: impl Fn() + Send + 'static,
        handle: impl FnMut(Req) + Send + 'static,
    ) -> Self {
        let (requests, pending) = mpsc::channel::<Req>();
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = wake; // the thread runs by itself
            let mut handle = handle;
            std::thread::Builder::new()
                .name(name.to_owned())
                .spawn(move || {
                    while let Ok(mut request) = pending.recv() {
                        while let Ok(newer) = pending.try_recv() {
                            request = newer;
                        }
                        handle(request);
                    }
                })
                .unwrap_or_else(|e| panic!("failed to start the {name} thread: {e}"));
            Self { requests }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = name;
            Self {
                requests,
                inline: Inline {
                    pending,
                    ready: std::cell::RefCell::new(None),
                    handle: std::cell::RefCell::new(Box::new(handle)),
                    wake: Box::new(wake),
                },
            }
        }
    }

    pub fn send(&self, request: Req) {
        // Only fails when the worker is gone, i.e. during shutdown.
        let _ = self.requests.send(request);
        #[cfg(target_arch = "wasm32")]
        (self.inline.wake)();
    }

    /// Web only: handles the request queued before the last frame. A no-op
    /// on desktop, where the thread does this by itself.
    pub fn poll(&self) {
        #[cfg(target_arch = "wasm32")]
        {
            let inline = &self.inline;
            if let Some(newest) = inline.pending.try_iter().last() {
                *inline.ready.borrow_mut() = Some(newest);
                (inline.wake)();
                return;
            }
            let ready = inline.ready.borrow_mut().take();
            if let Some(request) = ready {
                (inline.handle.borrow_mut())(request);
            }
        }
    }
}
