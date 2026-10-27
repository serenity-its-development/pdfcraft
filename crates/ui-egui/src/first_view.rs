//! Open at where the host's read-only first view was left (OneOS progressive open, 2026-10-27).
//!
//! The viewer showed the pages as a scrolling column; the host says how far down it was (a fraction of the scrollable height) and which
//! text, if any, was selected. The editor opens at the same PAGE and OFFSET IN IT: the page at that fraction of the document, and the
//! fraction of its height that was at the top edge (`DocView::goto`, the app's own jump); the selected text is selected as soon as the
//! text of the pages near that place has been read (the renderer extracts it for the pages on screen), on the occurrence nearest the top.
//! Nothing here changes the document.

use std::ops::Range;

use pdfcraft_render::PageText;

use crate::PdfCraftApp;

/// The most frames the editor waits for the text of the pages near the place before it lets the first view go without the selection.
pub const TEXT_WAIT_FRAMES: u32 = 120;

/// The page and the fraction of its height at the top edge, for a first view scrolled to `y_fraction` of a document of `pages` pages.
pub fn place(pages: usize, y_fraction: f64) -> (usize, f32) {
    if pages == 0 {
        return (0, 0.0);
    }
    let f = if y_fraction.is_finite() { y_fraction.clamp(0.0, 1.0) } else { 0.0 };
    // The column's height is the pages' heights; with pages of about one height, `f` is how many pages down the top edge was.
    let at = f * pages as f64;
    let page = (at.floor() as usize).min(pages - 1);
    let within = if page + 1 == pages && at >= pages as f64 { 1.0 } else { at - at.floor() };
    (page, within as f32)
}

/// The glyph range of the occurrence of `needle` on `text` (case-insensitive, runs of white space as one space), or none. A selection of
/// several lines is placed by its first non-empty line.
pub fn find_selection(text: &PageText, needle: &str) -> Option<Range<usize>> {
    let first_line = needle.lines().map(str::trim).find(|l| !l.is_empty())?;
    text.find(first_line).into_iter().next()
}

/// The selected text still to be found: the page the top edge was on, and how many frames it has been waited for.
#[derive(Clone, Debug)]
pub struct PendingText {
    pub text: String,
    pub page: usize,
    pub waited: u32,
}

impl PdfCraftApp {
    /// Open at the first view's place: the document scrolls to the page and offset the first view was at; the selection follows as the
    /// text of the pages there arrives ([`Self::first_view_busy`]). False when there is no document shown.
    pub fn open_at_first_view(&mut self, selection: &str, y_fraction: f64) -> bool {
        let Some(index) = self.active else { return false };
        let pages = self.views.get(index).and_then(|v| self.session.get(v.id)).map_or(0, |d| d.info.pages.len());
        let Some(view) = self.views.get_mut(index) else { return false };
        let (page, within) = place(pages, y_fraction);
        view.current = page;
        view.goto = Some((page, within));
        view.clear_selection();
        view.first_view_text = selection.lines().map(str::trim).find(|l| !l.is_empty()).map(|t| PendingText { text: t.to_owned(), page, waited: 0 });
        pages > 0
    }

