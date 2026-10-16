mod backend;
mod config;
mod desktop;
mod input;
mod protocols;
mod render;
mod state;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_logging();

    #[cfg(all(feature = "winit", feature = "udev"))]
    {
        let use_udev = std::env::var("PLUTO_BACKEND")
            .map(|backend| backend == "udev")
            .unwrap_or(false);
        if use_udev {
            return run_udev();
        }
        return run_winit();
    }

    #[cfg(all(feature = "winit", not(feature = "udev")))]
    {
        run_winit()
    }

    #[cfg(all(feature = "udev", not(feature = "winit")))]
    {
        run_udev()
    }

    #[cfg(not(any(feature = "winit", feature = "udev")))]
    {
        compile_error!("Pluto requires at least one of the `winit` or `udev` features");
    }
}

#[cfg(feature = "winit")]
fn run_winit() -> Result<(), Box<dyn std::error::Error>> {
    use smithay::reexports::{calloop::EventLoop, wayland_server::Display};

    let mut event_loop: EventLoop<state::Pluto<backend::winit::WinitBackend>> = EventLoop::try_new()?;
    let display: Display<state::Pluto<backend::winit::WinitBackend>> = Display::new()?;

    let config = config::Config::default();
    let mut state =
        state::Pluto::new(display, event_loop.handle(), backend::winit::WinitBackend::new()?, config);

    backend::winit::init(&mut event_loop, &mut state)?;

    if let Some(ref socket) = state.socket_name {
        unsafe { std::env::set_var("WAYLAND_DISPLAY", socket) };
    }

    if let Some(ref cmd) = state
        .config
        .autostart
        .clone()
        .or_else(|| std::env::args().skip(1).next().map(|_| state.config.terminal.clone()))
    {
        let _ = std::process::Command::new(cmd).spawn();
    }

    event_loop.run(None, &mut state, |_| {})?;

    Ok(())
}

#[cfg(feature = "udev")]
fn run_udev() -> Result<(), Box<dyn std::error::Error>> {
    backend::udev::run()
}

fn init_logging() {
    if let Ok(env_filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    } else {
        tracing_subscriber::fmt().init();
    }
}
