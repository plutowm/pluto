//! `xdg_activation` and `wp_fractional_scale`.

use smithay::{
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    wayland::{
        compositor::{get_parent, with_states},
        fractional_scale::{FractionalScaleHandler, with_fractional_scale},
        xdg_activation::{XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData},
    },
};

use crate::state::{Backend, Pluto};

impl<B: Backend + 'static> XdgActivationHandler for Pluto<B> {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.protocols.xdg_activation
    }

    fn request_activation(
        &mut self,
        _token: XdgActivationToken,
        _token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        // Unconditionally honor activation requests coming from recently used
        // tokens: we do not implement a focus-stealing-prevention policy yet.
        if let Some(window) = self.window_for_surface(&surface) {
            self.focus_window(Some(window));
        }
    }
}

impl<B: Backend + 'static> FractionalScaleHandler for Pluto<B> {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        // Walk up to the root surface and use the scale of the output it is on.
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }

        let output = self
            .space
            .elements()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == &root))
            .and_then(|w| self.space.outputs_for_element(w).first().cloned())
            .or_else(|| self.space.outputs().next().cloned());

        if let Some(output) = output {
            let scale = output.current_scale().fractional_scale();
            with_states(&surface, |states| {
                with_fractional_scale(states, |state| {
                    state.set_preferred_scale(scale);
                });
            });
        }
    }
}
