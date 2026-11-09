//! One `WKWebView` per browser pane, attached above GPUI's layer.

use super::{EditingCommand, Frame, appkit_origin, page_editing_command};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, Sel};
use objc2::{define_class, msg_send, ClassType, DefinedClass, MainThreadMarker, MainThreadOnly};
use std::cell::Cell;
use objc2_app_kit::{NSAutoresizingMaskOptions, NSEvent, NSEventModifierFlags, NSResponder, NSView};
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

        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> Bool {
            // WebKit handles the chord when this view is first responder, then
            // re-sends it if the page did not. Swallow that re-send for editing
            // so it cannot reach GPUI or the app menu. The menu's copy: and
            // paste: read a menu-item tag; a page key arrives with the wrong
            // sender and quits the app.
            thread_local! {
                static BUSY: Cell<bool> = const { Cell::new(false) };
            }
            if BUSY.with(|guard| guard.replace(true)) {
                return false.into();
            }
            let handled: Bool = unsafe { msg_send![super(self), performKeyEquivalent: event] };
            let keep = handled.as_bool() || editing_command(event).is_some();
            BUSY.with(|guard| guard.set(false));
            keep.into()
        }
    }

    unsafe impl NSObjectProtocol for HerdrWebView {}
);

define_class!(
    // Sits between the page and GPUI. WebKit forwards keys it already delivered
    // to the next responder, and unanswered editing commands walk that same
    // chain to the app delegate. This view ends both walks.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ()]
    struct KeySink;

    impl KeySink {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            false
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, _event: &NSEvent) {}

        #[unsafe(method(keyUp:))]
        fn key_up(&self, _event: &NSEvent) {}

        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, _event: &NSEvent) -> Bool {
            false.into()
        }

        #[unsafe(method(doCommandBySelector:))]
        fn do_command(&self, _selector: Sel) {}

        #[unsafe(method(copy:))]
        fn copy_cmd(&self, _sender: Option<&AnyObject>) {}

        #[unsafe(method(cut:))]
        fn cut_cmd(&self, _sender: Option<&AnyObject>) {}

        #[unsafe(method(paste:))]
        fn paste_cmd(&self, _sender: Option<&AnyObject>) {}

        #[unsafe(method(selectAll:))]
        fn select_all_cmd(&self, _sender: Option<&AnyObject>) {}

        #[unsafe(method(undo:))]
        fn undo_cmd(&self, _sender: Option<&AnyObject>) {}

        #[unsafe(method(redo:))]
        fn redo_cmd(&self, _sender: Option<&AnyObject>) {}
    }

    unsafe impl NSObjectProtocol for KeySink {}
);

#[derive(Clone)]
pub struct Native {
    view: Retained<HerdrWebView>,
    sink: Retained<KeySink>,
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
        let sink = KeySink::alloc(mtm).set_ivars(());
        let sink: Retained<KeySink> = unsafe { msg_send![super(sink), initWithFrame: NSRect::ZERO] };
        let native = Native { view, sink };
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
        if unsafe { self.sink.superview() }.is_none() {
            parent.addSubview(&self.sink);
            self.sink.addSubview(&self.view);
            self.view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
        }
        if show
            && let Some(frame) = frame
        {
            let height = parent.bounds().size.height;
            let (x, y) = appkit_origin(height, f64::from(frame.x), f64::from(frame.y), f64::from(frame.height));
            self.sink.setFrame(NSRect::new(NSPoint::new(x, y), NSSize::new(f64::from(frame.width), f64::from(frame.height))));
            self.view.setFrame(self.sink.bounds());
        }
        self.sink.setHidden(!show);
        self.view.setHidden(!show);
    }

    pub fn close(&self) {
        self.sink.removeFromSuperview();
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
    // click is not immediately undone. The first responder is usually a private
    // subview (WKContentView), not the WKWebView itself.
    if win.firstResponder().is_some_and(|responder| responder_in_page(&responder)) {
        win.makeFirstResponder(Some(&*parent));
    }
}

pub fn page_holds_keyboard() -> bool {
    let Ok(parent) = parent_view() else { return false };
    let Some(win) = parent.window() else { return false };
    win.firstResponder().is_some_and(|responder| responder_in_page(&responder))
}

fn responder_in_page(responder: &NSResponder) -> bool {
    if responder.isKindOfClass(WKWebView::class()) {
        return true;
    }
    let Some(view) = responder.downcast_ref::<NSView>() else { return false };
    let mut parent = unsafe { view.superview() };
    while let Some(next) = parent {
        if next.isKindOfClass(WKWebView::class()) {
            return true;
        }
        parent = unsafe { next.superview() };
    }
    false
}

fn editing_command(event: &NSEvent) -> Option<EditingCommand> {
    let flags = event.modifierFlags();
    let key = event.charactersIgnoringModifiers().map(|s| s.to_string()).unwrap_or_default();
    page_editing_command(
        flags.contains(NSEventModifierFlags::Command),
        flags.contains(NSEventModifierFlags::Shift),
        flags.contains(NSEventModifierFlags::Control),
        flags.contains(NSEventModifierFlags::Option),
        flags.contains(NSEventModifierFlags::Function),
        &key.to_lowercase(),
    )
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
