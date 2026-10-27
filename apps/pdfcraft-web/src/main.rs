//! PDF in the browser. Build: `cd apps/pdfcraft-web && trunk build --release`
//! (trunk generates the JS loader; no handwritten JS — plan/execution-plan.md §1).

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(any(target_arch = "wasm32", test))]
mod handoff;

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
                    let mut handoff = None;
                    if let Some(bridge) = host::Bridge::new() {
                        bridge.listen(app.startup_inbox.clone(), app.host_notices.clone(), app.host_runs.clone(), cc.egui_ctx.clone());
                        app.host_answer = Some(bridge.answer_fn());
                        app.host_save = Some(bridge.save_fn());
                        app.host_dirty = Some(bridge.dirty_fn());
                        bridge.post_ready();
                        handoff = Some((bridge, handoff::Handoff::new()));
                    }
                    Ok(Box::new(Shell { app, handoff }))
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

/// The app and, when framed by OneOS, the progressive open's hand-over to the host (`craft:opened`, the first view's place,
/// `craft:viewed`: `handoff.rs`).
#[cfg(target_arch = "wasm32")]
struct Shell {
    app: pdfcraft_ui_egui::PdfCraftApp,
    handoff: Option<(host::Bridge, handoff::Handoff<Option<u64>>)>,
}

#[cfg(target_arch = "wasm32")]
impl Shell {
    /// The document the app shows (the id of the active view): it changes when the host's file is opened.
    fn shown(app: &pdfcraft_ui_egui::PdfCraftApp) -> Option<u64> {
        app.active.and_then(|i| app.views.get(i)).map(|v| v.id.0)
    }
}

#[cfg(target_arch = "wasm32")]
impl eframe::App for Shell {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.app.raw_input_hook(ctx, raw);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.app.save(storage);
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Some((bridge, handoff)) = self.handoff.as_mut() {
            handoff.before(bridge, Self::shown(&self.app));
        }
        self.app.logic(ctx, frame);
        if let Some((bridge, handoff)) = self.handoff.as_mut() {
            let shown = Self::shown(&self.app);
            let busy = self.app.first_view_busy();
            let app = &mut self.app;
            if handoff.after_with(bridge, shown, |view| { app.open_at_first_view(&view.selection, view.y_fraction()); }, busy) {
                ctx.request_repaint();
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.app.ui(ui, frame);
    }
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
/// - `craft:opened {session}` (to the parent): the document is loaded and drawn; the editor takes input. `craft:view {session, scrollTop,
///   scrollHeight, clientHeight, selection ...}` (from the parent, kept by the loader on `globalThis.__craftFirstView`): where the host's
///   read-only first view was left; the editor opens there (the page and the offset in it, the selected text) and answers
///   `craft:viewed {session}` once that is drawn (`handoff.rs`).
/// - `craft:command {session, id, method: "command.batch", params: {label, steps: [{name, args?}]}}` (from the parent) is
///   the assistant's allow-listed run (`pdfcraft_ui_egui::host_commands`): all of it as ONE undo step, or none of it. It is
///   answered `craft:result {session, id, ok:true, result}` or `{ok:false, error: "<code>: <reason>"}` (a command outside
///   the list, a step the document forbids, a file open read-only, or a `session` that is not the open one).
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

        /// `craft:opened {session}`: the document is loaded and drawn, the editor takes input (handoff.rs).
        pub fn post_opened(&self) {
            let session = self.opened.borrow().session.clone();
            if let Some(m) = message(&[("type", "craft:opened".into()), ("session", session.as_str().into())]) {
                let _ = self.post(&m, None);
            }
        }

        /// `craft:viewed {session}`: the first view's place is applied and drawn; the host may let the first view go.
        pub fn post_viewed(&self) {
            let session = self.opened.borrow().session.clone();
            if let Some(m) = message(&[("type", "craft:viewed".into()), ("session", session.as_str().into())]) {
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
        pub fn listen(&self, inbox: pdfcraft_ui_egui::Inbox, notices: pdfcraft_ui_egui::HostNotices, runs: pdfcraft_ui_egui::HostRuns, ctx: egui::Context) {
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
                        crate::handoff::wasm::note_open();
                    }
                    // The loader keeps the place on `__craftFirstView`; the app reads it (handoff.rs) - this wakes it.
                    "craft:view" => {}
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
                        let id = js_sys::Reflect::get(&data, &JsValue::from_str("id")).ok().and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let (current, read_only) = {
                            let opened = bridge.opened.borrow();
                            (opened.session.clone(), opened.read_only)
                        };
                        if session.is_empty() || session != current {
                            bridge.post_result(&session, id, Err("session-mismatch: this command is for another open of the file".into()));
                        } else if read_only {
                            bridge.post_result(&session, id, Err("read-only: this file is open to view; the assistant cannot change it".into()));
                        } else {
                            let params = js_sys::Reflect::get(&data, &JsValue::from_str("params"))
                                .ok()
                                .and_then(|p| js_sys::JSON::stringify(&p).ok())
                                .and_then(|s| s.as_string())
                                .and_then(|s| serde_json::from_str(&s).ok())
                                .unwrap_or(serde_json::Value::Null);
                            let run = pdfcraft_ui_egui::HostRun { id: serde_json::json!(id), method: string(&data, "method").unwrap_or_default(), params };
                            if let Ok(mut q) = runs.lock() {
                                q.push(run);
                            }
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

        /// `craft:result {session, id, ok, result|error}` to the parent.
        pub fn post_result(&self, session: &str, id: f64, outcome: Result<serde_json::Value, String>) {
            let mut fields: Vec<(&str, JsValue)> =
                vec![("type", "craft:result".into()), ("session", session.into()), ("id", id.into()), ("ok", outcome.is_ok().into())];
            match &outcome {
                Ok(value) => {
                    let result = js_sys::JSON::parse(&value.to_string()).unwrap_or(JsValue::NULL);
                    fields.push(("result", result));
                }
                Err(error) => fields.push(("error", error.as_str().into())),
            }
            if let Some(m) = message(&fields) {
                let _ = self.post(&m, None);
            }
        }

        /// Answers a relayed run, on the session it was asked in.
        pub fn answer_fn(&self) -> pdfcraft_ui_egui::HostAnswerFn {
            let bridge = self.clone();
            Box::new(move |id: &serde_json::Value, outcome: Result<serde_json::Value, String>| {
                let session = bridge.opened.borrow().session.clone();
                bridge.post_result(&session, id.as_f64().unwrap_or(0.0), outcome);
            })
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

    impl crate::handoff::Link for Bridge {
        fn open_seq(&self) -> u64 {
            crate::handoff::wasm::opens()
        }
        fn take_view(&self) -> Option<crate::handoff::FirstView> {
            crate::handoff::wasm::take_view(&self.opened.borrow().session)
        }
        fn post_opened(&self) {
            Bridge::post_opened(self);
        }
        fn post_viewed(&self) {
            Bridge::post_viewed(self);
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
