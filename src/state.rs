//! Compositor state shared by all backends.
//!
//! The [`Pluto`] type is generic over a [`Backend`], so the exact same protocol
//! handling, input processing, tiling and rendering code is reused for the
//! nested (winit) and the real-hardware (udev/DRM) backends. Backends only have
//! to provide a small [`Backend`] handle holding their device state.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use smithay::{
    backend::renderer::element::{
        RenderElementStates, default_primary_scanout_output_compare,
    },
    desktop::{
        LayerSurface, PopupManager, Space, Window, layer_map_for_output,
        utils::{
            OutputPresentationFeedback, surface_presentation_feedback_flags_from_states,
            surface_primary_scanout_output, update_surface_primary_scanout_output,
        },
    },
    input::{
        Seat, SeatHandler, SeatState,
        keyboard::{Keysym, LedState, XkbConfig},
        pointer::{CursorImageStatus, PointerHandle},
    },
    output::Output,
    reexports::{
        calloop::{Interest, LoopHandle, Mode, PostAction, generic::Generic},
        wayland_server::{
            Display, DisplayHandle,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::wl_surface::WlSurface,
            Resource,
        },
    },
    utils::{Clock, Logical, Monotonic, Point, SERIAL_COUNTER},
    wayland::{
        compositor::{CompositorClientState, get_parent},
        fractional_scale::with_fractional_scale,
        socket::ListeningSocketSource,
    },
};

use crate::{config::Config, desktop::tiling::TilingState, protocols::Protocols};

/// Data associated with each connected Wayland client.
#[derive(Debug, Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

/// A drag-and-drop icon currently being dragged.
#[derive(Debug)]
pub struct DndIcon {
    pub surface: WlSurface,
    pub offset: Point<i32, Logical>,
}

/// Backend-specific state that the shared compositor code needs to reach.
///
/// Every method here exists because *something* in the generic code path needs
/// to poke at backend internals. Keep this trait as small as possible.
pub trait Backend {
    /// Whether the backend produces relative pointer motion (needed for games).
    const HAS_RELATIVE_MOTION: bool = false;
    /// Whether the backend produces touchpad gestures.
    const HAS_GESTURES: bool = false;

    fn seat_name(&self) -> String;
    fn reset_buffers(&mut self, output: &Output);
    fn early_import(&mut self, surface: &WlSurface);
    fn update_led_state(&mut self, led_state: LedState);
}

/// The compositor state.
pub struct Pluto<B: Backend + 'static> {
    pub backend_data: B,
    pub config: Config,
    pub socket_name: Option<String>,
    pub display_handle: DisplayHandle,
    pub running: Arc<AtomicBool>,
    pub handle: LoopHandle<'static, Pluto<B>>,

    // Desktop
    pub space: Space<Window>,
    pub popups: PopupManager,
    pub tiling: TilingState,
    pub focused_window: Option<Window>,
    /// The layer surface currently holding keyboard focus, if any.
    pub focused_layer: Option<LayerSurface>,

    // Wayland protocol globals
    pub protocols: Protocols,
    pub seat_state: SeatState<Pluto<B>>,

    // Input
    pub seat: Seat<Pluto<B>>,
    pub pointer: PointerHandle<Pluto<B>>,
    pub cursor_status: CursorImageStatus,
    pub suppressed_keys: Vec<Keysym>,
    pub clock: Clock<Monotonic>,

    // Drag-and-drop
    pub dnd_icon: Option<DndIcon>,

    pub show_window_preview: bool,
}

