//! PDF in the browser. Build: `cd apps/pdfcraft-web && trunk build --release`
//! (trunk generates the JS loader; no handwritten JS — plan/execution-plan.md §1).

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(target_arch = "wasm32")]
fn main() {
    use eframe::wasm_bindgen::JsCast;
    use pdfcraft_ui_egui::PdfCraftApp;

    let options = eframe::WebOptions { renderer: eframe::Renderer::Glow, ..Default::default() };
    wasm_bindgen_futures::spawn_local(async move {
        let Some(canvas) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.get_element_by_id("pdfcraft"))
            .and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok())
        else {
            return;
        };
        let started = eframe::WebRunner::new()
            .start(
                canvas,
                options,
                Box::new(|cc| {
                    let mut app = PdfCraftApp::new();
                    // "printcraft": settings saved under the app's former name, PrintCraft.
                    if let Some(json) = cc.storage.and_then(|s| s.get_string("pdfcraft").or_else(|| s.get_string("printcraft"))) {
                        app.restore(&json);
                    }
                    // `?file=<url>` opens a PDF from a URL (same-origin or CORS-enabled).
                    if let Some(url) = query_param("file") {
                        let inbox = app.startup_inbox.clone();
                        let failed = app.failed_inbox.clone();
                        let ctx = cc.egui_ctx.clone();
                        wasm_bindgen_futures::spawn_local(async move {
                            let name = url.rsplit('/').next().unwrap_or("document.pdf").split('?').next().unwrap_or("document.pdf").to_string();
                            match fetch_bytes(&url).await {
                                Ok(bytes) => {
                                    if let Ok(mut q) = inbox.lock() {
                                        q.push((name, bytes));
                                    }
                                }
                                Err(e) => {
                                    eframe::web_sys::console::error_1(&format!("PDF: could not fetch {url}: {e}").into());
                                    // Shown in the app too, not only in the console (#173).
                                    if let Ok(mut q) = failed.lock() {
                                        q.push((name, e));
                                    }
                                }
                            }
                            ctx.request_repaint();
                        });
                    }
                    // eframe never raises a close request on a browser reload or tab close, so `guard_quit` never
                    // prompts; ask the browser to confirm instead while a document has unsaved work (#812).
                    warn_before_unload(app.unsaved_flag.clone());
                    // OneOS host bridge: the frame page's loader (`bootstrap.js`) declares the ONE parent origin this
                    // editor answers to (`globalThis.__craftHost`); with none declared the editor runs unframed and
                    // saves download.
                    if let Some(bridge) = host::Bridge::new() {
                        bridge.listen(app.startup_inbox.clone(), app.host_notices.clone(), cc.egui_ctx.clone());
                        app.host_save = Some(bridge.save_fn());
                        app.host_dirty = Some(bridge.dirty_fn());
                        bridge.post_ready();
                    }
                    Ok(Box::new(app))
                }),
            )
            .await;
        if let Err(e) = started {
            eframe::web_sys::console::error_1(&e);
            if let Some(bridge) = host::Bridge::new() {
                bridge.post_failed(&e.as_string().unwrap_or_else(|| format!("{e:?}")));
            }
        }
    });
}

#[cfg(target_arch = "wasm32")]
fn warn_before_unload(unsaved: std::sync::Arc<std::sync::atomic::AtomicBool>) {
    use eframe::wasm_bindgen::{JsCast, closure::Closure};
    let Some(window) = web_sys::window() else { return };
    let handler = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
        if unsaved.load(std::sync::atomic::Ordering::Relaxed) {
            event.prevent_default();
        }
    });
    if window.add_event_listener_with_callback("beforeunload", handler.as_ref().unchecked_ref()).is_ok() {
        handler.forget();
    }
}

#[cfg(target_arch = "wasm32")]
fn query_param(key: &str) -> Option<String> {
    let search = web_sys::window()?.location().search().ok()?;
    web_sys::UrlSearchParams::new_with_str(&search).ok()?.get(key)
}

#[cfg(target_arch = "wasm32")]
async fn fetch_bytes(url: &str) -> Result<Vec<u8>, String> {
    use eframe::wasm_bindgen::JsCast;
    let window = web_sys::window().ok_or("no window")?;
    let resp = wasm_bindgen_futures::JsFuture::from(window.fetch_with_str(url)).await.map_err(|e| format!("{e:?}"))?;
    let resp: web_sys::Response = resp.dyn_into().map_err(|_| "not a response")?;
    if !resp.ok() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let buf = wasm_bindgen_futures::JsFuture::from(resp.array_buffer().map_err(|e| format!("{e:?}"))?).await.map_err(|e| format!("{e:?}"))?;
    Ok(js_sys::Uint8Array::new(&buf).to_vec())
}

