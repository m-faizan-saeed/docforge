//! Open and save dialogs.
//!
//! Opening works the same everywhere: [`pick`] shows the dialog and
//! [`picked`] hands over the chosen path, so call `picked` every frame. On
//! desktop the path arrives at once. In a web page the browser's file picker
//! runs asynchronously: the chosen file is copied into the in-memory file
//! store (`docforge_core::files`) and its path arrives a frame or two later.
//!
//! Save dialogs exist only on desktop; a web page offers downloads instead
//! (see `web::download`).

use std::{cell::RefCell, collections::HashMap, path::PathBuf};

use eframe::egui::{self, Id};

/// A named group of file extensions, e.g. `("Images", &["png", "jpg"])`.
pub type Filter = (&'static str, &'static [&'static str]);

pub const IMAGES: Filter = (
    "Images",
    &["png", "jpg", "jpeg", "webp", "tif", "tiff", "bmp", "gif"],
);
pub const PDF: Filter = ("PDF", &["pdf"]);
pub const FONTS: Filter = ("Fonts", &["ttf", "otf"]);
pub const PROJECT: Filter = ("docforge project", &["json"]);

thread_local! {
    static PICKED: RefCell<HashMap<Id, PathBuf>> = RefCell::new(HashMap::new());
}

/// The file chosen in the dialog opened under `id`, once.
pub fn picked(id: Id) -> Option<PathBuf> {
    PICKED.with_borrow_mut(|p| p.remove(&id))
}

/// Hands `path` to whoever polls [`picked`]`(id)`.
pub fn deliver(id: Id, path: PathBuf) {
    PICKED.with_borrow_mut(|p| p.insert(id, path));
}

/// Shows an open-file dialog; the result arrives through [`picked`]`(id)`.
/// `near` is a current path whose folder the dialog starts in.
#[cfg(not(target_arch = "wasm32"))]
pub fn pick(_: &egui::Context, id: Id, filters: &[Filter], near: Option<&std::path::Path>) {
    let mut dialog = rfd::FileDialog::new();
    for (name, extensions) in filters {
        dialog = dialog.add_filter(*name, extensions);
    }
    if let Some(near) = near {
        dialog = dialog.set_directory(start_dir(near));
    }
    if let Some(path) = dialog.pick_file() {
        deliver(id, path);
    }
}

#[cfg(target_arch = "wasm32")]
pub fn pick(ctx: &egui::Context, id: Id, filters: &[Filter], _: Option<&std::path::Path>) {
    let accept: Vec<String> = filters
        .iter()
        .flat_map(|(_, exts)| exts.iter())
        .filter(|e| **e != "*")
        .map(|e| format!(".{e}"))
        .collect();
    let ctx = ctx.clone();
    crate::web::pick_file(&accept.join(","), move |path| {
        deliver(id, path);
        ctx.request_repaint();
    });
}

/// A save-file dialog (desktop only).
#[cfg(not(target_arch = "wasm32"))]
pub fn save(filters: &[Filter], near: Option<&std::path::Path>, name: &str) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_file_name(name);
    for (name, extensions) in filters {
        dialog = dialog.add_filter(*name, extensions);
    }
    if let Some(near) = near {
        dialog = dialog.set_directory(start_dir(near));
    }
    dialog.save_file()
}

/// The folder of `current`, or the working directory.
#[cfg(not(target_arch = "wasm32"))]
fn start_dir(current: &std::path::Path) -> PathBuf {
    current
        .parent()
        .filter(|p| p.is_dir())
        .map(std::path::Path::to_path_buf)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default()
}
