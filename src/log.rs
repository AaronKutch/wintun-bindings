use crate::{wintun_raw, Wintun};
use std::sync::atomic::{AtomicBool, Ordering};
use widestring::U16CStr;

/// Sets the logger wintun will use when logging. Maps to the WintunSetLogger C function
pub fn set_logger(wintun: &Wintun, f: wintun_raw::WINTUN_LOGGER_CALLBACK) {
    unsafe { wintun.WintunSetLogger(f) };
}

pub fn reset_logger(wintun: &Wintun) {
    set_logger(wintun, None);
}

static SET_LOGGER: AtomicBool = AtomicBool::new(false);

/// The logger that is active by default. Logs messages to the log crate
///
/// # Safety
/// `message` must be a valid pointer that points to an aligned null terminated UTF-16 string
pub unsafe extern "C" fn default_logger(
    level: wintun_raw::WINTUN_LOGGER_LEVEL,
    _timestamp: wintun_raw::DWORD64,
    message: *const wintun_raw::WCHAR,
) {
    //Wintun will always give us a valid UTF16 null termineted string
    let msg = U16CStr::from_ptr_str(message);
    let utf8_msg = msg.to_string_lossy();
    match level {
        wintun_raw::WINTUN_LOGGER_LEVEL_WINTUN_LOG_INFO => log::info!("WinTun: {}", utf8_msg),
        wintun_raw::WINTUN_LOGGER_LEVEL_WINTUN_LOG_WARN => log::warn!("WinTun: {}", utf8_msg),
        wintun_raw::WINTUN_LOGGER_LEVEL_WINTUN_LOG_ERR => log::error!("WinTun: {}", utf8_msg),
        _ => log::error!("WinTun: {} (with invalid log level {})", utf8_msg, level),
    }
}

pub(crate) fn set_default_logger_if_unset(wintun: &Wintun) {
    if SET_LOGGER
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed)
        .is_ok()
    {
        set_logger(wintun, Some(default_logger));
    }
}
