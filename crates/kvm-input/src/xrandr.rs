//! Parser for `xrandr --listmonitors`. Platform-independent so it's tested
//! everywhere; only used on Linux.

#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use kvm_core::{DisplayInfo, Rect};

/// Parses lines like ` 0: +*DP-1 2560/597x1440/336+0+0  DP-1`.
pub fn parse_listmonitors(out: &str) -> Vec<DisplayInfo> {
    out.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<DisplayInfo> {
    let mut parts = line.split_whitespace();
    let index = parts.next()?.strip_suffix(':')?.parse().ok()?;
    let flags_name = parts.next()?;
    let geometry = parts.next()?;

    let name = flags_name.trim_start_matches(['+', '*']);
    let primary = flags_name.contains('*');

    // "2560/597x1440/336+0+0": width/mm x height/mm + x + y
    let (size, pos) = geometry.split_once('+')?;
    let (x, y) = pos.split_once('+')?;
    let (w, h) = size.split_once('x')?;
    let px = |s: &str| s.split('/').next()?.parse::<i32>().ok();
    let rect = Rect::new(x.parse().ok()?, y.parse().ok()?, px(w)?, px(h)?);

    Some(DisplayInfo { index, name: name.to_string(), rect, primary })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_two_monitors() {
        let out = "Monitors: 2\n 0: +*DP-1 2560/597x1440/336+0+0  DP-1\n 1: +HDMI-1 1920/527x1080/296+2560+180  HDMI-1\n";
        let ds = parse_listmonitors(out);
        assert_eq!(ds.len(), 2);
        assert_eq!(ds[0].name, "DP-1");
        assert!(ds[0].primary);
        assert_eq!(ds[0].rect, Rect::new(0, 0, 2560, 1440));
        assert_eq!(ds[1].name, "HDMI-1");
        assert!(!ds[1].primary);
        assert_eq!(ds[1].rect, Rect::new(2560, 180, 1920, 1080));
    }
}
