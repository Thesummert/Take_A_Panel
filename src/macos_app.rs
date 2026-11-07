use anyhow::{Context, Result};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, rc::Retained, sel};
use objc2_foundation::{
    MainThreadMarker, NSAppleEventDescriptor, NSAppleEventManager, NSObject, NSObjectProtocol,
};
use std::{cell::Cell, rc::Rc, time::Duration};

const CORE_EVENT_CLASS: u32 = u32::from_be_bytes(*b"aevt");
const REOPEN_APPLICATION: u32 = u32::from_be_bytes(*b"rapp");

define_class!(
    // SAFETY: NSObject has no subclassing requirements. This object and its
    // callback are confined to AppKit's main thread.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Box<dyn Fn()>]
    struct ReopenTarget;

    // SAFETY: NSObjectProtocol has no additional requirements.
    unsafe impl NSObjectProtocol for ReopenTarget {}

    impl ReopenTarget {
        // SAFETY: This is the handler signature required by NSAppleEventManager.
        #[unsafe(method(handleReopen:withReply:))]
        fn reopen(&self, _event: &NSAppleEventDescriptor, _reply: &NSAppleEventDescriptor) {
            (self.ivars())();
        }
    }
);

pub struct ReopenHandler {
    target: Retained<ReopenTarget>,
    manager: Retained<NSAppleEventManager>,
    registered: Cell<bool>,
}

impl Drop for ReopenHandler {
    fn drop(&mut self) {
        if !self.registered.get() {
            return;
        }
        // SAFETY: Both AppleEvent identifiers are OSType (u32). The manager is
        // alive, and this removes only the handler installed by this object.
        unsafe {
            let _: () = msg_send![
                &*self.manager,
                removeEventHandlerForEventClass: CORE_EVENT_CLASS,
                andEventID: REOPEN_APPLICATION,
            ];
        }
    }
}

/// Receive Finder reopen events without replacing Winit's application delegate.
pub fn install(on_reopen: impl Fn() + 'static) -> Result<Rc<ReopenHandler>> {
    let mtm = MainThreadMarker::new().context("macOS 应用必须在主线程运行")?;
    let allocated = ReopenTarget::alloc(mtm).set_ivars(Box::new(on_reopen));
    // SAFETY: NSObject's init takes no arguments and returns the initialized
    // instance; the ivars have already been set above.
    let target: Retained<ReopenTarget> = unsafe { msg_send![super(allocated), init] };
    let manager = NSAppleEventManager::sharedAppleEventManager();
    let handler = Rc::new(ReopenHandler {
        target,
        manager,
        registered: Cell::new(false),
    });
    let weak = Rc::downgrade(&handler);
    // AppKit installs its default AppleEvent handlers while finishing launch.
    // Register after the event loop starts so it doesn't overwrite ours.
    slint::Timer::single_shot(Duration::ZERO, move || {
        let Some(handler) = weak.upgrade() else {
            return;
        };
        // SAFETY: The target implements this selector with the required two
        // descriptor arguments and stays alive until the handler is removed.
        unsafe {
            let _: () = msg_send![
                &*handler.manager,
                setEventHandler: &*handler.target,
                andSelector: sel!(handleReopen:withReply:),
                forEventClass: CORE_EVENT_CLASS,
                andEventID: REOPEN_APPLICATION,
            ];
        }
        handler.registered.set(true);
    });
    Ok(handler)
}
