#![allow(clippy::let_unit_value)]
use super::window::WindowInner;
use crate::core::promise;
use crate::window::connection::ConnectionOps;
use crate::window::spawn::SPAWN_QUEUE;
use cocoa::appkit::{NSApp, NSApplication, NSApplicationActivationPolicyRegular};
use cocoa::base::{id, nil};
use core_foundation::date::CFAbsoluteTimeGetCurrent;
use core_foundation::runloop::*;
use objc::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::AtomicUsize;

pub struct Connection {
    ns_app: id,
    pub(crate) windows: RefCell<HashMap<usize, Rc<RefCell<WindowInner>>>>,
    pub(crate) next_window_id: AtomicUsize,
}

impl Connection {
    pub(crate) fn create_new() -> anyhow::Result<Self> {
        SPAWN_QUEUE.run();

        unsafe {
            let ns_app = NSApp();
            ns_app.setActivationPolicy_(NSApplicationActivationPolicyRegular);
            let conn = Self {
                ns_app,
                windows: RefCell::new(HashMap::new()),
                next_window_id: AtomicUsize::new(1),
            };
            Ok(conn)
        }
    }

    pub(crate) fn next_window_id(&self) -> usize {
        self.next_window_id.fetch_add(1, ::std::sync::atomic::Ordering::Relaxed)
    }

    pub(crate) fn window_by_id(&self, window_id: usize) -> Option<Rc<RefCell<WindowInner>>> {
        self.windows.borrow().get(&window_id).map(Rc::clone)
    }

    pub(crate) fn with_window_inner<F: FnMut(&mut WindowInner) + Send + 'static>(
        window_id: usize,
        mut f: F,
    ) {
        promise::spawn_into_main_thread(async move {
            if let Some(handle) = Connection::get().unwrap().window_by_id(window_id) {
                let mut inner = handle.borrow_mut();
                f(&mut inner);
            }
        });
    }
}

impl ConnectionOps for Connection {
    fn terminate_message_loop(&self) {
        unsafe {
            let () = msg_send![NSApp(), stop: nil];
        }
    }

    fn run_message_loop(&self) -> anyhow::Result<()> {
        unsafe {
            self.ns_app.run();
        }
        self.windows.borrow_mut().clear();
        Ok(())
    }

    fn schedule_timer<F: FnMut() + 'static>(&self, interval: std::time::Duration, callback: F) {
        let secs_f64 =
            (interval.as_secs() as f64) + (f64::from(interval.subsec_nanos()) / 1_000_000_000_f64);

        let callback = Box::into_raw(Box::new(callback));

        extern "C" fn timer_callback<F: FnMut()>(
            _timer_ref: CFRunLoopTimerRef,
            callback_ptr: *mut std::ffi::c_void,
        ) {
            unsafe {
                let callback: *mut F = callback_ptr as _;
                (*callback)();
            }
        }

        extern "C" fn release_callback<F: FnMut()>(info: *const std::ffi::c_void) {
            let callback: Box<F> = unsafe { Box::from_raw(info as *mut F) };
            drop(callback);
        }

        let timer_ref = unsafe {
            CFRunLoopTimerCreate(
                std::ptr::null(),
                CFAbsoluteTimeGetCurrent() + secs_f64,
                secs_f64,
                0,
                0,
                timer_callback::<F>,
                &mut CFRunLoopTimerContext {
                    copyDescription: None,
                    info: callback as _,
                    release: Some(release_callback::<F>),
                    retain: None,
                    version: 0,
                },
            )
        };

        unsafe {
            CFRunLoopAddTimer(CFRunLoopGetCurrent(), timer_ref, kCFRunLoopCommonModes);
        }
    }
}
