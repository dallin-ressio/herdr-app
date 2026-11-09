//! One `WKWebView` per browser pane, attached above GPUI's layer.

use super::{Frame, appkit_origin};
use objc2::rc::Retained;
use objc2::{define_class, msg_send, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly};
use std::cell::Cell;
use objc2_app_kit::{NSEvent, NSView};
use objc2_foundation::{NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSURL, NSURLRequest};
use objc2_web_kit::{WKWebView, WKWebViewConfiguration, WKWebsiteDataStore};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde_json::{Value, json};

struct Ivars {
    pane: String,
}

define_class!(
    // SAFETY: WKWebView can be subclassed. This subclass does not implement Drop;
    // removing it from the window and dropping the Retained releases it.
    #[unsafe(super(WKWebView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct HerdrWebView;

    impl HerdrWebView {
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            // The click lands here, not on GPUI, so the pane would otherwise stay unfocused.
            crate::webview::page_clicked(&self.ivars().pane);
            let _: () = unsafe { msg_send![super(self), mouseDown: event] };
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: &NSEvent) -> bool {
            // The first click should also land in the page, not only activate the window.
            true
        }
    }

    unsafe impl NSObjectProtocol for HerdrWebView {}
);

#[derive(Clone)]
pub struct Native {
    view: Retained<HerdrWebView>,
}

impl Native {
    pub fn open(pane: &str, url: &str) -> Result<Self, String> {
        let mtm = MainThreadMarker::new().ok_or("WebKit has to be used on the main thread")?;
        let config = unsafe { WKWebViewConfiguration::new(mtm) };
        // The default store is persistent and shared, so panes keep one cookie jar.
        unsafe { config.setWebsiteDataStore(&WKWebsiteDataStore::defaultDataStore(mtm)) };
        let allocated = HerdrWebView::alloc(mtm).set_ivars(Ivars { pane: pane.to_owned() });
        let view: Retained<HerdrWebView> =
            unsafe { msg_send![super(allocated), initWithFrame: NSRect::ZERO, configuration: &*config] };
        let native = Native { view };
        unsafe {
            // Leave Web Inspector off: this is a user-facing browser, not a devtools host.
            native.view.setInspectable(false);
            native.view.setAllowsBackForwardNavigationGestures(true);
        }
        native.navigate(url)?;
        Ok(native)
    }

    pub fn navigate(&self, url: &str) -> Result<(), String> {
        let text = NSString::from_str(url);
        let url = NSURL::URLWithString(&text).ok_or_else(|| format!("not a URL: {url}"))?;
        let request = NSURLRequest::requestWithURL(&url);
        unsafe { self.view.loadRequest(&request) };
        Ok(())
    }

    pub fn go_back(&self) -> Result<(), String> {
        unsafe {
            if self.view.canGoBack() {
                self.view.goBack();
            }
        }
        Ok(())
    }

    pub fn go_forward(&self) -> Result<(), String> {
        unsafe {
            if self.view.canGoForward() {
                self.view.goForward();
            }
        }
        Ok(())
    }

    pub fn reload(&self) -> Result<(), String> {
        unsafe { self.view.reload() };
        Ok(())
    }

    pub fn focus(&self) {
        if let Some(window) = self.view.window() {
            window.makeFirstResponder(Some(&self.view));
        }
    }

    pub fn place(&self, frame: Option<Frame>, show: bool) {
        let Ok(parent) = parent_view() else { return };
        if unsafe { self.view.superview() }.is_none() {
            parent.addSubview(&self.view);
        }
        if show
            && let Some(frame) = frame
        {
            let height = parent.bounds().size.height;
            let (x, y) = appkit_origin(height, f64::from(frame.x), f64::from(frame.y), f64::from(frame.height));
            self.view.setFrame(NSRect::new(NSPoint::new(x, y), NSSize::new(f64::from(frame.width), f64::from(frame.height))));
        }
        self.view.setHidden(!show);
    }

    pub fn close(&self) {
        self.view.removeFromSuperview();
        unsafe { self.view.setNavigationDelegate(None) };
    }

    pub fn state(&self) -> Value {
        let url = unsafe { self.view.URL() }
            .and_then(|url| url.absoluteString())
            .map(|s| s.to_string())
            .unwrap_or_default();
        let title = unsafe { self.view.title() }.map(|s| s.to_string()).unwrap_or_default();
        let loading = unsafe { self.view.isLoading() };
        let back = unsafe { self.view.canGoBack() };
        let forward = unsafe { self.view.canGoForward() };
        json!({ "url": url, "title": title, "loading": loading, "canGoBack": back, "canGoForward": forward })
    }
}

pub fn note_window(window: &gpui::Window) {
    let Ok(handle) = HasWindowHandle::window_handle(window) else { return };
    let RawWindowHandle::AppKit(kit) = handle.as_raw() else { return };
    PARENT.with(|slot| slot.set(kit.ns_view.as_ptr().cast()));
}

pub fn blur() {
    let Ok(parent) = parent_view() else { return };
    let Some(win) = parent.window() else { return };
    // Only take the keyboard back when a web view holds it, so an address-bar
    // click is not immediately undone.
    let holds_page = win.firstResponder().is_some_and(|responder| responder.isKindOfClass(WKWebView::class()));
    if holds_page {
        win.makeFirstResponder(Some(&*parent));
    }
}

thread_local! {
    static PARENT: Cell<*mut NSView> = const { Cell::new(std::ptr::null_mut()) };
}

fn parent_view() -> Result<Retained<NSView>, String> {
    let ptr = PARENT.with(|slot| slot.get());
    if ptr.is_null() {
        return Err("no window".into());
    }
    // SAFETY: `note_window` stored GPUI's live content view, and retain only bumps its count.
    unsafe { Retained::retain(ptr) }.ok_or_else(|| "the window view is gone".into())
}
