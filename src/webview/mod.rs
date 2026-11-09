//! Native web views a mod can place over a pane.
//!
//! GPUI paints the window into one layer-backed view, so a browser cannot be
//! another GPUI element. On macOS each web view is a `WKWebView` subview of
//! that view, above the GPU layer, and this module moves it to the rectangle
//! the mod measured. Dialogs and popups are painted into the GPU layer, which
//! sits underneath those subviews, so the web views hide while one is open.

use gpui::Window;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::HashSet;

#[cfg(target_os = "macos")]
mod macos;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// AppKit view frames start at the bottom left. `parent_height` is the GPUI view's.
pub fn appkit_origin(parent_height: f64, x: f64, y: f64, height: f64) -> (f64, f64) {
    (x, parent_height - y - height)
}

/// A web view is on screen only while its pane was laid out, nothing modal
/// covers the window, and the rectangle can actually show a page.
pub fn view_visible(obscured: bool, laid_out: &HashSet<String>, pane: &str, frame: Option<Frame>) -> bool {
    !obscured && laid_out.contains(pane) && frame.is_some_and(|f| f.width >= 1.0 && f.height >= 1.0)
}

pub fn allowed_url(url: &str) -> bool {
    let url = url.trim();
    if url.eq_ignore_ascii_case("about:blank") {
        return true;
    }
    let Some((scheme, rest)) = url.split_once(':') else { return false };
    if !rest.starts_with("//") {
        return false;
    }
    scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
}

struct Host {
    next: u64,
    /// Panes whose nodes were laid out during this frame.
    laid_out: HashSet<String>,
    obscured: bool,
    /// Set from a click in the page; the next frame gives that web view the keyboard
    /// after GPUI takes it back for the pane focus change.
    clicked: Option<String>,
    views: Vec<View>,
}

struct View {
    id: String,
    pane: String,
    frame: Option<Frame>,
    #[cfg(target_os = "macos")]
    native: macos::Native,
}

impl Host {
    fn view_mut(&mut self, id: &str) -> Option<&mut View> {
        self.views.iter_mut().find(|v| v.id == id)
    }
}

thread_local! {
    static HOST: RefCell<Host> = RefCell::new(Host {
        next: 0,
        laid_out: HashSet::new(),
        obscured: false,
        clicked: None,
        views: Vec::new(),
    });
}

fn with_host<R>(f: impl FnOnce(&mut Host) -> R) -> R {
    HOST.with(|host| f(&mut host.borrow_mut()))
}

/// Remembers the window's content view so a script can move a web view later.
pub fn note_window(window: &Window) {
    #[cfg(target_os = "macos")]
    macos::note_window(window);
    #[cfg(not(target_os = "macos"))]
    let _ = window;
}

/// Clears the set of panes laid out this frame. Call at the start of a draw.
pub fn begin_frame() {
    with_host(|host| host.laid_out.clear());
}

/// `id` is a layout node that was measured this frame. Mod panes use their pane id.
pub fn note_visible(id: &str) {
    with_host(|host| {
        host.laid_out.insert(id.to_owned());
    });
}

/// Dialogs and popups paint under the web views, so hide them while one is up.
pub fn set_obscured(obscured: bool) {
    with_host(|host| host.obscured = obscured);
}

/// Shows each web view that was laid out, and hides the rest.
pub fn finish_frame() {
    let plan: Vec<(String, Option<Frame>, bool)> = with_host(|host| {
        host.views
            .iter()
            .map(|view| {
                let show = view_visible(host.obscured, &host.laid_out, &view.pane, view.frame);
                (view.id.clone(), view.frame, show)
            })
            .collect()
    });
    for (id, frame, show) in plan {
        place(&id, frame, show);
    }
}

/// The pane whose page was clicked and still wants the keyboard.
pub fn keyboard_pane() -> Option<String> {
    with_host(|host| host.clicked.clone())
}

/// Drops every web view attached to a layout slot. Safe to call twice.
pub fn destroy_pane(pane: &str) {
    let gone = with_host(|host| {
        let mut keep = Vec::new();
        let mut gone = Vec::new();
        for view in host.views.drain(..) {
            if view.pane == pane {
                gone.push(view);
            } else {
                keep.push(view);
            }
        }
        host.views = keep;
        gone
    });
    #[cfg(target_os = "macos")]
    for view in gone {
        view.native.close();
    }
    #[cfg(not(target_os = "macos"))]
    let _ = gone;
}

