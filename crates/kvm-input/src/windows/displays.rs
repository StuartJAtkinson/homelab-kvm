//! Display enumeration through the CCD API (`QueryDisplayConfig`), the same
//! source the Windows display settings page uses. Each monitor's EDID is read
//! from the registry so it gets the same id Windows uses to key its saved
//! arrangements under `GraphicsDrivers\Configuration`.

use anyhow::bail;
use kvm_core::{DisplayInfo, Rect};
use windows_sys::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_TARGET_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
};
use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_BINARY};

use crate::edid::Edid;

pub fn displays() -> anyhow::Result<Vec<DisplayInfo>> {
    let (paths, modes) = query()?;
    let mut out: Vec<DisplayInfo> = Vec::new();
    let mut seen_sources = Vec::new();

    for path in &paths {
        // In clone mode several targets share one source; it's one desktop area.
        let source = (path.sourceInfo.adapterId.LowPart, path.sourceInfo.adapterId.HighPart, path.sourceInfo.id);
        if seen_sources.contains(&source) {
            continue;
        }
        let idx = unsafe { path.sourceInfo.Anonymous.modeInfoIdx } as usize;
        let Some(mode) = modes.get(idx).filter(|m| m.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE) else {
            continue;
        };
        seen_sources.push(source);
        let sm = unsafe { mode.Anonymous.sourceMode };
        let rect = Rect::new(sm.position.x, sm.position.y, sm.width as i32, sm.height as i32);

        let target = target_name(path);
        let edid = target.as_ref().and_then(|t| read_edid(&t.device_path));
        let id = match (&edid, &target) {
            (Some(e), _) => e.monitor_id(),
            (None, Some(t)) => t.device_path.clone(),
            (None, None) => String::new(),
        };
        let name = target
            .as_ref()
            .map(|t| t.friendly.clone())
            .filter(|n| !n.is_empty())
            .or_else(|| edid.as_ref().and_then(|e| e.model.clone()))
            .unwrap_or_else(|| format!("Display {}", out.len() + 1));

        out.push(DisplayInfo {
            index: out.len() as u32,
            id,
            name,
            // The primary monitor's top-left is the desktop origin.
            primary: rect.x == 0 && rect.y == 0,
            rect,
        });
    }
    Ok(out)
}

fn query() -> anyhow::Result<(Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>)> {
    // The configuration can change between sizing and querying; retry then.
    for _ in 0..3 {
        let (mut np, mut nm) = (0u32, 0u32);
        let rc = unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) };
        if rc != ERROR_SUCCESS {
            bail!("GetDisplayConfigBufferSizes failed: {rc}");
        }
        let mut paths: Vec<DISPLAYCONFIG_PATH_INFO> = vec![unsafe { std::mem::zeroed() }; np as usize];
        let mut modes: Vec<DISPLAYCONFIG_MODE_INFO> = vec![unsafe { std::mem::zeroed() }; nm as usize];
        let rc = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut np,
                paths.as_mut_ptr(),
                &mut nm,
                modes.as_mut_ptr(),
                std::ptr::null_mut(),
            )
        };
        match rc {
            ERROR_SUCCESS => {
                paths.truncate(np as usize);
                modes.truncate(nm as usize);
                return Ok((paths, modes));
            }
            ERROR_INSUFFICIENT_BUFFER => continue,
            _ => bail!("QueryDisplayConfig failed: {rc}"),
        }
    }
    bail!("display configuration kept changing")
}

struct TargetName {
    friendly: String,
    /// e.g. `\\?\DISPLAY#DELA10F#5&2a6a1b0&0&UID4353#{e6f07b5f-...}`.
    device_path: String,
}

fn target_name(path: &DISPLAYCONFIG_PATH_INFO) -> Option<TargetName> {
    let mut name: DISPLAYCONFIG_TARGET_DEVICE_NAME = unsafe { std::mem::zeroed() };
    name.header = DISPLAYCONFIG_DEVICE_INFO_HEADER {
        r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
        size: std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
        adapterId: path.targetInfo.adapterId,
        id: path.targetInfo.id,
    };
    if unsafe { DisplayConfigGetDeviceInfo(&mut name.header) } != 0 {
        return None;
    }
    Some(TargetName {
        friendly: super::wide_to_string(&name.monitorFriendlyDeviceName),
        device_path: super::wide_to_string(&name.monitorDevicePath),
    })
}

/// The device path names the monitor's device-instance key, which holds the
/// raw EDID: `\\?\DISPLAY#<hw id>#<instance>#{guid}` ->
/// `HKLM\SYSTEM\CurrentControlSet\Enum\DISPLAY\<hw id>\<instance>\Device Parameters\EDID`.
fn read_edid(device_path: &str) -> Option<Edid> {
    let mut parts = device_path.trim_start_matches(r"\\?\").split('#');
    let (class, hw, instance) = (parts.next()?, parts.next()?, parts.next()?);
    let key = format!(r"SYSTEM\CurrentControlSet\Enum\{class}\{hw}\{instance}\Device Parameters");
    let key: Vec<u16> = key.encode_utf16().chain([0]).collect();
    let value: Vec<u16> = "EDID".encode_utf16().chain([0]).collect();

    let mut buf = vec![0u8; 1024];
    let mut len = buf.len() as u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_BINARY,
            std::ptr::null_mut(),
            buf.as_mut_ptr().cast(),
            &mut len,
        )
    };
    if rc != ERROR_SUCCESS {
        return None;
    }
    Edid::parse(&buf[..len as usize])
}
