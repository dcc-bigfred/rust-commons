//! Optional SIGHUP → reload (microwaf).

use std::io;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// Register SIGHUP so the flag becomes `true` when the process is hung up.
/// Combine with [`super::spawn_callback`] / a poll of the flag.
///
/// # Errors
///
/// Returns IO error if the signal cannot be registered.
pub fn install_sighup_flag(flag: Arc<AtomicBool>) -> io::Result<()> {
    signal_hook::flag::register(signal_hook::consts::SIGHUP, flag)?;
    Ok(())
}
