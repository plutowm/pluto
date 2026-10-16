//! Wayland protocol globals and their handler implementations.
//!
//! Each submodule owns one logical group of protocols. `Protocols` bundles the
//! state objects; `Protocols::new` is the single place where globals are
//! advertised, so adding a protocol means touching exactly two places (the
//! struct and `new`).

pub mod activation;
pub mod compositor;
pub mod data_device;
pub mod output;
pub mod seat;
pub mod xdg_shell;

use smithay::{
    reexports::wayland_server::DisplayHandle,
    utils::{Clock, Monotonic},
    wayland::{
        compositor::CompositorState,
        fractional_scale::FractionalScaleManagerState,
        output::OutputManagerState,
        pointer_constraints::PointerConstraintsState,
        presentation::PresentationState,
        relative_pointer::RelativePointerManagerState,
        selection::{
            data_device::DataDeviceState, primary_selection::PrimarySelectionState,
        },
        shell::xdg::{XdgShellState, decoration::XdgDecorationState},
        shm::ShmState,
        viewporter::ViewporterState,
        xdg_activation::XdgActivationState,
    },
};

use crate::state::{Backend, Pluto};

/// Bundle of every protocol state object the compositor advertises.
pub struct Protocols {
    pub compositor: CompositorState,
    pub shm: ShmState,
    pub xdg_shell: XdgShellState,
    pub output_manager: OutputManagerState,
    pub data_device: DataDeviceState,
    pub primary_selection: PrimarySelectionState,
    pub xdg_decoration: XdgDecorationState,
    pub xdg_activation: XdgActivationState,
    pub fractional_scale: FractionalScaleManagerState,
    pub presentation: PresentationState,
    pub pointer_constraints: PointerConstraintsState,
    pub relative_pointer: Option<RelativePointerManagerState>,
    pub viewporter: ViewporterState,
}

impl Protocols {
    pub fn new<B: Backend + 'static>(
        dh: &DisplayHandle,
        has_relative_motion: bool,
        clock: &Clock<Monotonic>,
    ) -> Self {
        Protocols {
            compositor: CompositorState::new::<Pluto<B>>(dh),
            shm: ShmState::new::<Pluto<B>>(
                dh,
                vec![
                    smithay::reexports::wayland_server::protocol::wl_shm::Format::Argb8888,
                    smithay::reexports::wayland_server::protocol::wl_shm::Format::Xrgb8888,
                ],
            ),
            xdg_shell: XdgShellState::new::<Pluto<B>>(dh),
            output_manager: OutputManagerState::new_with_xdg_output::<Pluto<B>>(dh),
            data_device: DataDeviceState::new::<Pluto<B>>(dh),
            primary_selection: PrimarySelectionState::new::<Pluto<B>>(dh),
            xdg_decoration: XdgDecorationState::new::<Pluto<B>>(dh),
            xdg_activation: XdgActivationState::new::<Pluto<B>>(dh),
            fractional_scale: FractionalScaleManagerState::new::<Pluto<B>>(dh),
            presentation: PresentationState::new::<Pluto<B>>(dh, clock.id() as u32),
            pointer_constraints: PointerConstraintsState::new::<Pluto<B>>(dh),
            relative_pointer: has_relative_motion
                .then(|| RelativePointerManagerState::new::<Pluto<B>>(dh)),
            viewporter: ViewporterState::new::<Pluto<B>>(dh),
        }
    }
}
