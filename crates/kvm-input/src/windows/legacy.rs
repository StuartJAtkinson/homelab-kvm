//! `EnumDisplayMonitors` enumeration: no stable ids or model names, but works
//! where the CCD API doesn't (some remote sessions and virtual adapters).

use kvm_core::{DisplayInfo, Rect};
use windows_sys::Win32::Foundation::{BOOL, LPARAM, RECT, TRUE};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};

const MONITORINFOF_PRIMARY: u32 = 1;

pub fn displays() -> anyhow::Result<Vec<DisplayInfo>> {
    unsafe extern "system" fn collect(mon: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let out = &mut *(data as *mut Vec<DisplayInfo>);
        let mut info: MONITORINFOEXW = std::mem::zeroed();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(mon, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO) != 0 {
            let r = info.monitorInfo.rcMonitor;
            out.push(DisplayInfo {
                index: out.len() as u32,
                id: String::new(),
                name: super::wide_to_string(&info.szDevice),
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
