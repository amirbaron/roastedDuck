//! Ctrl-C handling (`InterruptHandler` and `ShellState::seenInterrupt`): an interrupt
//! stops printing and interrupts the running query; a third one exits.

use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

use rustdb::ClientHandle;

/// `ShellState::seenInterrupt`: the interrupts since the last `ClearInterrupt`.
static SEEN_INTERRUPT: AtomicU64 = AtomicU64::new(0);

/// The handle of the shell's connection (`state.conn->Interrupt()`), owned by
/// [`set_connection`]'s caller while it is set.
static CLIENT: AtomicPtr<ClientHandle> = AtomicPtr::new(std::ptr::null_mut());

/// Whether an interrupt was seen since the last [`clear`].
pub fn seen_interrupt() -> bool {
    SEEN_INTERRUPT.load(Ordering::Acquire) > 0
}

/// Records an interrupt, as `InterruptHandler` does for SIGPIPE while a pager runs.
pub fn record() -> u64 {
    SEEN_INTERRUPT.fetch_add(1, Ordering::AcqRel) + 1
}

/// Forgets the interrupts (the first half of `ShellState::ClearInterrupt`).
pub fn clear() {
    SEEN_INTERRUPT.store(0, Ordering::Release);
}

/// The connection whose queries Ctrl-C interrupts; the handle must stay alive until it is
/// replaced or cleared.
pub fn set_connection(client: Option<&Arc<ClientHandle>>) {
    let pointer = client.map_or(std::ptr::null_mut(), |client| Arc::as_ptr(client).cast_mut());
    CLIENT.store(pointer, Ordering::Release);
}

/// `InterruptHandler`.
extern "C" fn interrupt_handler(_signal: libc::c_int) {
    if record() > 2 {
        crate::state::exit(1);
    }
    let client = CLIENT.load(Ordering::Acquire);
    // SAFETY: the pointer is cleared or replaced before the handle it points to is dropped.
    if let Some(client) = unsafe { client.as_ref() } {
        client.interrupt();
    }
}

/// Installs the handler for `signal` (`signal(SIGINT, InterruptHandler)`).
pub fn install(signal: libc::c_int) {
    let handler: extern "C" fn(libc::c_int) = interrupt_handler;
    // SAFETY: the handler only touches atomics (and exits after the third interrupt).
    unsafe {
        libc::signal(signal, handler as libc::sighandler_t);
    }
}

/// Restores the default action for `signal` (`signal(signal, SIG_DFL)`).
pub fn restore_default(signal: libc::c_int) {
    // SAFETY: `SIG_DFL` is a valid disposition for every catchable signal.
    unsafe {
        libc::signal(signal, libc::SIG_DFL);
    }
}

/// Ignores `signal` (`signal(signal, SIG_IGN)`).
pub fn ignore(signal: libc::c_int) {
    // SAFETY: `SIG_IGN` is a valid disposition for every catchable signal.
    unsafe {
        libc::signal(signal, libc::SIG_IGN);
    }
}