/// The OneOS host bridge: `postMessage` to the ONE parent the frame page declared (`globalThis.__craftHost =
/// {app, version, parent}`, set by the build's `bootstrap.js` from the page's `data-parent`). The frame lives on its
/// own origin, so the parent is a DIFFERENT origin: messages go to that origin only, and only the parent window on
/// that origin is heard. Every message is a plain object with a `type`; `session` is the per-open nonce the host
/// mints in `craft:open` and every later message echoes (`plans/mona/craft-integration-design-2026-10-25.md` 2.2):
///
/// - `craft:ready {app, version}` (to the parent): listening; send the file now.
/// - `craft:open {session, name, mime, bytes, versionId, readOnly, collab}` (from the parent): open these bytes.
/// - `craft:save {session, name, mime, bytes, baseVersionId, saveAs}` (to the parent): the person saved; the bytes are a
///   transferred `ArrayBuffer`. The host makes a NEW version of the file through the compare-and-set and answers
///   `craft:saved {session, versionId}` (the next save's base) or `craft:refused {session, code, reason}`.
/// - `craft:dirty {session, dirty}` (to the parent): unsaved work appeared or went away.
/// - `craft:failed {error}` (to the parent): the editor could not start.
/// - `craft:command {session, id, method, params}` (from the parent) is answered `craft:result {session, id, ok:false}`:
///   this editor takes no assistant commands yet.
///
/// A file opened read-only is never saved (the save fails by name); a second document opened in a tab under a
/// different name is saved as a copy (`saveAs`), never over the host's file.
#[cfg(target_arch = "wasm32")]
mod host {
    use std::cell::RefCell;
    use std::rc::Rc;

    use eframe::wasm_bindgen::{JsCast, JsValue, closure::Closure};

    /// What the host's last `craft:open` said.
    #[derive(Default)]
    struct Opened {
        session: String,
        name: String,
        version_id: String,
        read_only: bool,
    }

    #[derive(Clone)]
    pub struct Bridge {
        window: web_sys::Window,
        parent: web_sys::Window,
        /// The declared parent origin: the only target and the only sender.
        origin: String,
        app: String,
        version: String,
        opened: Rc<RefCell<Opened>>,
    }

