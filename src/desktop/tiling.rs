//! A small master/stack tiling layout engine.
//!
//! The engine keeps an ordered list of windows per output. Windows are laid out
//! automatically: up to `nmaster` windows fill the master area on the left, the
//! rest are stacked vertically in the remaining space on the right. When there
//! are no stacked windows the master area grows to fill the output.
//!
//! Layout is expressed purely through `Space::map_element`, so it composes with
//! Smithay's space abstraction and knows nothing about rendering.

use std::collections::HashMap;

use smithay::{
    desktop::{Space, Window, layer_map_for_output},
    output::Output,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Rectangle, Size},
};

/// Per-output ordered window stack.
#[derive(Debug, Default)]
struct OutputStack {
    output: Option<Output>,
    windows: Vec<Window>,
}

/// The tiling state of the compositor.
#[derive(Debug)]
pub struct TilingState {
    /// Fraction of the usable width given to the master area (0.0..=1.0).
    pub master_ratio: f64,
    /// Number of windows kept in the master area.
    pub nmaster: usize,
    /// Gap between adjacent windows, in logical pixels.
    pub inner_gap: i32,
    /// Gap between the windows and the output edge, in logical pixels.
    pub outer_gap: i32,

    stacks: Vec<OutputStack>,
    /// Last size we asked each window to take, keyed by `Window::id`.
    requested: HashMap<usize, Size<i32, Logical>>,
}

impl Default for TilingState {
    fn default() -> Self {
        TilingState {
            master_ratio: 0.6,
            nmaster: 1,
            inner_gap: 4,
            outer_gap: 4,
            stacks: Vec::new(),
            requested: HashMap::new(),
        }
    }
}

impl TilingState {
    /// Register a new window on `output`, becoming the new master.
    pub fn add(&mut self, window: Window, output: Output) {
        let stack = match self.stacks.iter_mut().find(|s| s.output.as_ref() == Some(&output)) {
            Some(stack) => stack,
            None => {
                self.stacks.push(OutputStack {
                    output: Some(output),
                    windows: Vec::new(),
                });
                self.stacks.last_mut().unwrap()
            }
        };
        stack.windows.insert(0, window);
    }

    /// Drop `output` from the layout, moving its windows to another output.
    ///
    /// Clients keep their buffers; if no other output exists the windows simply
    /// fall out of the tiling engine (they are unmapped from the space by the
    /// caller).
    pub fn remove_output(&mut self, output: &Output) {
        let Some(pos) = self.stacks.iter().position(|s| s.output.as_ref() == Some(output)) else {
            return;
        };
        let stack = self.stacks.remove(pos);
        match self.stacks.first_mut() {
            Some(first) => first.windows.extend(stack.windows),
            None => {
                for window in stack.windows {
                    self.requested.remove(&window.id());
                }
            }
        }
    }

    /// Remove a window.
    pub fn remove(&mut self, window: &Window) {
        for stack in &mut self.stacks {
            stack.windows.retain(|w| w != window);
        }
        self.requested.remove(&window.id());
    }

    /// Remove whichever window owns `surface`.
    pub fn remove_surface(&mut self, surface: &WlSurface) {
        let found = self
            .stacks
            .iter()
            .flat_map(|s| s.windows.iter())
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
            .cloned();
        if let Some(window) = found {
            self.remove(&window);
        }
    }

    /// All windows, master first, grouped by output.
    pub fn all_windows(&self) -> impl Iterator<Item = &Window> {
        self.stacks.iter().flat_map(|s| s.windows.iter())
    }

    /// Whether this window is managed by the tiling engine.
    pub fn contains(&self, window: &Window) -> bool {
        self.all_windows().any(|w| w == window)
    }

    /// The window to focus after a close: the top of the first non-empty stack.
    pub fn focus_candidate(&self) -> Option<Window> {
        self.stacks
            .iter()
            .find_map(|s| s.windows.last().cloned())
    }

    /// Cycle focus forward (`next`) or backward relative to `current`.
    pub fn focus_next(&self, current: Option<&Window>, forward: bool) -> Option<Window> {
        let mut order: Vec<&Window> = self.all_windows().collect();
        if order.is_empty() {
            return None;
        }
        // Focus order follows stacking order (bottom -> top).
        order.reverse();

        let idx = current.and_then(|c| order.iter().position(|w| *w == c));
        let next = match (idx, forward) {
            (Some(i), true) => (i + 1) % order.len(),
            (Some(i), false) => (i + order.len() - 1) % order.len(),
            (None, _) => 0,
        };
        Some(order[next].clone())
    }

    /// Move `window` to the master position of its stack.
    pub fn promote(&mut self, window: &Window) {
        for stack in &mut self.stacks {
            if let Some(pos) = stack.windows.iter().position(|w| w == window) {
                let w = stack.windows.remove(pos);
                stack.windows.insert(0, w);
                return;
            }
        }
    }

