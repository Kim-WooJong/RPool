//! Restore the existing native window before asking egui to resume its UI.
//! A hidden/minimized window must not need a successful repaint to become visible.
use eframe::egui;
use std::sync::Arc;

/// Shared by the tray callbacks, Dock reopen callback and application logic.
pub(crate) type RestoreWindow = Arc<dyn Fn() + Send + Sync>;

/// Capture eframe's existing window; never create a replacement or restart work.
pub(crate) fn restore_callback(cc: &eframe::CreationContext<'_>) -> RestoreWindow {
    // Global tray handlers must not keep the native window alive after exit.
    let window = cc.winit_window().map(Arc::downgrade);
    let ctx = cc.egui_ctx.clone();
    Arc::new(move || {
        if let Some(window) = window.as_ref().and_then(std::sync::Weak::upgrade) {
            window.set_visible(true);
            window.set_minimized(false);
            window.focus_window();
            // Native redraw is independent of egui's coalesced repaint requests.
            window.request_redraw();
        } else {
            ctx.send_viewport_cmd_to(
                egui::ViewportId::ROOT,
                egui::ViewportCommand::Visible(true),
            );
            ctx.send_viewport_cmd_to(
                egui::ViewportId::ROOT,
                egui::ViewportCommand::Minimized(false),
            );
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Focus);
        }
        ctx.request_repaint_of(egui::ViewportId::ROOT);
    })
}

/// Dock reopen is not forwarded as a winit window event in locked winit 0.30.13.
/// Add only that optional delegate method, preserving winit's delegate/lifecycle.
#[cfg(target_os = "macos")]
pub(crate) mod dock {
    use super::RestoreWindow;
    use std::cell::RefCell;
    use std::ffi::{c_char, c_void};
    use std::sync::Weak;

    /// Objective-C BOOL follows the platform ABI, unlike Rust bool on Intel.
    #[cfg(target_arch = "aarch64")]
    type ObjcBool = bool;
    /// Intel AppKit uses signed char for BOOL.
    #[cfg(not(target_arch = "aarch64"))]
    type ObjcBool = i8;

    thread_local! {
        /// Weak callback: the process-global delegate must not keep a window alive.
        static RESTORE: RefCell<Option<Weak<dyn Fn() + Send + Sync>>> = RefCell::new(None);
    }

    #[link(name = "objc")]
    extern "C" {
        fn objc_getClass(name: *const c_char) -> *mut c_void;
        fn sel_registerName(name: *const c_char) -> *mut c_void;
        fn object_getClass(object: *mut c_void) -> *mut c_void;
        fn class_addMethod(
            class: *mut c_void,
            selector: *mut c_void,
            implementation: *const c_void,
            types: *const c_char,
        ) -> ObjcBool;
        fn objc_msgSend();
    }

    /// AppKit calls this on the main thread; restore without waiting for a UI pass.
    extern "C" fn reopen(
        _delegate: *mut c_void,
        _selector: *mut c_void,
        _app: *mut c_void,
        _visible: ObjcBool,
    ) -> ObjcBool {
        // Never unwind across the Objective-C boundary.
        let _ = std::panic::catch_unwind(|| {
            let restore = RESTORE.with(|slot| slot.borrow().as_ref().and_then(Weak::upgrade));
            if let Some(restore) = restore {
                restore();
            }
        });
        #[cfg(target_arch = "aarch64")]
        { true }
        #[cfg(not(target_arch = "aarch64"))]
        { 1 }
    }

    /// Install while the app creator runs on AppKit's main thread. Do not replace
    /// any existing reopen implementation or the delegate itself.
    pub(crate) fn install(restore: &RestoreWindow) {
        // SAFETY: AppKit has been initialized by eframe. These selectors return
        // Objective-C objects, and all calls and callback registration occur on
        // the main thread. The added method uses AppKit's documented signature.
        unsafe {
            let send: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
                std::mem::transmute(objc_msgSend as *const ());
            let app = send(
                objc_getClass(c"NSApplication".as_ptr()),
                sel_registerName(c"sharedApplication".as_ptr()),
            );
            if app.is_null() {
                return;
            }
            let delegate = send(app, sel_registerName(c"delegate".as_ptr()));
            if delegate.is_null() {
                return;
            }
            let types = if cfg!(target_arch = "aarch64") {
                c"B@:@B"
            } else {
                c"c@:@c"
            };
            RESTORE.with(|slot| *slot.borrow_mut() = Some(std::sync::Arc::downgrade(restore)));
            class_addMethod(
                object_getClass(delegate),
                sel_registerName(c"applicationShouldHandleReopen:hasVisibleWindows:".as_ptr()),
                reopen as *const c_void,
                types.as_ptr(),
            );
        }
    }
}
