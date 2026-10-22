//! Pure switching logic, independent of Wayland so it can be unit-tested.

use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Next,
    Prev,
}

/// A window that is on the current workspace.
#[derive(Clone, Debug)]
pub struct Candidate<K> {
    pub key: K,
    pub focused: bool,
}

/// Picks the workspace Alt+Tab should operate on.
///
/// `active` are all workspaces currently shown (one per output with OutputBound
/// workspaces). `focused_on` are the workspaces of the focused window, if any.
/// Returns `None` when the answer is ambiguous, so we never cycle windows on
/// a monitor the user is not looking at.
pub fn current_workspace<W: Clone + PartialEq>(
    active: &[W],
    focused_on: Option<&[W]>,
) -> Option<W> {
    if let Some(ws) = focused_on.and_then(|on| on.iter().find(|w| active.contains(w))) {
        return Some(ws.clone());
    }
    match active {
        [only] => Some(only.clone()),
        _ => None,
    }
}

struct Burst<K, W> {
    workspace: W,
    order: Vec<K>,
    index: usize,
    last: Instant,
}

/// Most-recently-used focus history plus the "burst" snapshot that lets
/// repeated Alt+Tab presses walk the list instead of toggling between two windows.
pub struct Switcher<K, W> {
    mru: Vec<K>,
    burst: Option<Burst<K, W>>,
    burst_window: Duration,
}

impl<K: Clone + PartialEq, W: Clone + PartialEq> Switcher<K, W> {
    pub fn new(burst_window: Duration) -> Self {
        Self {
            mru: Vec::new(),
            burst: None,
            burst_window,
        }
    }

    pub fn note_focused(&mut self, key: &K) {
        self.mru.retain(|k| k != key);
        self.mru.insert(0, key.clone());
    }

    pub fn forget(&mut self, key: &K) {
        self.mru.retain(|k| k != key);
        if let Some(burst) = &mut self.burst {
            burst.order.retain(|k| k != key);
        }
    }

    /// Candidates ordered by focus history; never-focused windows keep their given order at the end.
    pub fn ordered(&self, candidates: &[Candidate<K>]) -> Vec<K> {
        let mut order: Vec<K> = self
            .mru
            .iter()
            .filter(|k| candidates.iter().any(|c| &c.key == *k))
            .cloned()
            .collect();
        for c in candidates {
            if !order.contains(&c.key) {
                order.push(c.key.clone());
            }
        }
        // The focused window always comes first, even if its focus event hasn't arrived yet.
        if let Some(focused) = candidates.iter().find(|c| c.focused) {
            order.retain(|k| k != &focused.key);
            order.insert(0, focused.key.clone());
        }
        order
    }

    /// Returns the window to activate, or `None` if there is nothing to switch to.
    pub fn pick(
        &mut self,
        now: Instant,
        dir: Direction,
        workspace: &W,
        candidates: &[Candidate<K>],
    ) -> Option<K> {
        let in_burst = self.burst.as_ref().is_some_and(|b| {
            b.workspace == *workspace && now.duration_since(b.last) < self.burst_window
        });

        if in_burst {
            let burst = self.burst.as_mut().unwrap();
            // Drop windows that closed or left the workspace mid-burst.
            burst
                .order
                .retain(|k| candidates.iter().any(|c| &c.key == k));
            if !burst.order.is_empty() {
                let len = burst.order.len();
                burst.index = step(burst.index.min(len - 1), dir, len);
                burst.last = now;
                return Some(burst.order[burst.index].clone());
            }
        }

        let order = self.ordered(candidates);
        if order.is_empty() {
            self.burst = None;
            return None;
        }
        let len = order.len();
        let has_focus = candidates.iter().any(|c| c.focused);
        let index = match (has_focus, dir) {
            // Nothing focused here: the first press goes to the most recent window.
            (false, Direction::Next) => 0,
            (false, Direction::Prev) => len - 1,
            (true, _) if len == 1 => {
                self.burst = None;
                return None;
            }
            (true, dir) => step(0, dir, len),
        };
        let key = order[index].clone();
        self.burst = Some(Burst {
            workspace: workspace.clone(),
            order,
            index,
            last: now,
        });
        Some(key)
    }
}

