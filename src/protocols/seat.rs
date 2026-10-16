//! Seat-related protocol handlers.

use smithay::wayland::pointer_constraints::PointerConstraintsHandler;

use crate::state::{Backend, Pluto};

/// Pluto does not implement pointer confinement or locking yet.
///
/// Applications requesting a constraint simply never get one activated, which is
/// safe (they just see a normal pointer).
impl<B: Backend + 'static> PointerConstraintsHandler for Pluto<B> {}
