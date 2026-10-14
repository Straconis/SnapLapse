//! Auto-hide at a screen edge, like FastStone Capture's toolbar.
//!
//! Drag the window against an edge of the screen and it docks there. When the mouse leaves it,
//! it hides and leaves a small red marker on that edge. Pointing at the marker brings it back.
//!
//! This file holds only the decisions, as a small state machine fed with plain numbers, so it can
//! be tested on any computer. The Windows code that reads the mouse and moves windows lives in
//! `dock_windows.rs`.

/// How close (in pixels) the window must be to an edge to dock there.
const SNAP_DISTANCE: i32 = 12;
/// How long the mouse must stay away before the window hides, in milliseconds.
const HIDE_DELAY_MS: u64 = 700;
/// Extra room around the window that still counts as "on the window", so a slightly shaky
/// mouse doesn't hide it.
const WINDOW_MARGIN: i32 = 8;
/// Extra room around the marker that still counts as pointing at it.
const MARKER_MARGIN: i32 = 6;
/// The marker's length along the edge, and its thickness.
pub const MARKER_LENGTH: i32 = 64;
pub const MARKER_THICKNESS: i32 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }

    fn grown(&self, by: i32) -> Rect {
        Rect {
            left: self.left - by,
            top: self.top - by,
            right: self.right + by,
            bottom: self.bottom + by,
        }
    }

    fn contains(&self, (x, y): (i32, i32)) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

/// Which edge of the screen area `window` is touching, if any. Top and bottom win over the
/// sides when the window is in a corner, since a wide window most likely meant those.
pub fn touching_edge(window: Rect, screen: Rect) -> Option<Edge> {
    let near = |a: i32, b: i32| (a - b).abs() <= SNAP_DISTANCE;
    if near(window.top, screen.top) {
        Some(Edge::Top)
    } else if near(window.bottom, screen.bottom) {
        Some(Edge::Bottom)
    } else if near(window.left, screen.left) {
        Some(Edge::Left)
    } else if near(window.right, screen.right) {
        Some(Edge::Right)
    } else {
        None
    }
}

/// Where the red marker goes: on `edge`, centred on where the window was, and kept on screen.
pub fn marker_rect(edge: Edge, window: Rect, screen: Rect) -> Rect {
    let along = |start: i32, end: i32, min: i32, max: i32| {
        let centre = (start + end) / 2;
        let first = (centre - MARKER_LENGTH / 2).clamp(min, (max - MARKER_LENGTH).max(min));
        (first, first + MARKER_LENGTH)
    };
    match edge {
        Edge::Top | Edge::Bottom => {
            let (left, right) = along(window.left, window.right, screen.left, screen.right);
            let top = if edge == Edge::Top {
                screen.top
            } else {
                screen.bottom - MARKER_THICKNESS
            };
            Rect {
                left,
                top,
                right,
                bottom: top + MARKER_THICKNESS,
            }
        }
        Edge::Left | Edge::Right => {
            let (top, bottom) = along(window.top, window.bottom, screen.top, screen.bottom);
            let left = if edge == Edge::Left {
                screen.left
            } else {
                screen.right - MARKER_THICKNESS
            };
            Rect {
                left,
                top,
                right: left + MARKER_THICKNESS,
                bottom,
            }
        }
    }
}

/// What the Windows side reports every time it checks, about ten times a second.
#[derive(Clone, Copy, Debug)]
pub struct Inputs {
    /// Milliseconds from any fixed starting point.
    pub now_ms: u64,
    /// The "Auto-hide at screen edge" toggle.
    pub enabled: bool,
    /// Whether SnapLapse's window is showing (not minimized, not hidden to the tray).
    pub window_visible: bool,
    /// A maximized window touches every edge, but should never dock.
    pub maximized: bool,
    pub window: Rect,
    /// The usable area of the monitor the window is on, not counting the taskbar.
    pub screen: Rect,
    pub cursor: (i32, i32),
    /// Left mouse button held, for example while dragging the window.
    pub mouse_down: bool,
    /// Another SnapLapse window is in front, such as the folder picker, so don't hide.
    pub dialog_open: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Keep the window above other windows (true while docked) or stop doing so.
    SetTopmost(bool),
    /// Hide the window and show the red marker here.
    Hide { marker: Rect },
    /// Show the window and remove the marker.
    Show,
    /// Remove the marker without touching the window, because something else showed it.
    RemoveMarker,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Not docked: a normal window.
    Free,
    /// Docked and showing. `away_since` is when the mouse left, if it's away.
    Docked { away_since: Option<u64> },
    /// Docked and hidden, with the marker showing.
    Hidden { marker: Rect },
}

