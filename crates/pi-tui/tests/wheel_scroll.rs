//! Port of packages/tui/test/wheel-scroll.test.ts

use pi_tui::wheel_scroll::{WheelScrollAccelerator, WheelScrollLines};

fn scroll(accelerator: &WheelScrollAccelerator, times: &[f64], direction: i64) -> Vec<i64> {
    times.iter().map(|&time| accelerator.next(direction, time)).collect()
}

// #9758: fullscreen wheel scrolling was one line per notch on terminals that do not accelerate wheels.
#[test]
fn uses_fixed_line_counts_regardless_of_timing() {
    let accelerator = WheelScrollAccelerator::new(Some(WheelScrollLines::Lines(3.0)), Some(true));
    assert_eq!(scroll(&accelerator, &[0.0, 10.0, 20.0, 1000.0], 1), vec![3, 3, 3, 3]);
    accelerator.set_lines(WheelScrollLines::Lines(0.5));
    assert_eq!(accelerator.next(1, 2000.0), 1);
}

#[test]
fn keeps_one_line_per_event_in_auto_mode_when_terminal_already_accelerates() {
    let accelerator = WheelScrollAccelerator::new(Some(WheelScrollLines::AUTO), Some(false));
    assert_eq!(scroll(&accelerator, &[0.0, 10.0, 20.0, 30.0], 1), vec![1, 1, 1, 1]);
}

#[test]
fn scales_auto_mode_with_wheel_velocity() {
    let accelerator = WheelScrollAccelerator::new(Some(WheelScrollLines::AUTO), Some(true));
    assert_eq!(scroll(&accelerator, &[0.0, 150.0, 300.0, 450.0], 1), vec![1, 1, 1, 1]);
    assert_eq!(
        scroll(&accelerator, &[1000.0, 1050.0, 1100.0, 1150.0], 1),
        vec![1, 2, 2, 2]
    );
    assert_eq!(
        scroll(&accelerator, &[2000.0, 2020.0, 2040.0, 2060.0], 1),
        vec![1, 5, 5, 5]
    );
    assert_eq!(
        scroll(&accelerator, &[3000.0, 3010.0, 3020.0, 3030.0], 1),
        vec![1, 6, 6, 6]
    );
}

#[test]
fn does_not_accelerate_bursts_of_events_for_a_single_notch() {
    let accelerator = WheelScrollAccelerator::new(Some(WheelScrollLines::AUTO), Some(true));
    assert_eq!(scroll(&accelerator, &[0.0, 3.0, 6.0, 9.0], 1), vec![1, 1, 1, 1]);
}

#[test]
fn resets_acceleration_on_direction_changes_and_pauses() {
    let accelerator = WheelScrollAccelerator::new(Some(WheelScrollLines::AUTO), Some(true));
    assert_eq!(scroll(&accelerator, &[0.0, 20.0, 40.0], 1), vec![1, 5, 5]);
    assert_eq!(accelerator.next(-1, 60.0), 1);
    assert_eq!(scroll(&accelerator, &[500.0, 520.0], 1), vec![1, 5]);
}

#[test]
fn carries_fractional_lines_between_events() {
    let accelerator = WheelScrollAccelerator::new(Some(WheelScrollLines::AUTO), Some(true));
    assert_eq!(
        scroll(&accelerator, &[0.0, 40.0, 80.0, 120.0, 160.0], 1),
        vec![1, 2, 3, 2, 3]
    );
}
