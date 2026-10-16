//! Window management: the tiling engine and the actions keybindings invoke.

pub mod tiling;

use crate::state::{Backend, Pluto};

impl<B: Backend + 'static> Pluto<B> {
    /// Close the currently focused window.
    pub fn close_focused(&mut self) {
        let Some(window) = self.focused_window() else {
            return;
        };
        if let Some(toplevel) = window.toplevel() {
            toplevel.send_close();
        }
    }

    /// Cycle keyboard focus to the next (`forward`) or previous window.
    pub fn focus_cycle(&mut self, forward: bool) {
        let current = self.focused_window();
        let next = self.tiling.focus_next(current.as_ref(), forward);
        self.focus_window(next);
    }

    /// Promote the focused window to the master slot and re-tile.
    pub fn promote_focused(&mut self) {
        if let Some(window) = self.focused_window() {
            self.tiling.promote(&window);
            self.tiling.arrange(&mut self.space);
        }
    }

    /// Move the focused window up one slot towards the master and re-tile.
    pub fn move_focused_up(&mut self) {
        if let Some(window) = self.focused_window() {
            self.tiling.move_up(&window);
            self.tiling.arrange(&mut self.space);
        }
    }

    /// Move the focused window down one slot away from the master and re-tile.
    pub fn move_focused_down(&mut self) {
        if let Some(window) = self.focused_window() {
            self.tiling.move_down(&window);
            self.tiling.arrange(&mut self.space);
        }
    }
}
