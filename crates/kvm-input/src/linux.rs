use std::process::Command;

use anyhow::Context;
use kvm_core::DisplayInfo;

/// Uses `xrandr`, which also works under XWayland on most compositors.
/// TODO: native Wayland via wlr-output-management / KDE / GNOME D-Bus.
pub fn displays() -> anyhow::Result<Vec<DisplayInfo>> {
    let out = Command::new("xrandr")
        .arg("--listmonitors")
        .output()
        .context("running xrandr (is it installed and is DISPLAY set?)")?;
    anyhow::ensure!(out.status.success(), "xrandr failed: {}", String::from_utf8_lossy(&out.stderr));
    Ok(crate::xrandr::parse_listmonitors(&String::from_utf8_lossy(&out.stdout)))
}

pub fn has_battery() -> bool {
    let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") else { return false };
    entries.flatten().any(|e| {
        std::fs::read_to_string(e.path().join("type")).is_ok_and(|t| t.trim() == "Battery")
    })
}
