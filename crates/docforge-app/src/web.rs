//! Browser-only plumbing: the start-up entry point's helpers, the file
//! picker, downloads and opening a project from a URL.
//!
//! Files the user opens are copied into the in-memory file store
//! ([`docforge_core::files`]) under [`UPLOADS`], where nodes find them by
//! path like on desktop.

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
};

use docforge_core::{GraphDoc, files};
use wasm_bindgen::{JsCast, prelude::*};
use wasm_bindgen_futures::{JsFuture, spawn_local};

thread_local! {
    static REPORTS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static DROPPED: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
}

/// Stores a file dropped on the page once its contents are read; its path
/// is then returned by [`dropped`].
pub fn read_dropped(
    file: std::sync::Arc<dyn eframe::egui::DroppedFile + Send + Sync>,
    ctx: eframe::egui::Context,
) {
    spawn_local(async move {
        let name = file.path().to_string_lossy().into_owned();
        match file.bytes_async().await {
            Ok(bytes) => {
                let path = upload_path(&name);
                files::insert(&path, bytes);
                DROPPED.with_borrow_mut(|d| d.push(path));
            }
            Err(err) => report(format!("Could not read {name}: {err}")),
        }
        ctx.request_repaint();
    });
}

/// Paths of the dropped files read since the last call.
pub fn dropped() -> Vec<PathBuf> {
    DROPPED.with_borrow_mut(std::mem::take)
}

/// Queues an error for the status bar (from asynchronous work).
pub fn report(message: String) {
    log::warn!("{message}");
    REPORTS.with_borrow_mut(|r| r.push(message));
}

/// Errors queued by [`report`] since the last call.
pub fn reports() -> Vec<String> {
    REPORTS.with_borrow_mut(std::mem::take)
}

/// Folder of the in-memory store that opened and dropped files go to.
pub const UPLOADS: &str = "/browser";

fn window() -> web_sys::Window {
    web_sys::window().expect("running in a browser window")
}

/// Where a file the user opened is kept.
pub fn upload_path(name: &str) -> PathBuf {
    Path::new(UPLOADS).join(name)
}

async fn bytes_of(promise: js_sys::Promise) -> Result<Vec<u8>, JsValue> {
    let buffer = JsFuture::from(promise).await?;
    Ok(js_sys::Uint8Array::new(&buffer).to_vec())
}

/// Opens the browser's file picker. The chosen file is stored and its path
/// passed to `done` (not called if the user cancels).
pub fn pick_file(accept: &str, done: impl FnOnce(PathBuf) + 'static) {
    let Some(document) = window().document() else {
        return;
    };
    let Ok(input) = document
        .create_element("input")
        .map(|e| e.unchecked_into::<web_sys::HtmlInputElement>())
    else {
        return;
    };
    input.set_type("file");
    if !accept.is_empty() {
        input.set_accept(accept);
    }
    let _ = input.style().set_property("display", "none");
    if let Some(body) = document.body() {
        let _ = body.append_child(&input);
    }

    let done = Rc::new(RefCell::new(Some(done)));
    let on_change = {
        let input = input.clone();
        Closure::<dyn FnMut()>::new(move || {
            let file = input.files().and_then(|f| f.get(0));
            input.remove();
            let (Some(file), Some(done)) = (file, done.borrow_mut().take()) else {
                return;
            };
            spawn_local(async move {
                if let Ok(bytes) = bytes_of(file.array_buffer()).await {
                    let path = upload_path(&file.name());
                    files::insert(&path, bytes);
                    done(path);
                }
            });
        })
    };
    input.set_onchange(Some(on_change.as_ref().unchecked_ref()));
    let on_cancel = {
        let input = input.clone();
        Closure::<dyn FnMut()>::new(move || input.remove())
    };
    let _ = input.add_event_listener_with_callback("cancel", on_cancel.as_ref().unchecked_ref());
    // The closures live as long as the (short-lived) input element.
    on_change.forget();
    on_cancel.forget();
    input.click();
}

/// Hands `bytes` to the browser as a download called `name`.
pub fn download(name: &str, bytes: &[u8]) {
    let result = (|| -> Result<(), JsValue> {
        let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes));
        let options = web_sys::BlobPropertyBag::new();
        options.set_type(mime_type(name));
        let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options)?;
        let url = web_sys::Url::create_object_url_with_blob(&blob)?;
        let document = window().document().ok_or("no document")?;
        let link: web_sys::HtmlAnchorElement = document.create_element("a")?.unchecked_into();
        link.set_href(&url);
        link.set_download(name);
        link.click();
        // Give the browser time to start the download before freeing it.
        let revoke = Closure::once_into_js(move || {
            let _ = web_sys::Url::revoke_object_url(&url);
        });
        window().set_timeout_with_callback_and_timeout_and_arguments_0(
            revoke.unchecked_ref(),
            60_000,
        )?;
        Ok(())
    })();
    if let Err(err) = result {
        log::warn!("download of {name} failed: {err:?}");
    }
}