fn dock_edge(i: &Inputs) -> Option<Edge> {
    if i.maximized {
        None
    } else {
        touching_edge(i.window, i.screen)
    }
}

/// Decides what happens next. Returns the new phase and what the Windows side should do.
pub fn step(phase: Phase, i: &Inputs) -> (Phase, Vec<Action>) {
    if !i.enabled {
        return match phase {
            Phase::Free => (Phase::Free, vec![]),
            Phase::Docked { .. } => (Phase::Free, vec![Action::SetTopmost(false)]),
            Phase::Hidden { .. } => (Phase::Free, vec![Action::Show, Action::SetTopmost(false)]),
        };
    }
    match phase {
        Phase::Free => {
            // Dock once the user lets go of the window against an edge.
            if i.window_visible && !i.mouse_down && dock_edge(i).is_some() {
                (
                    Phase::Docked { away_since: None },
                    vec![Action::SetTopmost(true)],
                )
            } else {
                (Phase::Free, vec![])
            }
        }
        Phase::Docked { away_since } => {
            if !i.window_visible || i.mouse_down || i.dialog_open {
                // Closed to the tray, being dragged, or busy with a dialog: just wait.
                return (Phase::Docked { away_since: None }, vec![]);
            }
            let Some(edge) = dock_edge(i) else {
                // Dragged away from the edge, so it's a normal window again.
                return (Phase::Free, vec![Action::SetTopmost(false)]);
            };
            if i.window.grown(WINDOW_MARGIN).contains(i.cursor) {
                return (Phase::Docked { away_since: None }, vec![]);
            }
            let since = away_since.unwrap_or(i.now_ms);
            if i.now_ms.saturating_sub(since) >= HIDE_DELAY_MS {
                let marker = marker_rect(edge, i.window, i.screen);
                (Phase::Hidden { marker }, vec![Action::Hide { marker }])
            } else {
                (
                    Phase::Docked {
                        away_since: Some(since),
                    },
                    vec![],
                )
            }
        }
        Phase::Hidden { marker } => {
            if i.window_visible {
                // Shown some other way, for example from the tray icon.
                (
                    Phase::Docked { away_since: None },
                    vec![Action::RemoveMarker],
                )
            } else if marker.grown(MARKER_MARGIN).contains(i.cursor) {
                (Phase::Docked { away_since: None }, vec![Action::Show])
            } else {
                (Phase::Hidden { marker }, vec![])
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1040,
    };
    /// A 480×640 window pushed against the top edge.
    const AT_TOP: Rect = Rect {
        left: 700,
        top: 0,
        right: 1180,
        bottom: 640,
    };
    const IN_MIDDLE: Rect = Rect {
        left: 700,
        top: 200,
        right: 1180,
        bottom: 840,
    };

    fn inputs(now_ms: u64, window: Rect, cursor: (i32, i32)) -> Inputs {
        Inputs {
            now_ms,
            enabled: true,
            window_visible: true,
            maximized: false,
            window,
            screen: SCREEN,
            cursor,
            mouse_down: false,
            dialog_open: false,
        }
    }

    /// Feeds a series of moments through the state machine and collects every action.
    fn run(start: Phase, moments: &[Inputs]) -> (Phase, Vec<Action>) {
        let mut phase = start;
        let mut all = Vec::new();
        for i in moments {
            let (next, actions) = step(phase, i);
            phase = next;
            all.extend(actions);
        }
        (phase, all)
    }

    #[test]
    fn finds_the_edge_the_window_touches() {
        assert_eq!(touching_edge(AT_TOP, SCREEN), Some(Edge::Top));
        assert_eq!(touching_edge(IN_MIDDLE, SCREEN), None);
        let left = Rect {
            left: 5,
            top: 200,
            right: 485,
            bottom: 840,
        };
        assert_eq!(touching_edge(left, SCREEN), Some(Edge::Left));
        let right = Rect {
            left: 1440,
            top: 200,
            right: 1920,
            bottom: 840,
        };
        assert_eq!(touching_edge(right, SCREEN), Some(Edge::Right));
        let bottom = Rect {
            left: 700,
            top: 400,
            right: 1180,
            bottom: 1040,
        };
        assert_eq!(touching_edge(bottom, SCREEN), Some(Edge::Bottom));
    }

    #[test]
    fn docks_after_being_dropped_on_an_edge_not_while_dragging() {
        let mut dragging = inputs(0, AT_TOP, (900, 10));
        dragging.mouse_down = true;
        assert_eq!(step(Phase::Free, &dragging).0, Phase::Free);
        let (phase, actions) = step(Phase::Free, &inputs(100, AT_TOP, (900, 10)));
        assert_eq!(phase, Phase::Docked { away_since: None });
        assert_eq!(actions, vec![Action::SetTopmost(true)]);
    }

    #[test]
    fn hides_after_the_mouse_stays_away_then_comes_back_from_the_marker() {
        let docked = Phase::Docked { away_since: None };
        // The mouse leaves; nothing happens until the delay passes.
        let (phase, actions) = run(
            docked,
            &[
                inputs(0, AT_TOP, (100, 900)),
                inputs(500, AT_TOP, (100, 900)),
            ],
        );
        assert!(actions.is_empty());
        let (phase, actions) = run(phase, &[inputs(800, AT_TOP, (100, 900))]);
        let marker = marker_rect(Edge::Top, AT_TOP, SCREEN);
        assert_eq!(actions, vec![Action::Hide { marker }]);
        assert_eq!(phase, Phase::Hidden { marker });

        // Hidden: moving around elsewhere does nothing; pointing at the marker shows it.
        let mut hidden = inputs(900, AT_TOP, (100, 900));
        hidden.window_visible = false;
        assert_eq!(step(phase, &hidden).1, vec![]);
        hidden.cursor = ((marker.left + marker.right) / 2, 0);
        let (phase, actions) = step(phase, &hidden);
        assert_eq!(actions, vec![Action::Show]);
        assert_eq!(phase, Phase::Docked { away_since: None });
    }

    #[test]
    fn a_quick_trip_outside_does_not_hide_it() {
        let (phase, actions) = run(
            Phase::Docked { away_since: None },
            &[
                inputs(0, AT_TOP, (100, 900)),
                inputs(400, AT_TOP, (900, 300)), // back on the window
                inputs(800, AT_TOP, (100, 900)),
                inputs(1300, AT_TOP, (100, 900)), // only 500 ms away this time
            ],
        );
        assert!(actions.is_empty());
        assert_eq!(
            phase,
            Phase::Docked {
                away_since: Some(800)
            }
        );
    }

    #[test]
    fn never_hides_while_a_dialog_is_open_or_the_window_is_in_the_tray() {
        let mut busy = inputs(0, AT_TOP, (100, 900));
        busy.dialog_open = true;
        let later = Inputs {
            now_ms: 5000,
            ..busy
        };
        assert!(run(Phase::Docked { away_since: None }, &[busy, later])
            .1
            .is_empty());

        let mut in_tray = inputs(0, AT_TOP, (100, 900));
        in_tray.window_visible = false;
        let later = Inputs {
            now_ms: 5000,
            ..in_tray
        };
        assert!(run(Phase::Docked { away_since: None }, &[in_tray, later])
            .1
            .is_empty());
    }

    #[test]
    fn a_maximized_window_never_docks() {
        let full = Rect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        let mut maximized = inputs(0, full, (900, 10));
        maximized.maximized = true;
        assert_eq!(step(Phase::Free, &maximized).0, Phase::Free);
        let (phase, actions) = step(Phase::Docked { away_since: None }, &maximized);
        assert_eq!(phase, Phase::Free);
        assert_eq!(actions, vec![Action::SetTopmost(false)]);
    }

    #[test]
    fn dragging_away_from_the_edge_undocks() {
        let (phase, actions) = step(
            Phase::Docked { away_since: None },
            &inputs(0, IN_MIDDLE, (900, 300)),
        );
        assert_eq!(phase, Phase::Free);
        assert_eq!(actions, vec![Action::SetTopmost(false)]);
    }

    #[test]
    fn showing_from_the_tray_removes_the_marker() {
        let marker = marker_rect(Edge::Top, AT_TOP, SCREEN);
        let (phase, actions) = step(Phase::Hidden { marker }, &inputs(0, AT_TOP, (100, 900)));
        assert_eq!(actions, vec![Action::RemoveMarker]);
        assert_eq!(phase, Phase::Docked { away_since: None });
    }

    #[test]
    fn turning_the_toggle_off_brings_the_window_back() {
        let marker = marker_rect(Edge::Top, AT_TOP, SCREEN);
        let mut off = inputs(0, AT_TOP, (100, 900));
        off.enabled = false;
        off.window_visible = false;
        let (phase, actions) = step(Phase::Hidden { marker }, &off);
        assert_eq!(phase, Phase::Free);
        assert_eq!(actions, vec![Action::Show, Action::SetTopmost(false)]);
    }

    #[test]
    fn the_marker_sits_on_the_edge_centred_on_the_window() {
        let marker = marker_rect(Edge::Top, AT_TOP, SCREEN);
        assert_eq!(marker.top, 0);
        assert_eq!(marker.height(), MARKER_THICKNESS);
        assert_eq!(marker.width(), MARKER_LENGTH);
        assert_eq!(
            (marker.left + marker.right) / 2,
            (AT_TOP.left + AT_TOP.right) / 2
        );

        let right = Rect {
            left: 1440,
            top: 200,
            right: 1920,
            bottom: 840,
        };
        let marker = marker_rect(Edge::Right, right, SCREEN);
        assert_eq!(marker.right, SCREEN.right);
        assert_eq!(marker.width(), MARKER_THICKNESS);
    }

    mod properties {
        use super::super::*;
        use proptest::prelude::*;

        fn rects() -> impl Strategy<Value = (Rect, Rect)> {
            // A screen (possibly a second monitor at negative coordinates) and a window that
            // may hang partly off it.
            (-3000i32..3000, -2000i32..2000, 400i32..4000, 300i32..2500).prop_flat_map(
                |(x, y, w, h)| {
                    let screen = Rect {
                        left: x,
                        top: y,
                        right: x + w,
                        bottom: y + h,
                    };
                    (x - 500..x + w, y - 500..y + h, 100i32..1200, 100i32..1200).prop_map(
                        move |(wx, wy, ww, wh)| {
                            (
                                screen,
                                Rect {
                                    left: wx,
                                    top: wy,
                                    right: wx + ww,
                                    bottom: wy + wh,
                                },
                            )
                        },
                    )
                },
            )
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(3000))]

            /// Wherever the window is, the marker is the right size, lies on the screen, and
            /// sits flush against the chosen edge, so the user can always reach it.
            #[test]
            fn the_marker_is_always_on_screen_and_on_its_edge((screen, window) in rects()) {
                for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
                    let m = marker_rect(edge, window, screen);
                    prop_assert!(m.left >= screen.left && m.right <= screen.right, "{m:?} {screen:?}");
                    prop_assert!(m.top >= screen.top && m.bottom <= screen.bottom, "{m:?} {screen:?}");
                    match edge {
                        Edge::Top => prop_assert_eq!(m.top, screen.top),
                        Edge::Bottom => prop_assert_eq!(m.bottom, screen.bottom),
                        Edge::Left => prop_assert_eq!(m.left, screen.left),
                        Edge::Right => prop_assert_eq!(m.right, screen.right),
                    }
                }
            }

            /// Turning the toggle off always ends with a normal, visible window.
            #[test]
            fn switching_off_always_restores_the_window((screen, window) in rects(), hidden in any::<bool>()) {
                let phase = if hidden {
                    Phase::Hidden { marker: marker_rect(Edge::Top, window, screen) }
                } else {
                    Phase::Docked { away_since: Some(0) }
                };
                let i = Inputs {
                    now_ms: 10_000, enabled: false, window_visible: !hidden, maximized: false, window, screen,
                    cursor: (0, 0), mouse_down: false, dialog_open: false,
                };
                let (next, actions) = step(phase, &i);
                prop_assert_eq!(next, Phase::Free);
                prop_assert!(actions.contains(&Action::SetTopmost(false)));
                prop_assert_eq!(actions.contains(&Action::Show), hidden);
            }
        }
    }
}
