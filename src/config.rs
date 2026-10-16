//! Static configuration for the compositor.
//!
//! Values here are intentionally minimal. They are used by the winit backend to
//! spawn the initial client and by keybinds.

use smithay::input::keyboard::ModifiersState;

#[derive(Debug, Clone)]
pub struct Config {
    /// Command executed when the user presses `Mod+Return`.
    pub terminal: String,
    /// Command executed on startup (if none specified via CLI).
    pub autostart: Option<String>,
    /// Modifiers that count as the compositor's "super" modifier.
    pub mod_key: ModifiersState,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            terminal: "foot".to_string(),
            autostart: None,
            mod_key: ModifiersState {
                logo: true,
                ..Default::default()
            },
        }
    }
}
