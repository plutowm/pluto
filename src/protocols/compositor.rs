//! `wl_compositor`, `wl_shm` and buffer lifecycle.

use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    desktop::{PopupKind, WindowSurfaceType, layer_map_for_output},
    reexports::wayland_server::{
        Client,
        protocol::{wl_buffer, wl_surface::WlSurface},
    },
    wayland::{
        buffer::BufferHandler,
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState, get_parent,
            is_sync_subsurface, with_states,
        },
        shell::wlr_layer::LayerSurfaceData,
        shm::{ShmHandler, ShmState},
    },
};

use crate::state::{Backend, ClientState, Pluto};

impl<B: Backend + 'static> CompositorHandler for Pluto<B> {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.protocols.compositor
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit(self, surface);
    }
}

impl<B: Backend + 'static> ShmHandler for Pluto<B> {
    fn shm_state(&self) -> &ShmState {
        &self.protocols.shm
    }
}

impl<B: Backend + 'static> BufferHandler for Pluto<B> {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

/// Handle a surface commit: import buffers, run `Window::on_commit`, send the
/// initial xdg configure, track popups and re-tile if a client changed size.
pub fn on_commit<B: Backend + 'static>(state: &mut Pluto<B>, surface: &WlSurface) {
    on_commit_buffer_handler::<Pluto<B>>(surface);

    if is_sync_subsurface(surface) {
        return;
    }

    let mut root = surface.clone();
    while let Some(parent) = get_parent(&root) {
        root = parent;
    }

    let maybe_window = state
        .space
        .elements()
        .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == &root))
        .cloned();

    if let Some(window) = &maybe_window {
        window.on_commit();

        if let Some(toplevel) = window.toplevel() {
            // Send the initial configure as soon as the surface is mapped.
            if !toplevel.is_initial_configure_sent() {
                toplevel.send_configure();
            }
        }
    }

    // Popups commit their own state and may need their initial configure.
    state.popups.commit(surface);
    if let Some(popup) = state.popups.find_popup(surface) {
        match popup {
            PopupKind::Xdg(ref xdg) => {
                if !xdg.is_initial_configure_sent() {
                    let _ = xdg.send_configure();
                }
            }
            PopupKind::InputMethod(_) => {}
        }
    }

    // Layer surfaces are arranged and given their initial configure here, after
    // the client has committed the anchor/size it wants, so the configure we
    // send actually reflects the client's requested state.
    if let Some(output) = state
        .space
        .outputs()
        .find(|o| {
            let map = layer_map_for_output(o);
            map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
                .is_some()
        })
        .cloned()
    {
        let initial_configure_sent = with_states(surface, |states| {
            states
                .data_map
                .get::<LayerSurfaceData>()
                .map(|data| data.lock().unwrap().initial_configure_sent)
        })
        .unwrap_or(false);

        let mut map = layer_map_for_output(&output);
        map.arrange();

        if !initial_configure_sent
            && let Some(layer) = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL)
        {
            layer.layer_surface().send_configure();
        }
    }

    // If the client committed a size we did not ask for, re-assert the layout.
    if let Some(window) = &maybe_window {
        if state.tiling.size_changed(window) {
            state.tiling.arrange(&mut state.space);
        }
    }
}
