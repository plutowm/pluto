//! `wl_output` / `zxdg_output_manager_v1` handler.

use smithay::wayland::output::OutputHandler;

use crate::state::{Backend, Pluto};

impl<B: Backend + 'static> OutputHandler for Pluto<B> {}