impl<B: Backend + 'static> Pluto<B> {
    /// Create the compositor state and register all Wayland globals.
    pub fn new(
        display: Display<Pluto<B>>,
        handle: LoopHandle<'static, Pluto<B>>,
        backend_data: B,
        config: Config,
    ) -> Pluto<B> {
        let display_handle = display.handle();
        let clock = Clock::new();

        let socket_name = Self::init_socket(&handle);

        // The Display itself must be driven by the event loop.
        handle
            .insert_source(
                Generic::new(display, Interest::READ, Mode::Level),
                |_, display, data| {
                    // Safety: we never drop the display while the loop runs.
                    unsafe {
                        display.get_mut().dispatch_clients(data).unwrap();
                    }
                    Ok(PostAction::Continue)
                },
            )
            .expect("Failed to insert the wayland display into the event loop");

        let protocols = Protocols::new::<B>(&display_handle, B::HAS_RELATIVE_MOTION, &clock);

        // Input: a single seat with a pointer and a keyboard.
        let seat_name = backend_data.seat_name();
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&display_handle, seat_name);
        let pointer = seat.add_pointer();
        seat.add_keyboard(XkbConfig::default(), 200, 25)
            .expect("Failed to initialize the keyboard");

        Pluto {
            backend_data,
            config,
            socket_name,
            display_handle,
            running: Arc::new(AtomicBool::new(true)),
            handle,
            space: Space::default(),
            popups: PopupManager::default(),
            tiling: TilingState::default(),
            focused_window: None,
            focused_layer: None,
            protocols,
            seat_state,
            seat,
            pointer,
            cursor_status: CursorImageStatus::default_named(),
            suppressed_keys: Vec::new(),
            clock,
            dnd_icon: None,
            show_window_preview: false,
        }
    }

    fn init_socket(handle: &LoopHandle<'static, Pluto<B>>) -> Option<String> {
        let source = ListeningSocketSource::new_auto().ok()?;
        let socket_name = source.socket_name().to_string_lossy().into_owned();
        handle
            .insert_source(source, |client_stream, _, data| {
                if let Err(err) = data
                    .display_handle
                    .insert_client(client_stream, Arc::new(ClientState::default()))
                {
                    tracing::warn!("Error adding wayland client: {}", err);
                }
            })
            .expect("Failed to insert the wayland socket source");
        tracing::info!(name = socket_name, "Listening on wayland socket");
        Some(socket_name)
    }

    /// Request the compositor to shut down.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    /// The `Output` that currently contains `pos`, if any.
    pub fn output_under(&self, pos: Point<f64, Logical>) -> Option<Output> {
        self.space
            .outputs()
            .find(|o| {
                self.space
                    .output_geometry(o)
                    .is_some_and(|geo| geo.to_f64().contains(pos))
            })
            .cloned()
    }

    /// The window that contains `pos` on the desktop.
    pub fn window_under(&self, pos: Point<f64, Logical>) -> Option<(Window, Point<f64, Logical>)> {
        self.space
            .element_under(pos)
            .map(|(w, l)| (w.clone(), Point::from((l.x as f64, l.y as f64))))
    }

    /// The currently focused window, cloned.
    pub fn focused_window(&self) -> Option<Window> {
        self.focused_window.clone()
    }

    /// Resolve the [`Window`] a [`WlSurface`] (possibly a subsurface) belongs to.
    pub fn window_for_surface(&self, surface: &WlSurface) -> Option<Window> {
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        self.space
            .elements()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == &root))
            .cloned()
    }

    /// Focus a window (or clear focus) and update keyboard focus accordingly.
    pub fn focus_window(&mut self, window: Option<Window>) {
        let serial = smithay::utils::SERIAL_COUNTER.next_serial();
        self.focused_window = window.clone();
        self.focused_layer = None;

        for w in self.space.elements() {
            let active = Some(w) == window.as_ref();
            if w.set_activated(active) {
                if let Some(toplevel) = w.toplevel() {
                    toplevel.send_pending_configure();
                }
            }
        }

        let surface = window.and_then(|w| w.toplevel().map(|t| t.wl_surface().clone()));
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, surface, serial);
        }
    }

    /// Give keyboard focus to a layer surface (e.g. a launcher), or clear it.
    ///
    /// Clearing does not restore window focus; callers decide what to focus
    /// next (see the layer-destroyed handler).
    pub fn focus_layer(&mut self, layer: Option<LayerSurface>) {
        let serial = SERIAL_COUNTER.next_serial();
        self.focused_layer = layer;
        let surface = self
            .focused_layer
            .as_ref()
            .map(|l| l.wl_surface().clone());
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, surface, serial);
        }
    }
}

/// Send frame callbacks to every client on `output` to advance their render loop.
pub fn send_frames<B: Backend + 'static>(
    state: &mut Pluto<B>,
    output: &Output,
    _render_element_states: &RenderElementStates,
) {
    let time = state.clock.now();
    let throttle = Some(Duration::from_secs(1));

    state.space.elements().for_each(|window| {
        if state.space.outputs_for_element(window).contains(output) {
            window.send_frame(output, time, throttle, surface_primary_scanout_output);
        }
    });

    {
        let map = layer_map_for_output(output);
        for layer in map.layers() {
            layer.send_frame(output, time, throttle, surface_primary_scanout_output);
        }
    }

    state.space.refresh();
    state.popups.cleanup();
    let _ = state.display_handle.flush_clients();
}

