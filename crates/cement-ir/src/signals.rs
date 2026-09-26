//! Minimal signal handling without an async runtime: one `AtomicBool` the
//! hold loop polls. A handler that only sets a flag keeps the release path
//! (`Torch::drop`) on the normal stack instead of inside the handler, where
//! only async-signal-safe operations are legal.
use std::sync::atomic::{AtomicBool, Ordering};

pub static SIGNALLED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_sig: libc::c_int) {
    SIGNALLED.store(true, Ordering::SeqCst);
}

/// Install SIGTERM/SIGINT/SIGHUP handlers. Installation failures are ignored
/// — the ~13 s hardware watchdog still covers the torch if this failed, so a
/// failed install cannot strand the emitter on.
pub fn install() {
    unsafe {
        for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_signal as *const () as usize;
            libc::sigemptyset(&mut sa.sa_mask);
            libc::sigaction(sig, &sa, std::ptr::null_mut());
        }
    }
}
