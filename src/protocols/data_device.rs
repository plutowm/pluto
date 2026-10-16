//! Clipboard (`wl_data_device`), primary selection and drag-and-drop.

use smithay::{
    input::{
        Seat,
        dnd::{DnDGrab, DndGrabHandler, GrabType, Source},
        pointer::Focus,
    },
    reexports::wayland_server::{
        Client, DisplayHandle,
        protocol::wl_surface::WlSurface,
    },
    utils::{Logical, Point, Serial},
    wayland::selection::{
        SelectionHandler,
        data_device::{
            DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler, set_data_device_focus,
        },
        primary_selection::{PrimarySelectionHandler, PrimarySelectionState, set_primary_focus},
    },
};

use crate::state::{Backend, DndIcon, Pluto};

impl<B: Backend + 'static> SelectionHandler for Pluto<B> {
    type SelectionUserData = ();
}

impl<B: Backend + 'static> DataDeviceHandler for Pluto<B> {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.protocols.data_device
    }
}

impl<B: Backend + 'static> PrimarySelectionHandler for Pluto<B> {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState {
        &mut self.protocols.primary_selection
    }
}

impl<B: Backend + 'static> DndGrabHandler for Pluto<B> {
    fn dropped(
        &mut self,
        _target: Option<smithay::input::dnd::DndTarget<'_, Self>>,
        _validated: bool,
        _seat: Seat<Self>,
        _location: Point<f64, Logical>,
    ) {
        self.dnd_icon = None;
    }
}

impl<B: Backend + 'static> WaylandDndGrabHandler for Pluto<B> {
    fn dnd_requested<S: Source>(
        &mut self,
        source: S,
        icon: Option<WlSurface>,
        seat: Seat<Self>,
        serial: Serial,
        type_: GrabType,
    ) {
        self.dnd_icon = icon.map(|surface| DndIcon {
            surface,
            offset: (0, 0).into(),
        });

        match type_ {
            GrabType::Pointer => {
                let pointer = seat.get_pointer().unwrap();
                let start_data = pointer.grab_start_data().unwrap();
                let grab = DnDGrab::new_pointer(&self.display_handle, start_data, source, seat);
                pointer.set_grab(self, grab, serial, Focus::Keep);
            }
            GrabType::Touch => {
                // Pluto does not implement touch drag-and-drop.
                source.cancel();
            }
        }
    }
}

/// Update both data-device and primary-selection focus for a seat.
pub fn set_focus<D>(
    dh: &DisplayHandle,
    seat: &Seat<D>,
    client: Option<Client>,
)
where
    D: smithay::input::SeatHandler + DataDeviceHandler + PrimarySelectionHandler + 'static,
{
    set_data_device_focus(dh, seat, client.clone());
    set_primary_focus(dh, seat, client);
}