fn mime_type(name: &str) -> &'static str {
    let ext = Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    match ext.as_deref() {
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("tif" | "tiff") => "image/tiff",
        Some("bmp") => "image/bmp",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

/// The `?<name>=` parameter of the page address, if any.
pub fn parameter(name: &str) -> Option<String> {
    let search = window().location().search().ok()?;
    web_sys::UrlSearchParams::new_with_str(&search)
        .ok()?
        .get(name)
}

pub fn set_title(title: &str) {
    if let Some(document) = window().document() {
        document.set_title(title);
    }
}

/// Shows `image` over the whole page and marks the page title (a test hook:
/// a WebGL canvas is not captured by browser screenshots).
pub fn show_screenshot(image: &eframe::egui::ColorImage) {
    let result = (|| -> Result<(), JsValue> {
        let document = window().document().ok_or("no document")?;
        let canvas: web_sys::HtmlCanvasElement =
            document.create_element("canvas")?.unchecked_into();
        let [w, h] = image.size;
        canvas.set_width(w as u32);
        canvas.set_height(h as u32);
        canvas
            .style()
            .set_css_text("position:fixed;inset:0;width:100%;height:100%;z-index:10");
        let context: web_sys::CanvasRenderingContext2d = canvas
            .get_context("2d")?
            .ok_or("no 2d context")?
            .unchecked_into();
        let rgba: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
        let data = web_sys::ImageData::new_with_u8_clamped_array_and_sh(
            wasm_bindgen::Clamped(&rgba),
            w as u32,
            h as u32,
        )?;
        context.put_image_data(&data, 0.0, 0.0)?;
        document.body().ok_or("no body")?.append_child(&canvas)?;
        document.set_title("docforge screenshot ready");
        Ok(())
    })();
    if let Err(err) = result {
        log::warn!("showing the screenshot failed: {err:?}");
    }
}

async fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let response: web_sys::Response = JsFuture::from(window().fetch_with_str(url))
        .await
        .map_err(|e| format!("{e:?}"))?
        .unchecked_into();
    if !response.ok() {
        return Err(format!("HTTP {}", response.status()));
    }
    bytes_of(response.array_buffer().map_err(|e| format!("{e:?}"))?)
        .await
        .map_err(|e| format!("{e:?}"))
}

/// Fetches a project and the files its nodes read (every `path`/`font`
/// parameter, resolved like the project's relative paths), stores them and
/// passes the project's path, or an error, to `done`.
pub fn open_url(url: String, done: impl FnOnce(Result<PathBuf, String>) + 'static) {
    spawn_local(async move {
        done(
            open_url_inner(&url)
                .await
                .map_err(|e| format!("{url}: {e}")),
        );
    });
}

async fn open_url_inner(url: &str) -> Result<PathBuf, String> {
    let base = window().location().href().map_err(|e| format!("{e:?}"))?;
    let project_url = web_sys::Url::new_with_base(url, &base).map_err(|e| format!("{e:?}"))?;
    let bytes = fetch(&project_url.href()).await?;
    let doc: GraphDoc = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;

    // Mirror the server's layout, so relative paths resolve the same way.
    let pathname = js_sys::decode_uri(&project_url.pathname())
        .map(String::from)
        .unwrap_or_else(|_| project_url.pathname());
    let project = files::normalize(&Path::new("/web").join(pathname.trim_start_matches('/')));
    let folder = project.parent().unwrap_or(Path::new("/web")).to_owned();

    let mut wanted = Vec::new();
    for node in &doc.nodes {
        collect_paths(&node.params, &mut wanted);
    }
    for rel in wanted {
        if Path::new(&rel).is_absolute() {
            continue; // a desktop path: the user opens that file by hand
        }
        let Ok(file_url) = web_sys::Url::new_with_base(&rel, &project_url.href()) else {
            continue;
        };
        // Outputs (e.g. a PDF to save) don't exist yet; that's fine.
        if let Ok(bytes) = fetch(&file_url.href()).await {
            files::insert(&folder.join(&rel), bytes);
        }
    }
    files::insert(&project, bytes);
    Ok(project)
}

/// String values of `path` and `font` keys, anywhere in `value`.
fn collect_paths(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, v) in map {
                match v {
                    serde_json::Value::String(s)
                        if (key == "path" || key == "font") && !s.is_empty() =>
                    {
                        out.push(s.clone());
                    }
                    _ => collect_paths(v, out),
                }
            }
        }
        serde_json::Value::Array(items) => items.iter().for_each(|v| collect_paths(v, out)),
        _ => {}
    }
}
