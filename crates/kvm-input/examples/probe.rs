//! Sanity-check the platform layer on a real machine:
//! `cargo run -p kvm-input --example probe`.
//! Moves the cursor briefly (then puts it back); injects no clicks or keys.

use std::time::Duration;

use kvm_core::engine::LocalEvent;
use kvm_core::Point;

fn main() -> anyhow::Result<()> {
    println!("battery: {}", kvm_input::has_battery());
    for d in kvm_input::displays()? {
        println!("display {}: {:?} id={:?} rect={:?} primary={}", d.index, d.name, d.id, d.rect, d.primary);
    }

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let capture = kvm_input::start_capture(tx)?;
    let mut inj = kvm_input::injector()?;
    let home = kvm_input::cursor_position().expect("cursor position");
    std::thread::sleep(Duration::from_millis(100));
    while rx.try_recv().is_ok() {} // drop anything the user did meanwhile

    let mut ok = true;
    for p in [Point::new(100, 100), Point::new(500, 300), Point::new(1, 1)] {
        inj.warp(p)?;
        std::thread::sleep(Duration::from_millis(50));
        let got = kvm_input::cursor_position().unwrap();
        println!("warp {p:?} -> cursor at {got:?}");
        ok &= got == p;
    }
    let echoed = std::iter::from_fn(|| rx.try_recv().ok())
        .filter(|e| matches!(e, LocalEvent::Motion { .. }))
        .count();
    println!("motion events reported for injected moves: {echoed} (expect 0 unless you moved the mouse)");

    capture.set_grab(true);
    std::thread::sleep(Duration::from_millis(50));
    let parked = kvm_input::cursor_position().unwrap();
    std::thread::sleep(Duration::from_millis(250));
    capture.set_grab(false);
    println!("grabbed: cursor parked at {parked:?}");

    inj.warp(home)?;
    drop(capture);
    println!("restored cursor to {home:?}; warps exact: {ok}");
    Ok(())
}