    /// Once per frame after the app's pass: select the first view's text when the text of its page (or the pages beside it) has arrived.
    /// True while still waiting for it (so the hand-over holds `craft:viewed` for it, up to [`TEXT_WAIT_FRAMES`] frames).
    pub fn first_view_busy(&mut self) -> bool {
        let Some(index) = self.active else { return false };
        let Some(view) = self.views.get_mut(index) else { return false };
        let Some(pending) = view.first_view_text.as_mut() else { return false };
        pending.waited += 1;
        let (text, page, waited) = (pending.text.clone(), pending.page, pending.waited);
        // The page itself first, then the next (the top edge may be near its foot), then the one before.
        let mut found = None;
        let mut unread = false;
        for p in [Some(page), page.checked_add(1), page.checked_sub(1)].into_iter().flatten() {
            match view.page_text(p) {
                Some(t) => {
                    if let Some(r) = find_selection(&t, &text) {
                        found = Some((p, r));
                        break;
                    }
                }
                None if !view.text_failed.contains(&p) => unread = true,
                None => {}
            }
        }
        if let Some((p, r)) = found {
            view.select_text(p, r.start, r.end.saturating_sub(1).max(r.start));
            view.first_view_text = None;
            return false;
        }
        if !unread || waited >= TEXT_WAIT_FRAMES {
            view.first_view_text = None;
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfcraft_render::TextGlyph;

    fn glyph(c: char, x: f32) -> TextGlyph {
        TextGlyph {
            text: c.to_string(),
            rect: [x, 0.0, x + 5.0, 10.0],
            quad: [[x, 0.0], [x + 5.0, 0.0], [x + 5.0, 10.0], [x, 10.0]],
            direction: [1.0, 0.0],
        }
    }

    fn line(s: &str) -> PageText {
        PageText {
            glyphs: s.chars().enumerate().map(|(i, c)| glyph(c, i as f32 * 5.0)).collect(),
            line_of: vec![0; s.chars().count()],
            space_before: vec![false; s.chars().count()],
        }
    }

    #[test]
    fn the_page_and_offset_are_the_fraction_of_the_document() {
        assert_eq!(place(10, 0.0), (0, 0.0));
        assert_eq!(place(10, 0.5), (5, 0.0));
        let (page, within) = place(10, 0.25);
        assert_eq!(page, 2);
        assert!((within - 0.5).abs() < 1e-6, "half way down page 3: {within}");
        assert_eq!(place(10, 1.0), (9, 1.0), "the end of the last page");
        assert_eq!(place(10, f64::NAN), (0, 0.0), "hostile numbers are the top");
        assert_eq!(place(0, 0.5), (0, 0.0));
    }

    #[test]
    fn the_selected_text_is_found_by_its_glyph_range_whatever_its_case() {
        let t = line("the quick brown fox");
        assert_eq!(find_selection(&t, "Quick Brown"), Some(4..15));
        assert_eq!(find_selection(&t, "  \n  quick\nnot used"), Some(4..9), "a selection of several lines is placed by its first line");
        assert_eq!(find_selection(&t, "elephant"), None);
        assert_eq!(find_selection(&t, "   "), None);
    }

    fn app_with_pages(pages: usize) -> PdfCraftApp {
        let mut app = PdfCraftApp::new();
        let pdf = pdfcraft_engine::Session::new().create_blank(595.0, 842.0, pages).unwrap().to_vec();
        app.open_bytes("doc.pdf", None, pdf).unwrap();
        app
    }

    fn page_with(text: &str) -> PageText {
        PageText {
            glyphs: text.chars().enumerate().map(|(i, c)| glyph(c, i as f32 * 5.0)).collect(),
            line_of: vec![0; text.chars().count()],
            space_before: vec![false; text.chars().count()],
        }
    }

    #[test]
    fn the_editor_opens_at_the_first_views_page_and_offset() {
        let mut app = app_with_pages(10);
        assert!(app.open_at_first_view("", 0.25));
        let view = &app.views[app.active.unwrap()];
        assert_eq!(view.current, 2);
        let (page, within) = view.goto.expect("the jump is asked for");
        assert_eq!(page, 2);
        assert!((within - 0.5).abs() < 1e-6, "half way down page 3: {within}");
        assert!(!app.first_view_busy(), "no selected text: nothing to wait for");
    }

    #[test]
    fn the_selected_text_is_selected_when_its_pages_text_arrives_and_the_wait_is_bounded() {
        let mut app = app_with_pages(10);
        assert!(app.open_at_first_view("brown fox", 0.5));
        assert!(app.first_view_busy(), "page 6's text has not been read yet");
        let index = app.active.unwrap();
        app.views[index].texts.insert(5, std::sync::Arc::new(page_with("the quick brown fox")));
        assert!(!app.first_view_busy(), "found: nothing more to wait for");
        assert_eq!(app.views[index].selected_text().as_deref(), Some("brown fox"));
        // Text that is not there: the wait ends once every page near the place has been read (or failed).
        assert!(app.open_at_first_view("elephant", 0.5));
        for p in [4, 5, 6] {
            app.views[index].texts.insert(p, std::sync::Arc::new(page_with("the quick brown fox")));
        }
        assert!(!app.first_view_busy(), "read and not there: no selection, no wait");
        assert_eq!(app.views[index].selected_text(), None);
        // Text never read: the wait is bounded.
        assert!(app.open_at_first_view("elephant", 0.9));
        let mut frames = 0;
        while app.first_view_busy() {
            frames += 1;
            assert!(frames <= TEXT_WAIT_FRAMES, "waited {frames} frames");
        }
    }

    #[test]
    fn no_document_has_no_place() {
        let mut app = PdfCraftApp::new();
        assert!(!app.open_at_first_view("x", 0.5));
        assert!(!app.first_view_busy());
    }
}
