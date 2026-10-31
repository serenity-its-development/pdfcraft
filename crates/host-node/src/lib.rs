//! PDF's host window for node.
//!
//! What the browser build does with the host's `craft:open`, `craft:command` and `craft:save` (`apps/pdfcraft-web`), on the app shell
//! alone: a [`HostWindow`] opens a PDF from its bytes, runs the assistant's `command.batch`
//! (`pdfcraft_ui_egui::PdfCraftApp::run_command_batch`, the function the window answers `craft:command` with) and writes the file a Save
//! would hand the host (`Session::save_bytes`, what the browser build's Save delivers). OneOS's AI-drive tests drive a PDF through the
//! `pdf.*` tools with it; nothing here is a second implementation.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use pdfcraft_ui_egui::PdfCraftApp;
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;

fn err(e: impl std::fmt::Display) -> JsError {
    JsError::new(&e.to_string())
}

/// One open PDF, as the browser window holds it.
#[wasm_bindgen]
pub struct HostWindow {
    app: PdfCraftApp,
}

#[wasm_bindgen]
impl HostWindow {
    /// Open `name`'s bytes (base64), or a new one-page Letter PDF when there are none. `client` is the party number the host would give
    /// the window; PDF has no collaboration, so it is unused.
    #[wasm_bindgen(constructor)]
    pub fn new(_client: u32, name: String, file_b64: String) -> Result<HostWindow, JsError> {
        let mut app = PdfCraftApp::new();
        let bytes = if file_b64.is_empty() {
            app.session.create_blank(612.0, 792.0, 1).map_err(err)?.to_vec()
        } else {
            STANDARD.decode(file_b64.trim()).map_err(err)?
        };
        app.open_bytes(&name, None, bytes).map_err(|e| JsError::new(&e))?;
        if app.active_ids().is_none() {
            return Err(JsError::new(&format!("{name} did not open as a PDF")));
        }
        Ok(HostWindow { app })
    }

    /// The window's `command.batch`: `params_json` is the host's `{label, steps}`; the answer is the `result` of `craft:result` as
    /// JSON, or an error carrying its `<code>: <reason>` text.
    pub fn command_batch(&mut self, params_json: &str) -> Result<String, JsError> {
        let params: Value = serde_json::from_str(params_json).map_err(err)?;
        let result = self.app.run_command_batch(&params).map_err(|e| JsError::new(&e))?;
        Ok(result.to_string())
    }

    /// A registry command as the person's own menu runs it (Undo, in the tests), answered as JSON `{"ran": id}`. PDF's registry
    /// commands take no parameters, so `params_json` is only checked to be JSON.
    pub fn run(&mut self, command: &str, params_json: &str) -> Result<String, JsError> {
        let _: Value = serde_json::from_str(params_json).map_err(err)?;
        if !self.app.execute(command) {
            return Err(JsError::new(&format!("{command} is not a command, or is disabled right now")));
        }
        Ok(json!({ "ran": command }).to_string())
    }

    /// The file a Save would hand the host, base64: the PDF saved as the file it was opened as (an incremental update of it).
    pub fn file_b64(&self) -> Result<String, JsError> {
        let (_, id) = self.app.active_ids().ok_or_else(|| JsError::new("no PDF is open in this window"))?;
        let bytes = self.app.session.save_bytes(id).map_err(err)?;
        Ok(STANDARD.encode(bytes.as_slice()))
    }

    /// Whether the PDF has changes the host does not hold yet (what the browser build reports as `craft:dirty`).
    pub fn dirty(&self) -> bool {
        self.app.first_dirty().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pdf(pages: usize) -> String {
        let bytes = pdfcraft_ui_egui::PdfCraftApp::new().session.create_blank(200.0, 300.0, pages).unwrap();
        STANDARD.encode(bytes.as_slice())
    }

    fn pages_in(b64: &str) -> usize {
        let mut app = PdfCraftApp::new();
        app.open_bytes("back.pdf", None, STANDARD.decode(b64).unwrap()).unwrap();
        let (_, id) = app.active_ids().unwrap();
        app.session.get(id).unwrap().info.pages.len()
    }

    fn depth(w: &HostWindow) -> usize {
        w.app.session.undo_depth(w.app.active_ids().unwrap().1)
    }

    /// Corpus G1: PDF had no node host window, so no AI-drive test could reach it. The window opens a PDF from bytes, runs a
    /// command.batch as one undo step, and saves the file it was opened as.
    #[test]
    fn host_window_runs_a_batch_as_one_undo_step_and_saves_it() {
        let mut w = HostWindow::new(7, "report.pdf".into(), pdf(2)).unwrap();
        assert!(!w.dirty());
        let r: Value = serde_json::from_str(
            &w.command_batch(
                r#"{"label":"Add two blank pages","steps":[{"name":"view.goto","args":{"page":2}},{"name":"page.insert_blank"},{"name":"page.insert_blank"},{"name":"inspect"}]}"#,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(r["pages"], json!(4), "{r}");
        assert_eq!(r["undo"], json!("Add two blank pages"), "{r}");
        assert_eq!(r["steps"][3]["result"]["pages"], json!(4), "{r}");
        assert_eq!(depth(&w), 1, "two inserts, one undo step");
        assert!(w.dirty());
        let saved = w.file_b64().unwrap();
        assert!(STANDARD.decode(&saved).unwrap().starts_with(b"%PDF"));
        assert_eq!(pages_in(&saved), 4, "the saved PDF has the assistant's pages");
        // One Undo takes the whole run back.
        assert_eq!(serde_json::from_str::<Value>(&w.run("edit.undo", "{}").unwrap()).unwrap(), json!({"ran": "edit.undo"}));
        assert_eq!(depth(&w), 0);
        assert!(!w.dirty());
        assert_eq!(pages_in(&w.file_b64().unwrap()), 2);
    }

    #[test]
    fn a_command_outside_the_list_refuses_the_whole_batch_by_name() {
        let mut w = HostWindow::new(1, "a.pdf".into(), pdf(3)).unwrap();
        let e = w.app.run_command_batch(&json!({"steps": [{"name": "page.delete"}, {"name": "file.save"}]})).unwrap_err();
        assert!(e.starts_with("command-not-allowed") && e.contains("'file.save'"), "{e}");
        assert_eq!(depth(&w), 0, "the allowed step before it did not run");
        assert!(!w.dirty());
        assert_eq!(pages_in(&w.file_b64().unwrap()), 3);
    }

    #[test]
    fn no_bytes_open_a_new_one_page_pdf() {
        let w = HostWindow::new(1, "new.pdf".into(), String::new()).unwrap();
        assert_eq!(pages_in(&w.file_b64().unwrap()), 1);
    }
}