/// A click in the page asked for the keyboard. GPUI focuses the window when the
/// pane changes, which would take it back; put it on the web view afterwards.
pub fn restore_keyboard(window: &mut Window) {
    let pane = with_host(|host| host.clicked.take());
    #[cfg(target_os = "macos")]
    {
        let _ = window;
        if let Some(pane) = pane {
            let native = with_host(|host| host.views.iter().find(|v| v.pane == pane).map(|v| v.native.clone()));
            if let Some(native) = native {
                native.focus();
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (pane, window);
}

pub fn create(pane: String, url: Option<String>) -> Result<String, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (pane, url);
        return Err("WebKit views are only available on macOS".into());
    }
    #[cfg(target_os = "macos")]
    {
        let initial = url.as_deref().unwrap_or("about:blank");
        if !allowed_url(initial) {
            return Err("only http and https addresses can be opened".into());
        }
        // Open the view before borrowing the host: AppKit can call back into us.
        let id = with_host(|host| {
            host.next += 1;
            format!("w{}", host.next)
        });
        let native = macos::Native::open(&pane, initial)?;
        with_host(|host| host.views.push(View { id: id.clone(), pane, frame: None, native }));
        Ok(id)
    }
}

pub fn navigate(id: &str, url: &str) -> Result<(), String> {
    if !allowed_url(url) {
        return Err("only http and https addresses can be opened".into());
    }
    #[cfg(target_os = "macos")]
    {
        let native = native(id)?;
        native.navigate(url)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (id, url);
        Err("WebKit views are only available on macOS".into())
    }
}

pub fn go_back(id: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        native(id)?.go_back()
    }
    #[cfg(not(target_os = "macos"))]
    unsupported(id)
}

pub fn go_forward(id: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        native(id)?.go_forward()
    }
    #[cfg(not(target_os = "macos"))]
    unsupported(id)
}

pub fn reload(id: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        native(id)?.reload()
    }
    #[cfg(not(target_os = "macos"))]
    unsupported(id)
}

pub fn focus(id: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        native(id)?.focus();
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    unsupported(id)
}

pub fn blur(id: &str) -> Result<(), String> {
    let known = with_host(|host| {
        let known = host.views.iter().any(|v| v.id == id);
        if known {
            host.clicked = None;
        }
        known
    });
    if !known {
        return Err("no web view".into());
    }
    #[cfg(target_os = "macos")]
    macos::blur();
    Ok(())
}

pub fn set_frame(id: &str, frame: Frame) -> Result<(), String> {
    let show = with_host(|host| {
        let pane = {
            let Some(view) = host.view_mut(id) else { return Err("no web view".to_owned()) };
            view.frame = Some(frame);
            view.pane.clone()
        };
        Ok(!host.obscured && host.laid_out.contains(&pane) && frame.width >= 1.0 && frame.height >= 1.0)
    })?;
    place(id, Some(frame), show);
    Ok(())
}

pub fn destroy(id: &str) {
    let view = with_host(|host| {
        let Some(i) = host.views.iter().position(|v| v.id == id) else { return None };
        Some(host.views.remove(i))
    });
    #[cfg(target_os = "macos")]
    if let Some(view) = view {
        view.native.close();
    }
    #[cfg(not(target_os = "macos"))]
    let _ = view;
}

pub fn state(id: &str) -> Option<Value> {
    #[cfg(target_os = "macos")]
    {
        let native = with_host(|host| host.views.iter().find(|v| v.id == id).map(|v| v.native.clone()))?;
        Some(native.state())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = id;
        None
    }
}

/// The page was clicked. Remember it so `restore_keyboard` can give it the keys
/// after the pane-focus update moves them to the GPUI view.
pub fn page_clicked(pane: &str) {
    with_host(|host| host.clicked = Some(pane.to_owned()));
    crate::mods::request(crate::mods::Request::Run { name: "pane.focus".into(), args: json!({ "pane": pane }) });
}

fn place(id: &str, frame: Option<Frame>, show: bool) {
    #[cfg(target_os = "macos")]
    if let Ok(native) = native(id) {
        native.place(frame, show);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (id, frame, show);
}

#[cfg(target_os = "macos")]
fn native(id: &str) -> Result<macos::Native, String> {
    with_host(|host| host.views.iter().find(|v| v.id == id).map(|v| v.native.clone()).ok_or_else(|| "no web view".into()))
}

#[cfg(not(target_os = "macos"))]
fn unsupported(id: &str) -> Result<(), String> {
    let _ = id;
    Err("WebKit views are only available on macOS".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appkit_y_grows_up_from_the_bottom() {
        assert_eq!(appkit_origin(800.0, 12.0, 30.0, 100.0), (12.0, 670.0));
    }

    #[test]
    fn a_view_shows_only_in_its_laid_out_pane() {
        let frame = Frame { x: 1.0, y: 2.0, width: 40.0, height: 50.0 };
        let mut laid = HashSet::new();
        assert!(!view_visible(false, &laid, "p", Some(frame)));
        laid.insert("p".into());
        assert!(view_visible(false, &laid, "p", Some(frame)));
        assert!(!view_visible(true, &laid, "p", Some(frame)));
        assert!(!view_visible(false, &laid, "other", Some(frame)));
        assert!(!view_visible(false, &laid, "p", Some(Frame { width: 0.0, ..frame })));
        assert!(!view_visible(false, &laid, "p", None));
    }

    #[test]
    fn only_http_and_https_pages_open() {
        assert!(allowed_url("https://example.com"));
        assert!(allowed_url("http://localhost:3000/a"));
        assert!(allowed_url("about:blank"));
        assert!(!allowed_url("file:///etc/passwd"));
        assert!(!allowed_url("javascript:alert(1)"));
        assert!(!allowed_url("example.com"));
        assert!(!allowed_url("https:example.com"));
    }
}
