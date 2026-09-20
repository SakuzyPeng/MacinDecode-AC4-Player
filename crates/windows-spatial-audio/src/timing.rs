//! A scoped timer request for the player's device/control workers.
use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};

pub struct Resolution;
impl Resolution {
    /// # Errors
    /// Returns an error when Windows cannot grant the timer request.
    pub fn new() -> Result<Self, String> {
        // SAFETY: scalar argument; successful requests are balanced by Drop.
        let result = unsafe { timeBeginPeriod(1) };
        if result == 0 {
            Ok(Self)
        } else {
            Err(format!(
                "High resolution timing unavailable ({result}); using system timing"
            ))
        }
    }
}
impl Drop for Resolution {
    fn drop(&mut self) {
        // SAFETY: exactly one release for the successful request owned by this value.
        unsafe {
            timeEndPeriod(1);
        }
    }
}
