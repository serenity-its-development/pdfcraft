use std::cell::RefCell;
use std::rc::Rc;

use pdfcraft_engine::Session;

use super::*;

fn app(pages: usize) -> PdfCraftApp {
    let mut app = PdfCraftApp::new();
    let pdf = Session::new().create_blank(200.0, 300.0, pages).unwrap().to_vec();
    app.open_bytes("doc.pdf", None, pdf).unwrap();
    app
}

fn pages(app: &PdfCraftApp) -> usize {
    app.session.get(app.active_ids().unwrap().1).unwrap().info.pages.len()
}

fn rotation(app: &PdfCraftApp, page: usize) -> i32 {
    app.session.get(app.active_ids().unwrap().1).unwrap().info.pages[page].rotation as i32
}

fn depth(app: &PdfCraftApp) -> usize {
    app.session.undo_depth(app.active_ids().unwrap().1)
}

#[test]
fn every_listed_command_is_a_registered_one_and_none_is_a_file_or_dialog_door() {
    for id in ASSISTANT_COMMANDS {
        assert!(pdfcraft_engine::commands::command(id).is_some(), "{id} is not in the registry");
        for door in ["file.", "print.", "export.", "create.", "page.insert", "page.replace", "page.extract", "page.split", "edit.undo", "edit.redo"] {
            assert!(!id.starts_with(door) || *id == "page.insert_blank", "{id} opens a door the assistant must not");
        }
    }
    let all = allowed();
    let mut sorted = all.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), all.len(), "an id is listed twice");
}

#[test]
fn a_run_is_one_undo_step_named_by_its_label() {
    let mut app = app(3);
    let out = app
        .run_command_batch(&json!({"label": "Rotate the first two", "steps": [
            {"name": "select.pages", "args": {"pages": "1-2"}},
            {"name": "page.rotate"},
            {"name": "page.rotate"},
            {"name": "inspect"},
        ]}))
        .unwrap();
    assert_eq!(rotation(&app, 0), 180);
    assert_eq!(rotation(&app, 1), 180);
    assert_eq!(rotation(&app, 2), 0);
    assert_eq!(depth(&app), 1, "two rotations, one step");
    assert_eq!(out["undo"], "Rotate the first two");
    assert_eq!(out["edits"], 2);
    assert_eq!(out["steps"][3]["result"]["selected"], json!([1, 2]));
    app.undo();
    assert_eq!((rotation(&app, 0), rotation(&app, 1)), (0, 0), "one Undo takes the whole run back");
}

#[test]
fn an_id_outside_the_list_refuses_the_whole_run() {
    let mut app = app(3);
    for name in ["file.save", "file.open", "page.insert", "edit.undo", "ui.command", "page.nope", ""] {
        let e = app.run_command_batch(&json!({"steps": [{"name": "page.delete"}, {"name": name}]})).unwrap_err();
        assert!(e.starts_with("command-not-allowed"), "{name}: {e}");
    }
    assert_eq!(pages(&app), 3, "the allowed step before it did not run");
    assert_eq!(depth(&app), 0);
    assert!(app.run_command_batch(&json!({"steps": []})).unwrap_err().starts_with("run-empty"));
    assert!(app.run_command_batch(&json!({})).unwrap_err().starts_with("run-empty"));
    let long = "x".repeat(121);
    assert!(app.run_command_batch(&json!({"label": long, "steps": [{"name": "inspect"}]})).unwrap_err().starts_with("label-too-long"));
}

#[test]
fn a_failing_step_takes_back_the_steps_before_it() {
    let mut app = app(3);
    // This document has no comments, so Flatten comments is refused after the page was deleted.
    let e = app.run_command_batch(&json!({"label": "x", "steps": [{"name": "page.delete"}, {"name": "comment.flatten"}]})).unwrap_err();
    assert!(e.starts_with("step-failed: step 2 (comment.flatten)"), "{e}");
    assert!(e.ends_with("nothing was changed"));
    assert_eq!(pages(&app), 3);
    assert_eq!(depth(&app), 0);
    // A bad argument is a step failure too.
    let e = app.run_command_batch(&json!({"steps": [{"name": "select.pages", "args": {"pages": "99"}}]})).unwrap_err();
    assert!(e.starts_with("step-failed"), "{e}");
}

#[test]
fn view_steps_are_no_edit() {
    let mut app = app(3);
    let out = app
        .run_command_batch(&json!({"steps": [{"name": "view.goto", "args": {"page": 2}}, {"name": "view.zoom", "args": {"percent": 150}}]}))
        .unwrap();
    assert_eq!(out["edits"], 0);
    assert_eq!(depth(&app), 0);
    let inspect = app.run_command_batch(&json!({"steps": [{"name": "inspect"}]})).unwrap();
    assert_eq!(inspect["steps"][0]["result"]["page"], 2);
    assert_eq!(inspect["steps"][0]["result"]["pages"], 3);
}

#[test]
fn no_document_open_is_refused_by_name() {
    let mut app = PdfCraftApp::new();
    assert!(app.run_command_batch(&json!({"steps": [{"name": "page.rotate"}]})).unwrap_err().starts_with("no-document"));
}

#[test]
fn relayed_runs_are_answered_on_the_next_frame() {
    let mut app = app(2);
    let answers: Rc<RefCell<Vec<(Value, Result<Value, String>)>>> = Rc::default();
    let sink = answers.clone();
    app.host_answer = Some(Box::new(move |id, r| sink.borrow_mut().push((id.clone(), r))));
    {
        let mut q = app.host_runs.lock().unwrap();
        q.push(HostRun { id: json!(7), method: "command.batch".into(), params: json!({"label": "Flip", "steps": [{"name": "page.reverse"}]}) });
        q.push(HostRun { id: json!(8), method: "ui.quit".into(), params: Value::Null });
    }
    app.drain_host_runs();
    let got = answers.borrow();
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].0, json!(7));
    assert_eq!(got[0].1.as_ref().unwrap()["undo"], "Flip");
    assert!(got[1].1.as_ref().unwrap_err().starts_with("unknown-method"));
}

#[test]
fn the_allow_list_and_the_commands_left_out_are_the_whole_catalog_with_nothing_twice() {
    let all: Vec<String> = pdfcraft_engine::commands::COMMANDS.iter().map(|c| c.id.to_string()).collect();
    let mut seen = std::collections::HashSet::new();
    for id in ASSISTANT_COMMANDS {
        assert!(all.iter().any(|c| c == id), "{id} is not a command of the app");
        assert!(seen.insert(id.to_string()), "{id} is listed twice");
    }
    // The window's own commands stand beside the registry's: they are not in it.
    for id in ARGUMENT_COMMANDS {
        assert!(seen.insert(id.to_string()), "{id} is listed twice");
    }
    for (reason, ids) in LEFT_OUT {
        assert!(reason.len() > 20, "a reason: {reason}");
        for id in *ids {
            assert!(all.iter().any(|c| c == id), "{id} is left out but is not a command of the app");
            assert!(seen.insert(id.to_string()), "{id} is both allowed and left out, or left out twice");
        }
    }
    let missing: Vec<&String> = all.iter().filter(|c| !seen.contains(c.as_str())).collect();
    assert!(missing.is_empty(), "commands neither allowed nor left out (with a reason): {missing:?}");
}
