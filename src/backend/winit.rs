use smithay::{
    backend::renderer::damage::OutputDamageTracker,
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{calloop::EventLoop},
    utils::Transform,
};

use crate::{state::{Backend, Pluto}, render};

pub struct WinitBackend {
    name: String,
}

impl WinitBackend {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            name: "winit".to_string(),
        })
    }
}

impl Backend for WinitBackend {
    const HAS_RELATIVE_MOTION: bool = false;
    const HAS_GESTURES: bool = false;

    fn seat_name(&self) -> String {
        self.name.clone()
    }

    fn reset_buffers(&mut self, _output: &Output) {}

    fn early_import(&mut self, _surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface) {}

    fn update_led_state(&mut self, _led_state: smithay::input::keyboard::LedState) {}
}

pub fn init(
    event_loop: &mut EventLoop<Pluto<WinitBackend>>,
    state: &mut Pluto<WinitBackend>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut backend, winit) = smithay::backend::winit::init()?;

    let mode = Mode {
        size: backend.window_size(),
        refresh: 60_000,
    };

    let output = Output::new(
        "winit".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "Smithay".into(),
            model: "Winit".into(),
            serial_number: "Unknown".into(),
        },
    );
    let _global = output.create_global::<Pluto<WinitBackend>>(&state.display_handle);
    output.change_current_state(Some(mode), Some(Transform::Flipped180), None, Some((0, 0).into()));
    output.set_preferred(mode);

    state.space.map_output(&output, (0, 0));

    let mut damage_tracker = OutputDamageTracker::from_output(&output);

    event_loop.handle().insert_source(winit, move |event, _, state| {
        use smithay::backend::winit::WinitEvent;
        match event {
            WinitEvent::Resized { size, .. } => {
                output.change_current_state(
                    Some(Mode { size, refresh: 60_000 }),
                    None,
                    None,
                    None,
                );
                smithay::desktop::layer_map_for_output(&output).arrange();
                state.tiling.arrange(&mut state.space);
            }
            WinitEvent::Input(event) => state.process_input_event(event),
            WinitEvent::Redraw => {
                let age = backend.buffer_age().unwrap_or(0);

                let render_result = {
                    let (renderer, mut framebuffer) = backend.bind().unwrap();
                    smithay::desktop::space::render_output::<
                        _,
                        smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement<
                            smithay::backend::renderer::gles::GlesRenderer,
                        >,
                        smithay::desktop::Window,
                        _,
                    >(
                        &output,
                        renderer,
                        &mut framebuffer,
                        1.0,
                        age,
                        [&state.space],
                        &[],
                        &mut damage_tracker,
                        render::CLEAR_COLOR,
                    )
                };

                match render_result {
                    Ok(result) => {
                        if let Some(damage) = result.damage {
                            if let Err(err) = backend.submit(Some(damage)) {
                                tracing::warn!("Failed to submit buffer: {}", err);
                            }
                        }

                        let states = result.states;
                        crate::state::update_primary_scanout_output(
                            &state.space,
                            &output,
                            &state.cursor_status,
                            &state.dnd_icon,
                            &states,
                        );
                        crate::state::post_repaint(state, &output, &states);
                    }
                    Err(err) => {
                        tracing::error!("Render error: {:?}", err);
                    }
                }

                state.space.refresh();
                state.popups.cleanup();
                let _ = state.display_handle.flush_clients();

                backend.window().request_redraw();
            }
            WinitEvent::CloseRequested => {
                state.stop();
            }
            _ => (),
        }
    })?;

    Ok(())
}
