//! `xdg_shell` and `xdg_decoration` handlers.
//!
//! Pluto is a tiling compositor: clients do not get to place their own
//! toplevels, so `move_request` / `resize_request` intentionally do nothing.
//! Decorations are always client-side.

use smithay::{
    desktop::{PopupKind, find_popup_root_surface, get_popup_toplevel_coords},
    reexports::{
        wayland_protocols::xdg::{
            decoration::zv1::server::zxdg_toplevel_decoration_v1,
            shell::server::xdg_toplevel,
        },
        wayland_server::protocol::{wl_seat, wl_surface::WlSurface},
    },
    utils::Serial,
    wayland::shell::xdg::{
        PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
        decoration::XdgDecorationHandler,
    },
};

use crate::state::{Backend, Pluto};

impl<B: Backend + 'static> XdgShellHandler for Pluto<B> {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.protocols.xdg_shell
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // Every new toplevel becomes a tiled window on the focused output.
        let output = self
            .output_under(self.pointer.current_location())
            .or_else(|| self.space.outputs().next().cloned())
            .expect("no output available to place a window on");

        let window = smithay::desktop::Window::new_wayland_window(surface);
        self.tiling.add(window.clone(), output);
        self.focus_window(Some(window));
        self.tiling.arrange(&mut self.space);
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        unconstrain_popup(self, &surface);
        let _ = self.popups.track_popup(PopupKind::Xdg(surface));
    }

    fn reposition_request(&mut self, surface: PopupSurface, positioner: PositionerState, token: u32) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        unconstrain_popup(self, &surface);
        surface.send_repositioned(token);
    }

    fn move_request(&mut self, _surface: ToplevelSurface, _seat: wl_seat::WlSeat, _serial: Serial) {
        // Tiling: interactive client-driven moves are ignored.
    }

    fn resize_request(
        &mut self,
        _surface: ToplevelSurface,
        _seat: wl_seat::WlSeat,
        _serial: Serial,
        _edges: xdg_toplevel::ResizeEdge,
    ) {
        // Tiling: interactive client-driven resizes are ignored.
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {
        // TODO: popup grabs require dedicated focus target types. Popups are
        // positioned correctly but do not dismiss on outside clicks yet.
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        self.tiling.remove_surface(surface.wl_surface());
        self.tiling.arrange(&mut self.space);
        let next = self.tiling.focus_candidate();
        self.focus_window(next);
    }
}

impl<B: Backend + 'static> XdgDecorationHandler for Pluto<B> {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ClientSide);
        });
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: zxdg_toplevel_decoration_v1::Mode) {
        // Always force client-side decorations.
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ClientSide);
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(zxdg_toplevel_decoration_v1::Mode::ClientSide);
        });
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }
}

/// Adjust a popup's geometry so it stays inside its output.
fn unconstrain_popup<B: Backend + 'static>(pluto: &Pluto<B>, popup: &PopupSurface) {
    let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else {
        return;
    };
    let Some(window) = pluto
        .space
        .elements()
        .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == &root))
    else {
        return;
    };
    let Some(output) = pluto.space.outputs_for_element(window).first().cloned() else {
        return;
    };
    let Some(output_geo) = pluto.space.output_geometry(&output) else {
        return;
    };
    let Some(window_geo) = pluto.space.element_geometry(window) else {
        return;
    };

    let mut target = output_geo;
    target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
    target.loc -= window_geo.loc;

    popup.with_pending_state(|state| {
        state.geometry = state.positioner.get_unconstrained_geometry(target);
    });
}

/// Resolve the root surface of a popup (used to route popup input).
pub fn popup_root(_surface: &WlSurface) -> Option<WlSurface> {
    // and rely on space tracking.
    None
}
