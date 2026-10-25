//! The assistant's command surface for the OneOS host bridge (`craft:command {method: "command.batch"}`).
//!
//! The host relays an allow-listed run: `{label, steps: [{name, args?}]}`. The whole run is judged BEFORE anything
//! runs (an id outside the list refuses the run, `command-not-allowed`), every step runs through the same registry
//! the menus use (so a step the document's security or state forbids is refused exactly as it would be for the
//! person), and the run is ONE undo step named by `label`: a step that fails takes back every step before it, so a
//! run is all or nothing. There is no file-path or dialog command in the list: files move only through the host.
//!
//! This list is the one declared in the host's `platform-craft` registry as `pdf` commands (pinned equal there).

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::PdfCraftApp;
use crate::control::Host;

#[cfg(test)]
mod tests;

/// Registry commands the assistant may run. Each acts at once on the document (no dialog, no file picker, no
/// pointer gesture), and none is Undo or Redo: undoing a run is the person's.
pub const ASSISTANT_COMMANDS: &[&str] = &[
    // Pages (act on the selection, or the current page when none is selected).
    "page.rotate",
    "page.rotate_ccw",
    "page.delete",
    "page.insert_blank",
    "page.duplicate",
    "page.reverse",
    // Document clean-up.
    "comment.flatten",
    "form.flatten",
    "form.clear",
    "form.detect",
    "edit.links_from_urls",
    "edit.remove_links",
    "edit.header_footer.remove",
    "edit.watermark.remove",
    "edit.background.remove",
    "redact.clear",
    "bookmark.from_structure",
    "a11y.autotag",
    "a11y.autotag_fields",
    // The view.
    "view.layout.single",
    "view.layout.two_up",
    "view.continuous",
    "view.fit_width_scrolling",
    "view.fit_one_page",
    "view.fit_visible",
];

/// Commands the window itself provides (they take arguments, which the registry's commands do not).
pub const ARGUMENT_COMMANDS: &[&str] = &[
    // {pages: a page list such as 1,3-5}: the physical pages the page commands act on.
    "select.pages",
    // {page: 3}
    "view.goto",
    // {percent: 125}
    "view.zoom",
    // Read-only: the document's page count, current page, selection and what Undo would say.
    "inspect",
];

/// Every command id a run may name, in the order the host's registry declares them.
pub fn allowed() -> Vec<&'static str> {
    ASSISTANT_COMMANDS.iter().chain(ARGUMENT_COMMANDS).copied().collect()
}

const MAX_STEPS: usize = 50;
const MAX_LABEL: usize = 120;

/// One relayed run: the host's request id and its `command.batch` params.
pub struct HostRun {
    pub id: Value,
    pub method: String,
    pub params: Value,
}

/// Runs waiting for the next frame (the bridge's listener pushes, the app drains).
pub type HostRuns = Arc<Mutex<Vec<HostRun>>>;

/// Answers a run: the request id and the outcome (`craft:result {ok, result|error}`).
pub type HostAnswerFn = Box<dyn Fn(&Value, Result<Value, String>)>;

impl PdfCraftApp {
    /// Run the relayed runs that arrived since the last frame, and answer each.
    pub(crate) fn drain_host_runs(&mut self) {
        let runs: Vec<HostRun> = self.host_runs.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
        for run in runs {
            let outcome = if run.method == "command.batch" {
                self.run_command_batch(&run.params)
            } else {
                Err(format!("unknown-method: this editor does not take `{}`", run.method))
            };
            if let Some(answer) = &self.host_answer {
                answer(&run.id, outcome);
            }
        }
    }

    /// One `command.batch`: all of it, as ONE undo step, or none of it.
    pub fn run_command_batch(&mut self, params: &Value) -> Result<Value, String> {
        let label = params.get("label").and_then(Value::as_str).map(str::trim).unwrap_or("");
        if label.chars().count() > MAX_LABEL {
            return Err(format!("label-too-long: a run's label is at most {MAX_LABEL} characters"));
        }
        let steps = params.get("steps").and_then(Value::as_array).ok_or("run-empty: a run states its steps")?;
        if steps.is_empty() {
            return Err("run-empty: a run states at least one command".into());
        }
        if steps.len() > MAX_STEPS {
            return Err(format!("run-too-long: a run is at most {MAX_STEPS} steps"));
        }
        // Judge the whole run first; nothing has run when one id is outside the list.
        let mut named = Vec::with_capacity(steps.len());
        for step in steps {
            let name = step.get("name").and_then(Value::as_str).unwrap_or("");
            if !ASSISTANT_COMMANDS.contains(&name) && !ARGUMENT_COMMANDS.contains(&name) {
                return Err(format!("command-not-allowed: PDF does not allow the command '{name}' -- nothing was run"));
            }
            named.push((name, step.get("args").cloned().unwrap_or(Value::Null)));
        }
        let Some((_, doc)) = self.active_ids() else {
            return Err("no-document: no PDF is open in this window".into());
        };
        let depth = self.session.undo_depth(doc);
        let mut results = Vec::with_capacity(named.len());
        for (i, (name, args)) in named.iter().enumerate() {
            match self.run_step(name, args) {
                Ok(result) => results.push(json!({ "name": name, "result": result })),
                Err(e) => {
                    self.take_back_to(doc, depth);
                    return Err(format!("step-failed: step {} ({name}): {e} -- nothing was changed", i + 1));
                }
            }
        }
        let folded = self.session.merge_undo(doc, depth, if label.is_empty() { "Assistant" } else { label });
        let info = self.session.get(doc);
        Ok(json!({
            "steps": results,
            "edits": folded.max(usize::from(self.session.undo_depth(doc) > depth)),
            "pages": info.map(|d| d.info.pages.len()),
            "undo": info.and_then(|d| d.can_undo()),
        }))
    }

    fn run_step(&mut self, name: &str, args: &Value) -> Result<Value, String> {
        let text = |key: &str| -> Result<String, String> {
            match args.get(key) {
                Some(Value::String(s)) => Ok(s.clone()),
                Some(v @ Value::Number(_)) => Ok(v.to_string()),
                _ => Err(format!("{name} needs `{key}`")),
            }
        };
        match name {
            "select.pages" => self.set_option("select", &text("pages")?).map(|()| json!({ "selected": text("pages").ok() })),
            "view.goto" => self.set_option("page", &text("page")?).map(|()| Value::Null),
            "view.zoom" => self.set_option("zoom", &text("percent")?).map(|()| Value::Null),
            "inspect" => {
                let (view, id) = self.active_ids().ok_or("no PDF is open in this window")?;
                let v = &self.views[view];
                let doc = self.session.get(id).ok_or("no PDF is open in this window")?;
                Ok(json!({
                    "name": doc.name,
                    "pages": doc.info.pages.len(),
                    "page": v.current + 1,
                    "selected": v.selected.iter().map(|p| p + 1).collect::<Vec<_>>(),
                    "undo": doc.can_undo(),
                }))
            }
            _ => {
                let before = self.dialog;
                let result = Host::command(self, name)?;
                if self.dialog != before {
                    // A command in the list never opens one; if it does, the dialog must not sit over the person's work.
                    self.dialog = before;
                    return Err(format!("{name} needed a dialog"));
                }
                Ok(result)
            }
        }
    }

    /// Undo until the document holds only `depth` steps again (a failed run's earlier steps).
    fn take_back_to(&mut self, doc: pdfcraft_engine::DocId, depth: usize) {
        while self.session.undo_depth(doc) > depth {
            let before = self.session.undo_depth(doc);
            self.undo();
            if self.session.undo_depth(doc) >= before {
                break;
            }
        }
    }
}
