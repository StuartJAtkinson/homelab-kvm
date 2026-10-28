use std::sync::Once;

use kvm_core::{DisplayInfo, Rect};
use windows_sys::Win32::Foundation::{BOOL, LPARAM, RECT, TRUE};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows_sys::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

const MONITORINFOF_PRIMARY: u32 = 1;
const BATTERY_FLAG_NO_BATTERY: u8 = 128;
const BATTERY_FLAG_UNKNOWN: u8 = 255;

/// Without per-monitor DPI awareness Windows reports scaled (virtualised)
/// coordinates, which wouldn't match what the input hooks see.
fn ensure_dpi_aware() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        // Fails harmlessly if the manifest already set it.
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    });
}

pub fn displays() -> anyhow::Result<Vec<DisplayInfo>> {
    ensure_dpi_aware();

    unsafe extern "system" fn collect(mon: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let out = &mut *(data as *mut Vec<DisplayInfo>);
        let mut info: MONITORINFOEXW = std::mem::zeroed();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(mon, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO) != 0 {
            let r = info.monitorInfo.rcMonitor;
            let len = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
            out.push(DisplayInfo {
                index: out.len() as u32,
                name: String::from_utf16_lossy(&info.szDevice[..len]),
                rect: Rect::new(r.left, r.top, r.right - r.left, r.bottom - r.top),
                primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            });
        }
        TRUE
    }

    let mut out: Vec<DisplayInfo> = Vec::new();
    let ok = unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(collect),
            &mut out as *mut Vec<DisplayInfo> as LPARAM,
        )
    };
    anyhow::ensure!(ok != 0, "EnumDisplayMonitors failed: {}", std::io::Error::last_os_error());
    Ok(out)
}

pub fn has_battery() -> bool {
    let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    if unsafe { GetSystemPowerStatus(&mut status) } == 0 {
        return false;
    }
    !matches!(status.BatteryFlag, BATTERY_FLAG_NO_BATTERY | BATTERY_FLAG_UNKNOWN)
}