    /// Move `window` one slot towards the master area.
    pub fn move_up(&mut self, window: &Window) {
        for stack in &mut self.stacks {
            if let Some(pos) = stack.windows.iter().position(|w| w == window) {
                if pos > 0 {
                    stack.windows.swap(pos, pos - 1);
                }
                return;
            }
        }
    }

    /// Move `window` one slot away from the master area.
    pub fn move_down(&mut self, window: &Window) {
        for stack in &mut self.stacks {
            if let Some(pos) = stack.windows.iter().position(|w| w == window) {
                if pos + 1 < stack.windows.len() {
                    stack.windows.swap(pos, pos + 1);
                }
                return;
            }
        }
    }

    /// Whether the last configure we sent does not match the committed size.
    ///
    /// Used to re-assert the layout when a client resizes itself.
    pub fn size_changed(&self, window: &Window) -> bool {
        match self.requested.get(&window.id()) {
            Some(requested) => window.geometry().size != *requested,
            None => false,
        }
    }

    /// Recompute and apply the layout for every output.
    pub fn arrange(&mut self, space: &mut Space<Window>) {
        let outputs: Vec<Output> = space.outputs().cloned().collect();

        for output in outputs {
            let Some(output_geo) = space.output_geometry(&output) else {
                continue;
            };

            // Leave room for layer surfaces that reserved an exclusive zone
            // (bars, docks, ...). The zone is output-local.
            let zone = layer_map_for_output(&output).non_exclusive_zone();
            let area = Rectangle::new(output_geo.loc + zone.loc, zone.size);
            if area.size.w <= 0 || area.size.h <= 0 {
                continue;
            }

            let windows: Vec<Window> = self
                .stacks
                .iter()
                .find(|s| s.output.as_ref() == Some(&output))
                .map(|s| s.windows.clone())
                .unwrap_or_default();
            if windows.is_empty() {
                continue;
            }

            let rects = self.compute_layout(area, windows.len());
            for (window, rect) in windows.iter().zip(rects) {
                self.apply(space, window, rect);
            }
        }
    }

    /// Compute the rectangles for `n` windows inside `area`.
    fn compute_layout(&self, area: Rectangle<i32, Logical>, n: usize) -> Vec<Rectangle<i32, Logical>> {
        let gap = self.inner_gap;
        let usable = Rectangle::new(
            (area.loc.x + self.outer_gap, area.loc.y + self.outer_gap).into(),
            (
                (area.size.w - 2 * self.outer_gap).max(1),
                (area.size.h - 2 * self.outer_gap).max(1),
            )
                .into(),
        );

        match n {
            0 => Vec::new(),
            1 => vec![usable],
            _ => {
                let nmaster = self.nmaster.clamp(1, n);
                if n <= nmaster {
                    return split_vertical(usable, n, gap);
                }

                let master_w = (usable.size.w as f64 * self.master_ratio).round() as i32;
                let master_w = master_w.clamp(1, usable.size.w - gap - 1);
                let stack_w = usable.size.w - master_w - gap;

                let master_area = Rectangle::new(usable.loc, (master_w, usable.size.h).into());
                let stack_loc = (usable.loc.x + master_w + gap, usable.loc.y).into();
                let stack_area = Rectangle::new(stack_loc, (stack_w, usable.size.h).into());

                let mut rects = split_vertical(master_area, nmaster, gap);
                rects.extend(split_vertical(stack_area, n - nmaster, gap));
                rects
            }
        }
    }

    /// Send a configure if needed and move the window into place.
    fn apply(&mut self, space: &mut Space<Window>, window: &Window, rect: Rectangle<i32, Logical>) {
        let desired = rect.size;
        let needs_configure = self.requested.get(&window.id()) != Some(&desired);

        if needs_configure {
            if let Some(toplevel) = window.toplevel() {
                toplevel.with_pending_state(|state| {
                    state.size = Some(desired);
                });
                toplevel.send_pending_configure();
            }
            self.requested.insert(window.id(), desired);
        }

        space.map_element(window.clone(), rect.loc, false);
    }
}

/// Split `area` into `n` equal-height rows, separated by `gap`.
fn split_vertical(area: Rectangle<i32, Logical>, n: usize, gap: i32) -> Vec<Rectangle<i32, Logical>> {
    if n == 0 {
        return Vec::new();
    }
    let n_i = n as i32;
    let total_gap = gap * (n_i - 1);
    let usable_h = (area.size.h - total_gap).max(n_i);
    let base = usable_h / n_i;
    let extra = usable_h % n_i;

    let mut out = Vec::with_capacity(n);
    let mut y = area.loc.y;
    for i in 0..n_i {
        let h = base + i32::from(i < extra);
        out.push(Rectangle::new((area.loc.x, y).into(), (area.size.w, h).into()));
        y += h + gap;
    }
    out
}
