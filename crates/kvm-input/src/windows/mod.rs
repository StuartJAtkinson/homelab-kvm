mod capture;
mod displays;
mod inject;
mod legacy;

use std::sync::Once;

use kvm_core::DisplayInfo;
use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows_sys::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

pub use capture::WinCapture;
pub use inject::WinInjector;

const BATTERY_FLAG_NO_BATTERY: u8 = 128;
const BATTERY_FLAG_UNKNOWN: u8 = 255;

/// Without per-monitor DPI awareness Windows reports scaled (virtualised)
/// coordinates for displays, hooks and `SendInput`, and they'd disagree.
pub(crate) fn ensure_dpi_aware() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        // Fails harmlessly if the manifest already set it.
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    });
}

pub(crate) fn wide_to_string(w: &[u16]) -> String {
    let len = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..len])
}

pub fn displays() -> anyhow::Result<Vec<DisplayInfo>> {
    ensure_dpi_aware();
    match displays::displays() {
        Ok(d) if !d.is_empty() => Ok(d),
        Ok(_) => legacy::displays(),
        Err(e) => {
            tracing::debug!("CCD display query failed ({e:#}), falling back");
            legacy::displays()
        }
    }
}

pub fn has_battery() -> bool {
    let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    if unsafe { GetSystemPowerStatus(&mut status) } == 0 {
        return false;
    }
    !matches!(status.BatteryFlag, BATTERY_FLAG_NO_BATTERY | BATTERY_FLAG_UNKNOWN)
}

pub fn cursor_position() -> Option<kvm_core::Point> {
    let mut p = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
    (unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut p) } != 0)
        .then(|| kvm_core::Point::new(p.x, p.y))
}