    /// `scheme://host[:port]` exactly: https, or http only on localhost (the dev estate).
    fn plain_origin(v: &str) -> bool {
        let Some((scheme, rest)) = v.split_once("://") else { return false };
        let local = rest == "localhost" || rest.starts_with("localhost:") || rest.starts_with("127.0.0.1");
        (scheme == "https" || (scheme == "http" && local))
            && !rest.is_empty()
            && rest.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':'))
    }

    impl Bridge {
        /// `None` when the page is not framed, or declares no parent origin.
        pub fn new() -> Option<Self> {
            let window = web_sys::window()?;
            let parent = window.parent().ok().flatten()?;
            if js_sys::Object::is(&parent, &window) {
                return None;
            }
            let declared = js_sys::Reflect::get(&window, &JsValue::from_str("__craftHost")).ok().filter(|d| d.is_object())?;
            let origin = string(&declared, "parent").filter(|o| plain_origin(o))?;
            Some(Self {
                window,
                parent,
                origin,
                app: string(&declared, "app").unwrap_or_else(|| "pdf".into()),
                version: string(&declared, "version").unwrap_or_else(|| env!("CARGO_PKG_VERSION").into()),
                opened: Rc::default(),
            })
        }

        fn post(&self, message: &js_sys::Object, transfer: Option<&JsValue>) -> Result<(), String> {
            let sent = match transfer {
                Some(t) => self.parent.post_message_with_transfer(message, &self.origin, &js_sys::Array::of1(t)),
                None => self.parent.post_message(message, &self.origin),
            };
            sent.map_err(|e| e.as_string().unwrap_or_else(|| format!("{e:?}")))
        }

        pub fn post_ready(&self) {
            if let Some(m) = message(&[("type", "craft:ready".into()), ("app", self.app.as_str().into()), ("version", self.version.as_str().into())]) {
                let _ = self.post(&m, None);
            }
        }

        /// The app couldn't start (no WebGL, for example): let the parent say so.
        pub fn post_failed(&self, error: &str) {
            if let Some(m) = message(&[("type", "craft:failed".into()), ("error", error.into())]) {
                let _ = self.post(&m, None);
            }
        }

        /// Hear the host: `craft:open` queues the file like a `?file=` download; `craft:saved` and `craft:refused` answer a save.
        pub fn listen(&self, inbox: pdfcraft_ui_egui::Inbox, notices: pdfcraft_ui_egui::HostNotices, ctx: egui::Context) {
            let bridge = self.clone();
            let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
                if event.origin() != bridge.origin || !event.source().is_some_and(|s| js_sys::Object::is(&s, &bridge.parent)) {
                    return;
                }
                let data = event.data();
                let Some(kind) = string(&data, "type") else { return };
                match kind.as_str() {
                    "craft:open" => {
                        let Some(session) = string(&data, "session").filter(|s| !s.is_empty()) else { return };
                        let name = string(&data, "name").filter(|n| !n.trim().is_empty()).unwrap_or_else(|| "document.pdf".to_string());
                        let Ok(bytes) = js_sys::Reflect::get(&data, &JsValue::from_str("bytes")) else { return };
                        let bytes = if let Some(array) = bytes.dyn_ref::<js_sys::Uint8Array>() {
                            array.to_vec()
                        } else if bytes.is_instance_of::<js_sys::ArrayBuffer>() {
                            js_sys::Uint8Array::new(&bytes).to_vec()
                        } else {
                            return;
                        };
                        *bridge.opened.borrow_mut() = Opened {
                            session,
                            name: name.clone(),
                            version_id: string(&data, "versionId").unwrap_or_default(),
                            read_only: boolean(&data, "readOnly"),
                        };
                        if let Ok(mut q) = inbox.lock() {
                            q.push((name, bytes));
                        }
                    }
                    "craft:saved" => {
                        let mut opened = bridge.opened.borrow_mut();
                        if string(&data, "session").as_deref() == Some(opened.session.as_str())
                            && let Some(version) = string(&data, "versionId")
                        {
                            opened.version_id = version;
                        }
                    }
                    "craft:refused" => {
                        if string(&data, "session").as_deref() == Some(bridge.opened.borrow().session.as_str()) {
                            let reason = string(&data, "reason").unwrap_or_else(|| "refused".into());
                            if let Ok(mut q) = notices.lock() {
                                q.push(format!("Couldn't save: {reason}"));
                            }
                        }
                    }
                    "craft:command" => {
                        let session = string(&data, "session").unwrap_or_default();
                        let id = js_sys::Reflect::get(&data, &JsValue::from_str("id")).unwrap_or(JsValue::from_f64(0.0));
                        if let Some(m) = message(&[
                            ("type", "craft:result".into()),
                            ("session", session.as_str().into()),
                            ("id", id),
                            ("ok", false.into()),
                            ("error", "this editor takes no assistant commands yet".into()),
                        ]) {
                            let _ = bridge.post(&m, None);
                        }
                    }
                    _ => {}
                }
                ctx.request_repaint();
            });
            if self.window.add_event_listener_with_callback("message", on_message.as_ref().unchecked_ref()).is_ok() {
                // The listener lives as long as the page.
                on_message.forget();
            }
        }

        pub fn save_fn(&self) -> pdfcraft_ui_egui::HostSaveFn {
            let bridge = self.clone();
            Box::new(move |name: &str, bytes: &[u8], save_as: bool| {
                let (session, base, as_copy) = {
                    let opened = bridge.opened.borrow();
                    if opened.session.is_empty() {
                        return Err("the host has not opened a document yet".to_string());
                    }
                    if opened.read_only {
                        return Err("this file is open to view; saving is off".to_string());
                    }
                    (opened.session.clone(), opened.version_id.clone(), save_as || name != opened.name)
                };
                let buffer = js_sys::Uint8Array::from(bytes).buffer();
                let m = message(&[
                    ("type", "craft:save".into()),
                    ("session", session.as_str().into()),
                    ("name", name.into()),
                    ("mime", "application/pdf".into()),
                    ("baseVersionId", base.as_str().into()),
                    ("saveAs", as_copy.into()),
                    ("bytes", buffer.clone().into()),
                ])
                .ok_or_else(|| "couldn't build the message".to_string())?;
                bridge.post(&m, Some(&buffer))
            })
        }

        pub fn dirty_fn(&self) -> pdfcraft_ui_egui::HostDirtyFn {
            let bridge = self.clone();
            let mut last: Option<(String, bool)> = None;
            Box::new(move |dirty: bool| {
                let session = bridge.opened.borrow().session.clone();
                if session.is_empty() || last.as_ref() == Some(&(session.clone(), dirty)) {
                    return;
                }
                if let Some(m) = message(&[("type", "craft:dirty".into()), ("session", session.as_str().into()), ("dirty", dirty.into())])
                    && bridge.post(&m, None).is_ok()
                {
                    last = Some((session, dirty));
                }
            })
        }
    }

    fn message(fields: &[(&str, JsValue)]) -> Option<js_sys::Object> {
        let m = js_sys::Object::new();
        for (key, value) in fields {
            js_sys::Reflect::set(&m, &JsValue::from_str(key), value).ok()?;
        }
        Some(m)
    }

    fn string(data: &JsValue, key: &str) -> Option<String> {
        js_sys::Reflect::get(data, &JsValue::from_str(key)).ok()?.as_string()
    }

    fn boolean(data: &JsValue, key: &str) -> bool {
        js_sys::Reflect::get(data, &JsValue::from_str(key)).ok().and_then(|v| v.as_bool()).unwrap_or(false)
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    eprintln!("pdfcraft-web targets wasm32: run `trunk serve` or `trunk build --release` in apps/pdfcraft-web");
}
