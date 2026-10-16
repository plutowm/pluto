//! `wlr-layer-shell` handler.
//!
//! Panels, bars and other shell components map themselves as layer surfaces.
//! They live in a per-output [`LayerMap`](smithay::desktop::LayerMap) rather
//! than in the tiling [`Space`](smithay::desktop::Space), so the tiling engine
//! is completely unaware of them.

use smithay::{
    desktop::{LayerSurface, layer_map_for_output},
    output::Output,
    reexports::wayland_server::protocol::{wl_output, wl_surface::WlSurface},
    wayland::shell::wlr_layer::{
        KeyboardInteractivity, Layer, LayerSurface as WlrLayerSurface, LayerSurfaceConfigure,
        WlrLayerShellHandler, WlrLayerShellState,
    },
};

use crate::state::{Backend, Pluto};

impl<B: Backend + 'static> WlrLayerShellHandler for Pluto<B> {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.protocols.layer_shell
    }

    fn new_layer_surface(
        &mut self,
        surface: WlrLayerSurface,
        wl_output: Option<wl_output::WlOutput>,
        _layer: Layer,
        namespace: String,
    ) {
        // Respect an explicit output request, otherwise place the surface on the
        // output under the cursor (falling back to the first one).
        let output = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .or_else(|| self.output_under(self.pointer.current_location()))
            .or_else(|| self.space.outputs().next().cloned());

        let Some(output) = output else {
            tracing::warn!(namespace, "layer surface created with no output available");
            return;
        };

        let mut map = layer_map_for_output(&output);
        if let Err(err) = map.map_layer(&LayerSurface::new(surface, namespace)) {
            tracing::warn!("failed to map layer surface: {}", err);
        }
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        let Some((mut map, layer)) = self.space.outputs().find_map(|o| {
            let map = layer_map_for_output(o);
            let layer = map
                .layers()
                .find(|&layer| layer.layer_surface() == &surface)
                .cloned();
            layer.map(|layer| (map, layer))
        }) else {
            return;
        };

        let before = map.non_exclusive_zone();
        map.unmap_layer(&layer);
        let zone_changed = map.non_exclusive_zone() != before;
        drop(map);

        // A bar that reserved space went away: give the space back to windows.
        if zone_changed {
            self.tiling.arrange(&mut self.space);
        }

        if self.focused_layer.as_ref() == Some(&layer) {
            self.focus_layer(None);
            let next = self.tiling.focus_candidate();
            self.focus_window(next);
        }
    }

    fn new_popup(
        &mut self,
        _parent: WlrLayerSurface,
        popup: smithay::wayland::shell::xdg::PopupSurface,
    ) {
        let _ = self
            .popups
            .track_popup(smithay::desktop::PopupKind::Xdg(popup));
    }

    fn ack_configure(&mut self, surface: WlSurface, _configure: LayerSurfaceConfigure) {
        // A surface that asked for exclusive keyboard interactivity (e.g. a
        // launcher) receives focus as soon as it is configured. On-demand
        // surfaces are focused on click instead.
        let layer = self.space.outputs().find_map(|o| {
            let map = layer_map_for_output(o);
            map.layers()
                .find(|l| l.wl_surface() == &surface)
                .cloned()
        });

        if let Some(layer) = layer
            && layer.cached_state().keyboard_interactivity == KeyboardInteractivity::Exclusive
        {
            self.focus_layer(Some(layer));
        }
    }
}
