// SPDX-License-Identifier: MIT

use std::time::Duration;

use crate::windows::{visible_windows_without_spaces, WindowBounds};

const SETTLE: Duration = Duration::from_secs(8);
const RECHECK: Duration = Duration::from_secs(3);
const TOLERANCE: f64 = 2.0;
const MIN_EDGE: f64 = 80.0;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Placed {
    pub(crate) pid: i32,
    pub(crate) window_id: u32,
    pub(crate) bounds: WindowBounds,
}

fn capture() -> Vec<Placed> {
    let own = std::process::id() as i32;
    visible_windows_without_spaces()
        .into_iter()
        .filter(|w| {
            w.pid != own && w.alpha > 0.0 && w.bounds.width >= MIN_EDGE && w.bounds.height >= MIN_EDGE
        })
        .map(|w| Placed {
            pid: w.pid,
            window_id: w.window_id,
            bounds: w.bounds,
        })
        .collect()
}

fn same_frame(a: &WindowBounds, b: &WindowBounds) -> bool {
    (a.x - b.x).abs() <= TOLERANCE
        && (a.y - b.y).abs() <= TOLERANCE
        && (a.width - b.width).abs() <= TOLERANCE
        && (a.height - b.height).abs() <= TOLERANCE
}

fn overlaps(a: &WindowBounds, b: &WindowBounds) -> bool {
    a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
}

pub(crate) fn windows_to_restore(
    before: &[Placed],
    current: &[Placed],
    excluded: &[WindowBounds],
) -> Vec<Placed> {
    before
        .iter()
        .filter(|then| {
            current
                .iter()
                .find(|now| now.window_id == then.window_id)
                .is_some_and(|now| {
                    !same_frame(&then.bounds, &now.bounds)
                        && !excluded
                            .iter()
                            .any(|rect| overlaps(rect, &then.bounds) || overlaps(rect, &now.bounds))
                })
        })
        .cloned()
        .collect()
}

fn display_rects() -> Vec<WindowBounds> {
    crate::virtual_display::list()
        .into_iter()
        .map(|d| WindowBounds {
            x: d.x,
            y: d.y,
            width: d.width,
            height: d.height,
        })
        .collect()
}

pub(crate) fn around_display_change<T>(change: impl FnOnce() -> T) -> T {
    let before = capture();
    let mut excluded = display_rects();
    let out = change();
    excluded.extend(display_rects());
    std::thread::spawn(move || restore_after_settle(before, excluded));
    out
}

fn restore_after_settle(before: Vec<Placed>, excluded: Vec<WindowBounds>) {
    for wait in [SETTLE, RECHECK] {
        std::thread::sleep(wait);
        for window in windows_to_restore(&before, &capture(), &excluded) {
            let _ = crate::tools::restore_window_frame(window.pid, window.window_id, &window.bounds);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(window_id: u32, x: f64, width: f64) -> Placed {
        Placed {
            pid: 1,
            window_id,
            bounds: WindowBounds { x, y: 39.0, width, height: 900.0 },
        }
    }

    fn rect(x: f64) -> WindowBounds {
        WindowBounds { x, y: 0.0, width: 1920.0, height: 1080.0 }
    }

    #[test]
    fn a_window_pushed_aside_by_the_display_change_is_restored() {
        let before = [at(1, 0.0, 2056.0), at(2, 100.0, 800.0)];
        let after = [at(1, 686.0, 1370.0), at(2, 100.0, 800.0)];
        let restore = windows_to_restore(&before, &after, &[]);
        assert_eq!(restore, vec![at(1, 0.0, 2056.0)]);
    }

    #[test]
    fn a_window_the_agent_moved_onto_the_new_display_is_left_alone() {
        let before = [at(1, 100.0, 800.0)];
        let after = [at(1, -1700.0, 800.0)];
        assert!(windows_to_restore(&before, &after, &[rect(-1920.0)]).is_empty());
    }

    #[test]
    fn closed_windows_and_unchanged_windows_are_ignored() {
        let before = [at(1, 100.0, 800.0), at(2, 0.0, 500.0)];
        let after = [at(2, 1.0, 501.0)];
        assert!(windows_to_restore(&before, &after, &[]).is_empty());
    }
}