/// Update the primary scan-out output tracking used by presentation feedback and
/// frame callbacks.
pub fn update_primary_scanout_output(
    space: &Space<Window>,
    output: &Output,
    cursor_status: &CursorImageStatus,
    dnd_icon: &Option<DndIcon>,
    render_element_states: &RenderElementStates,
) {
    space.elements().for_each(|window| {
        window.with_surfaces(|surface, states| {
            update_surface_primary_scanout_output(
                surface,
                output,
                states,
                None,
                render_element_states,
                default_primary_scanout_output_compare,
            );
        });
    });

    {
        let map = layer_map_for_output(output);
        for layer in map.layers() {
            layer.with_surfaces(|surface, states| {
                update_surface_primary_scanout_output(
                    surface,
                    output,
                    states,
                    None,
                    render_element_states,
                    default_primary_scanout_output_compare,
                );
            });
        }
    }

    if let CursorImageStatus::Surface(surface) = cursor_status {
        smithay::desktop::utils::with_surfaces_surface_tree(surface, |surface, states| {
            update_surface_primary_scanout_output(
                surface,
                output,
                states,
                None,
                render_element_states,
                default_primary_scanout_output_compare,
            );
        });
    }

    if let Some(surface) = dnd_icon.as_ref().map(|i| &i.surface) {
        smithay::desktop::utils::with_surfaces_surface_tree(surface, |surface, states| {
            update_surface_primary_scanout_output(
                surface,
                output,
                states,
                None,
                render_element_states,
                default_primary_scanout_output_compare,
            );
        });
    }
}

/// Collect presentation feedback for everything that was rendered on `output`.
pub fn take_presentation_feedback(
    output: &Output,
    space: &Space<Window>,
    render_element_states: &RenderElementStates,
) -> OutputPresentationFeedback {
    let mut feedback = OutputPresentationFeedback::new(output);

    space.elements().for_each(|window| {
        if space.outputs_for_element(window).contains(output) {
            window.take_presentation_feedback(
                &mut feedback,
                surface_primary_scanout_output,
                |surface, _| {
                    surface_presentation_feedback_flags_from_states(surface, None, render_element_states)
                },
            );
        }
    });

    {
        let map = layer_map_for_output(output);
        for layer in map.layers() {
            layer.take_presentation_feedback(
                &mut feedback,
                surface_primary_scanout_output,
                |surface, _| {
                    surface_presentation_feedback_flags_from_states(surface, None, render_element_states)
                },
            );
        }
    }

    feedback
}

/// Update fractional scale and send frame callbacks after rendering.
pub fn post_repaint<B: Backend + 'static>(
    state: &mut Pluto<B>,
    output: &Output,
    render_element_states: &RenderElementStates,
) {
    state.space.elements().for_each(|window| {
        window.with_surfaces(|surface, states| {
            if let Some(o) = surface_primary_scanout_output(surface, states) {
                with_fractional_scale(states, |scale| {
                    scale.set_preferred_scale(o.current_scale().fractional_scale());
                });
            }
        });
    });

    {
        let map = layer_map_for_output(output);
        for layer in map.layers() {
            layer.with_surfaces(|surface, states| {
                if let Some(o) = surface_primary_scanout_output(surface, states) {
                    with_fractional_scale(states, |scale| {
                        scale.set_preferred_scale(o.current_scale().fractional_scale());
                    });
                }
            });
        }
    }

    send_frames(state, output, render_element_states);
}

// All protocol handler impls live in `crate::protocols`. This macro wires the
// state type up to `wayland-server`'s Dispatch/GlobalDispatch machinery.
smithay::delegate_dispatch2!(@<B: Backend + 'static> Pluto<B>);

impl<B: Backend + 'static> SeatHandler for Pluto<B> {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Pluto<B>> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let dh = &self.display_handle;
        let client = focused.and_then(|s| dh.get_client(s.id()).ok());
        crate::protocols::data_device::set_focus(dh, seat, client);
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor_status = image;
    }

    fn led_state_changed(&mut self, _seat: &Seat<Self>, led_state: LedState) {
        self.backend_data.update_led_state(led_state);
    }
}