fn step(index: usize, dir: Direction, len: usize) -> usize {
    match dir {
        Direction::Next => (index + 1) % len,
        Direction::Prev => (index + len - 1) % len,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BURST: Duration = Duration::from_millis(700);

    fn cands(keys: &[&'static str], focused: &str) -> Vec<Candidate<&'static str>> {
        keys.iter()
            .map(|k| Candidate {
                key: *k,
                focused: *k == focused,
            })
            .collect()
    }

    #[test]
    fn current_workspace_prefers_focused_window() {
        assert_eq!(current_workspace(&[1, 2], Some(&[2][..])), Some(2));
        assert_eq!(current_workspace(&[1], None), Some(1));
        // Two monitors, nothing focused: ambiguous.
        assert_eq!(current_workspace(&[1, 2], None), None);
        // Focused window is on a hidden workspace (should not happen) and two monitors.
        assert_eq!(current_workspace(&[1, 2], Some(&[3][..])), None);
        assert_eq!(current_workspace::<i32>(&[], None), None);
    }

    #[test]
    fn single_press_goes_to_previous_window() {
        let mut s = Switcher::new(BURST);
        s.note_focused(&"a");
        s.note_focused(&"b");
        s.note_focused(&"c"); // c focused, then b, then a
        let t = Instant::now();
        let c = cands(&["a", "b", "c"], "c");
        assert_eq!(s.pick(t, Direction::Next, &1, &c), Some("b"));
    }

    #[test]
    fn separate_presses_toggle_between_two_most_recent() {
        let mut s = Switcher::new(BURST);
        for k in ["a", "b", "c"] {
            s.note_focused(&k);
        }
        let t = Instant::now();
        assert_eq!(
            s.pick(t, Direction::Next, &1, &cands(&["a", "b", "c"], "c")),
            Some("b")
        );
        s.note_focused(&"b");
        let later = t + Duration::from_secs(2);
        assert_eq!(
            s.pick(later, Direction::Next, &1, &cands(&["a", "b", "c"], "b")),
            Some("c")
        );
    }

    #[test]
    fn burst_walks_frozen_snapshot() {
        let mut s = Switcher::new(BURST);
        for k in ["a", "b", "c"] {
            s.note_focused(&k);
        }
        let t = Instant::now();
        assert_eq!(
            s.pick(t, Direction::Next, &1, &cands(&["a", "b", "c"], "c")),
            Some("b")
        );
        // Activation updates MRU, but the burst keeps the original order.
        s.note_focused(&"b");
        let t2 = t + Duration::from_millis(300);
        assert_eq!(
            s.pick(t2, Direction::Next, &1, &cands(&["a", "b", "c"], "b")),
            Some("a")
        );
        s.note_focused(&"a");
        let t3 = t2 + Duration::from_millis(300);
        assert_eq!(
            s.pick(t3, Direction::Next, &1, &cands(&["a", "b", "c"], "a")),
            Some("c")
        );
        let t4 = t3 + Duration::from_millis(300);
        assert_eq!(
            s.pick(t4, Direction::Prev, &1, &cands(&["a", "b", "c"], "c")),
            Some("a")
        );
    }

    #[test]
    fn prev_goes_to_least_recent() {
        let mut s = Switcher::new(BURST);
        for k in ["a", "b", "c"] {
            s.note_focused(&k);
        }
        let t = Instant::now();
        assert_eq!(
            s.pick(t, Direction::Prev, &1, &cands(&["a", "b", "c"], "c")),
            Some("a")
        );
    }

    #[test]
    fn burst_resets_on_workspace_change() {
        let mut s = Switcher::new(BURST);
        for k in ["a", "b", "x", "y"] {
            s.note_focused(&k);
        }
        let t = Instant::now();
        assert_eq!(
            s.pick(t, Direction::Next, &1, &cands(&["a", "b"], "b")),
            Some("a")
        );
        let t2 = t + Duration::from_millis(100);
        assert_eq!(
            s.pick(t2, Direction::Next, &2, &cands(&["x", "y"], "y")),
            Some("x")
        );
    }

    #[test]
    fn closed_window_dropped_mid_burst() {
        let mut s = Switcher::new(BURST);
        for k in ["a", "b", "c"] {
            s.note_focused(&k);
        }
        let t = Instant::now();
        assert_eq!(
            s.pick(t, Direction::Next, &1, &cands(&["a", "b", "c"], "c")),
            Some("b")
        );
        s.forget(&"a");
        let t2 = t + Duration::from_millis(100);
        // order was [c, b, a] -> a closed -> [c, b]; index 1 -> next wraps to c
        assert_eq!(
            s.pick(t2, Direction::Next, &1, &cands(&["b", "c"], "b")),
            Some("c")
        );
    }

    #[test]
    fn nothing_to_do() {
        let mut s: Switcher<&str, i32> = Switcher::new(BURST);
        let t = Instant::now();
        assert_eq!(s.pick(t, Direction::Next, &1, &[]), None);
        assert_eq!(s.pick(t, Direction::Next, &1, &cands(&["a"], "a")), None);
    }

    #[test]
    fn unfocused_workspace_activates_most_recent() {
        let mut s = Switcher::new(BURST);
        s.note_focused(&"a");
        s.note_focused(&"b");
        let t = Instant::now();
        assert_eq!(
            s.pick(t, Direction::Next, &1, &cands(&["a", "b"], "")),
            Some("b")
        );
        let mut s2: Switcher<&str, i32> = Switcher::new(BURST);
        assert_eq!(
            s2.pick(t, Direction::Next, &1, &cands(&["a"], "")),
            Some("a")
        );
    }

    #[test]
    fn never_focused_windows_go_last() {
        let mut s = Switcher::new(BURST);
        s.note_focused(&"b");
        let t = Instant::now();
        assert_eq!(
            s.pick(t, Direction::Prev, &1, &cands(&["a", "b", "z"], "b")),
            Some("z")
        );
    }
}
