//! Real-hardware backend: DRM/KMS outputs, libinput devices and a libseat
//! session.
//!
//! This is deliberately single-GPU: one [`GlesRenderer`] drives every output.
//! Render offloading across multiple GPUs is a possible future extension but
//! keeping a single renderer makes this module substantially smaller.

use std::{
    collections::HashMap,
    path::Path,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use smithay::{
    backend::{
        allocator::{
            Fourcc,
            format::FormatSet,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{
            DrmDevice, DrmDeviceFd, DrmError, DrmEvent, DrmEventMetadata, DrmEventTime, DrmNode,
            NodeType,
            compositor::{FrameFlags, RenderFrameError},
            exporter::gbm::GbmFramebufferExporter,
            output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
        },
        egl::{EGLContext, EGLDisplay},
        input::InputEvent,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            ImportDma, ImportMemWl,
            damage::Error as OutputDamageTrackerError,
            element::{
                AsRenderElements, RenderElementStates, memory::MemoryRenderBuffer,
                surface::WaylandSurfaceRenderElement,
            },
            gles::GlesRenderer,
        },
        session::{
            Event as SessionEvent, Session,
            libseat::{self, LibSeatSession},
        },
        udev::{UdevBackend, UdevEvent, all_gpus, primary_gpu},
    },
    desktop::{Space, Window, utils::OutputPresentationFeedback},
    input::{
        keyboard::LedState,
        pointer::{CursorImageAttributes, CursorImageStatus},
    },
    output::{Mode as WlMode, Output, PhysicalProperties},
    reexports::{
        calloop::{
            EventLoop, RegistrationToken,
            timer::{TimeoutAction, Timer},
        },
        drm::{
            Device as _,
            control::{ModeTypeFlags, connector, crtc},
        },
        input::{Device as InputDevice, DeviceCapability, Libinput},
        rustix::fs::OFlags,
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::{Display, DisplayHandle, backend::GlobalId, protocol::wl_surface},
    },
    utils::{DeviceFd, IsAlive, Logical, Monotonic, Point, Scale, Time, Transform},
    wayland::{
        compositor,
        dmabuf::{DmabufFeedbackBuilder, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        presentation::Refresh,
    },
};
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};
use tracing::{error, info, trace, warn};

use crate::{
    config::Config,
    render::{Cursor, OutputRenderElements, PointerElement, CLEAR_COLOR},
    state::{Backend, Pluto, take_presentation_feedback, update_primary_scanout_output},
};

/// Formats we are happy to scan out, best first.
const SUPPORTED_FORMATS: &[Fourcc] = &[
    Fourcc::Abgr2101010,
    Fourcc::Argb2101010,
    Fourcc::Abgr8888,
    Fourcc::Argb8888,
];

/// A DRM output and its associated Wayland output global.
type UdevDrmOutput = DrmOutput<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    Option<OutputPresentationFeedback>,
    DrmDeviceFd,
>;

type UdevDrmOutputManager = DrmOutputManager<
    GbmAllocator<DrmDeviceFd>,
    GbmFramebufferExporter<DrmDeviceFd>,
    Option<OutputPresentationFeedback>,
    DrmDeviceFd,
>;

/// The render element type the single-GPU renderer produces.
type UdevElements = OutputRenderElements<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>;

/// Identifies which output a DRM surface feeds.
#[derive(Debug, PartialEq, Eq, Clone)]
struct UdevOutputId {
    device_id: DrmNode,
    crtc: crtc::Handle,
}

/// One DRM device we manage.
struct BackendData {
    surfaces: HashMap<crtc::Handle, SurfaceData>,
    drm_output_manager: UdevDrmOutputManager,
    drm_scanner: DrmScanner,
    registration_token: RegistrationToken,
}

/// A single output.
struct SurfaceData {
    dh: DisplayHandle,
    output: Output,
    global: Option<GlobalId>,
    drm_output: UdevDrmOutput,
    last_presentation_time: Option<Time<Monotonic>>,
}

impl Drop for SurfaceData {
    fn drop(&mut self) {
        self.output.leave_all();
        if let Some(global) = self.global.take() {
            self.dh.remove_global::<Pluto<UdevData>>(global);
        }
    }
}

/// Backend-specific state stored inside [`Pluto`].
pub struct UdevData {
    session: LibSeatSession,
    dmabuf_state: Option<(DmabufState, DmabufGlobal)>,
    primary_gpu: DrmNode,
    renderer: Option<GlesRenderer>,
    backends: HashMap<DrmNode, BackendData>,
    pointer_images: Vec<(xcursor::parser::Image, MemoryRenderBuffer)>,
    pointer_element: PointerElement,
    pointer_image: Cursor,
    keyboards: Vec<InputDevice>,
}

impl DmabufHandler for Pluto<UdevData> {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.backend_data.dmabuf_state.as_mut().unwrap().0
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: smithay::backend::allocator::dmabuf::Dmabuf,
        notifier: ImportNotifier,
    ) {
        let renderer = self
            .backend_data
            .renderer
            .as_mut()
            .expect("dmabuf imported before renderer was initialized");
        if renderer.import_dmabuf(&dmabuf, None).is_ok() {
            let _ = notifier.successful::<Pluto<UdevData>>();
        } else {
            notifier.failed();
        }
    }
}

impl Backend for UdevData {
    const HAS_RELATIVE_MOTION: bool = true;
    const HAS_GESTURES: bool = true;

    fn seat_name(&self) -> String {
        self.session.seat()
    }

    fn reset_buffers(&mut self, output: &Output) {
        if let Some(id) = output.user_data().get::<UdevOutputId>() {
            if let Some(backend) = self.backends.get_mut(&id.device_id) {
                if let Some(surface) = backend.surfaces.get_mut(&id.crtc) {
                    surface.drm_output.reset_buffers();
                }
            }
        }
    }

    fn early_import(&mut self, _surface: &wl_surface::WlSurface) {
        // Single GPU: the renderer can already import every buffer.
    }

    fn update_led_state(&mut self, led_state: LedState) {
        for keyboard in &mut self.keyboards {
            keyboard.led_update(led_state.into());
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum DeviceAddError {
    #[error("Failed to open device using libseat: {0}")]
    DeviceOpen(libseat::Error),
    #[error("Failed to initialize drm device: {0}")]
    DrmDevice(DrmError),
    #[error("Failed to initialize gbm device: {0}")]
    GbmDevice(std::io::Error),
    #[error("Failed to initialize EGL: {0}")]
    Egl(String),
    #[error("The primary GPU has not been initialized yet")]
    NoRenderer,
}

/// Entry point for the udev backend.
pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut event_loop: EventLoop<Pluto<UdevData>> = EventLoop::try_new()?;
    let display: Display<Pluto<UdevData>> = Display::new()?;

    let (session, notifier) = LibSeatSession::new()?;

    let primary_gpu = if let Ok(var) = std::env::var("PLUTO_DRM_DEVICE") {
        DrmNode::from_path(var)?
    } else {
        primary_gpu(session.seat())
            .unwrap()
            .and_then(|x| DrmNode::from_path(x).ok()?.node_with_type(NodeType::Render)?.ok())
            .unwrap_or_else(|| {
                all_gpus(session.seat())
                    .unwrap()
                    .into_iter()
                    .find_map(|x| DrmNode::from_path(x).ok())
                    .expect("No GPU found!")
            })
    };
    info!("Using {} as primary GPU", primary_gpu);

    let backend_data = UdevData {
        dmabuf_state: None,
        session,
        primary_gpu,
        renderer: None,
        backends: HashMap::new(),
        pointer_images: Vec::new(),
        pointer_element: PointerElement::default(),
        pointer_image: Cursor::load(),
        keyboards: Vec::new(),
    };

    let mut state = Pluto::new(display, event_loop.handle(), backend_data, Config::default());

    let udev_backend = UdevBackend::new(&state.backend_data.session.seat())?;

    let mut libinput_context = Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(
        state.backend_data.session.clone().into(),
    );
    libinput_context
        .udev_assign_seat(&state.backend_data.session.seat())
        .unwrap();
    let libinput_backend = LibinputInputBackend::new(libinput_context.clone());

    event_loop
        .handle()
        .insert_source(libinput_backend, move |mut event, _, data| {
            if let InputEvent::DeviceAdded { device } = &mut event {
                if device.has_capability(DeviceCapability::Keyboard) {
                    if let Some(led_state) = data.seat.get_keyboard().map(|k| k.led_state()) {
                        device.led_update(led_state.into());
                    }
                    data.backend_data.keyboards.push(device.clone());
                }
            } else if let InputEvent::DeviceRemoved { ref device } = event {
                if device.has_capability(DeviceCapability::Keyboard) {
                    data.backend_data.keyboards.retain(|item| item != device);
                }
            }
            data.process_input_event(event);
        })
        .unwrap();

    event_loop
        .handle()
        .insert_source(notifier, move |event, &mut (), data| match event {
            SessionEvent::PauseSession => {
                libinput_context.suspend();
                for backend in data.backend_data.backends.values_mut() {
                    backend.drm_output_manager.pause();
                }
            }
            SessionEvent::ActivateSession => {
                if let Err(err) = libinput_context.resume() {
                    error!("Failed to resume libinput: {:?}", err);
                }
                for (node, backend) in data.backend_data.backends.iter_mut() {
                    if let Err(err) = backend.drm_output_manager.lock().activate(false) {
                        error!("Failed to activate DRM output manager: {:?}", err);
                    }
                    let node = *node;
                    data.handle.insert_idle(move |data| {
                        data.render(node, None, data.clock.now());
                    });
                }
            }
        })
        .unwrap();

    // Bring up the primary GPU first so the renderer is available for everything
    // else.
    let primary_node = primary_gpu.node_with_type(NodeType::Primary).and_then(|n| n.ok());
    let primary_device = udev_backend.device_list().find(|(device_id, _)| {
        primary_node
            .map(|primary_node| *device_id == primary_node.dev_id())
            .unwrap_or(false)
            || *device_id == primary_gpu.dev_id()
    });

    if let Some((device_id, path)) = primary_device {
        let node = DrmNode::from_dev_id(device_id).expect("valid primary node");
        state.device_added(node, path).expect("failed to initialize primary node");
    }

    let primary_device_id = primary_device.map(|(device_id, _)| device_id);
    for (device_id, path) in udev_backend.device_list() {
        if Some(device_id) == primary_device_id {
            continue;
        }
        if let Err(err) = DrmNode::from_dev_id(device_id)
            .map_err(|_| DeviceAddError::NoRenderer)
            .and_then(|node| state.device_added(node, path))
        {
            error!("Skipping device {}: {}", device_id, err);
        }
    }

    event_loop
        .handle()
        .insert_source(udev_backend, move |event, _, data| match event {
            UdevEvent::Added { device_id, path } => {
                if let Err(err) = DrmNode::from_dev_id(device_id).map_err(|_| DeviceAddError::NoRenderer).and_then(
                    |node| {
                        data.device_added(node, &path)?;
                        Ok(())
                    },
                ) {
                    error!("Skipping device {}: {}", device_id, err);
                }
            }
            UdevEvent::Changed { device_id } => {
                if let Ok(node) = DrmNode::from_dev_id(device_id) {
                    data.device_changed(node);
                }
            }
            UdevEvent::Removed { device_id } => {
                if let Ok(node) = DrmNode::from_dev_id(device_id) {
                    data.device_removed(node);
                }
            }
        })
        .unwrap();

    state.initialize_dmabuf().expect("failed to initialize dmabuf");

    while state.running.load(Ordering::SeqCst) {
        if let Err(err) = event_loop.dispatch(Duration::from_millis(16), &mut state) {
            error!("Event loop error: {:?}", err);
            break;
        }
        state.space.refresh();
        state.popups.cleanup();
        let _ = state.display_handle.flush_clients();
    }

    Ok(())
}

impl Pluto<UdevData> {
    /// Bind the renderer to the display and advertise dmabuf + shm formats.
    fn initialize_dmabuf(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let renderer = self.backend_data.renderer.as_mut().ok_or(DeviceAddError::NoRenderer)?;

        let dmabuf_formats = renderer.dmabuf_formats();
        let default_feedback =
            DmabufFeedbackBuilder::new(self.backend_data.primary_gpu.dev_id(), dmabuf_formats).build()?;
        let mut dmabuf_state = DmabufState::new();
        let global = dmabuf_state
            .create_global_with_default_feedback::<Pluto<UdevData>>(&self.display_handle, &default_feedback);
        self.backend_data.dmabuf_state = Some((dmabuf_state, global));

        let formats = renderer.shm_formats().collect::<Vec<_>>();
        self.protocols.shm.update_formats(formats);

        Ok(())
    }

    fn device_added(&mut self, node: DrmNode, path: &Path) -> Result<(), DeviceAddError> {
        let fd = self
            .backend_data
            .session
            .open(
                path,
                OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
            )
            .map_err(DeviceAddError::DeviceOpen)?;

        let fd = DrmDeviceFd::new(DeviceFd::from(fd));
        let (drm, notifier) = DrmDevice::new(fd.clone(), true).map_err(DeviceAddError::DrmDevice)?;
        let gbm = GbmDevice::new(fd).map_err(DeviceAddError::GbmDevice)?;

        let registration_token = self
            .handle
            .insert_source(notifier, move |event, metadata, data: &mut Pluto<UdevData>| match event {
                DrmEvent::VBlank(crtc) => data.frame_finish(node, crtc, metadata),
                DrmEvent::Error(error) => error!("DRM error: {:?}", error),
            })
            .unwrap();

        let allocator = GbmAllocator::new(
            gbm.clone(),
            GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
        );
        let framebuffer_exporter =
            GbmFramebufferExporter::new(gbm.clone(), Some(self.backend_data.primary_gpu).into());

        // Ensure the renderer exists (for the primary GPU) before we need its
        // supported render formats.
        if self.backend_data.renderer.is_none() && node == self.backend_data.primary_gpu {
            let display =
                unsafe { EGLDisplay::new(gbm.clone()) }.map_err(|err| DeviceAddError::Egl(err.to_string()))?;
            let context =
                EGLContext::new(&display).map_err(|err| DeviceAddError::Egl(err.to_string()))?;
            let renderer = unsafe { GlesRenderer::new(context) }
                .map_err(|err| DeviceAddError::Egl(err.to_string()))?;
            self.backend_data.renderer = Some(renderer);
        }

        let renderer = self
            .backend_data
            .renderer
            .as_mut()
            .ok_or(DeviceAddError::NoRenderer)?;
        let render_formats: FormatSet = renderer
            .egl_context()
            .dmabuf_render_formats()
            .iter()
            .copied()
            .collect();

        let drm_output_manager = DrmOutputManager::new(
            drm,
            allocator,
            framebuffer_exporter,
            Some(gbm),
            SUPPORTED_FORMATS.iter().copied(),
            render_formats,
        );

        self.backend_data.backends.insert(
            node,
            BackendData {
                registration_token,
                drm_output_manager,
                drm_scanner: DrmScanner::new(),
                surfaces: HashMap::new(),
            },
        );

        self.device_changed(node);
        Ok(())
    }

    fn connector_connected(&mut self, node: DrmNode, connector: connector::Info, crtc: crtc::Handle) {
        let device = match self.backend_data.backends.get_mut(&node) {
            Some(device) => device,
            None => return,
        };

        let output_name = format!("{}-{}", connector.interface().as_str(), connector.interface_id());
        info!(?crtc, "Setting up connector {}", output_name);

        let mode_id = connector
            .modes()
            .iter()
            .position(|mode| mode.mode_type().contains(ModeTypeFlags::PREFERRED))
            .unwrap_or(0);
        let drm_mode = connector.modes()[mode_id];
        let wl_mode = WlMode::from(drm_mode);

        let (phys_w, phys_h) = connector.size().unwrap_or((0, 0));
        let output = Output::new(
            output_name,
            PhysicalProperties {
                size: (phys_w as i32, phys_h as i32).into(),
                subpixel: connector.subpixel().into(),
                make: "Unknown".into(),
                model: "Unknown".into(),
                serial_number: "Unknown".into(),
            },
        );
        let global = output.create_global::<Pluto<UdevData>>(&self.display_handle);

        let x = self
            .space
            .outputs()
            .fold(0, |acc, o| acc + self.space.output_geometry(o).unwrap().size.w);
        let position = (x, 0).into();

        output.set_preferred(wl_mode);
        output.change_current_state(Some(wl_mode), Some(Transform::Normal), None, Some(position));
        self.space.map_output(&output, position);
        smithay::desktop::layer_map_for_output(&output).arrange();
        self.tiling.arrange(&mut self.space);

        output.user_data().insert_if_missing(|| UdevOutputId {
            crtc,
            device_id: node,
        });

        let mut planes = match device.drm_output_manager.device().planes(&crtc) {
            Ok(planes) => planes,
            Err(err) => {
                warn!("Failed to query planes: {}", err);
                return;
            }
        };
        if device
            .drm_output_manager
            .device()
            .get_driver()
            .map(|d| d.name().to_string_lossy().to_lowercase().contains("nvidia"))
            .unwrap_or(false)
        {
            planes.overlay = vec![];
        }

        let renderer = self
            .backend_data
            .renderer
            .as_mut()
            .expect("renderer not initialized");

        let drm_output = match device
            .drm_output_manager
            .lock()
            .initialize_output::<GlesRenderer, UdevElements>(
                crtc,
                drm_mode,
                &[connector.handle()],
                &output,
                Some(planes),
                renderer,
                &DrmOutputRenderElements::default(),
            ) {
            Ok(drm_output) => drm_output,
            Err(err) => {
                warn!("Failed to initialize drm output: {}", err);
                return;
            }
        };

        device.surfaces.insert(
            crtc,
            SurfaceData {
                dh: self.display_handle.clone(),
                output,
                global: Some(global),
                drm_output,
                last_presentation_time: None,
            },
        );

        self.handle.insert_idle(move |data| {
            data.render_surface(node, crtc, data.clock.now());
        });
    }

    fn connector_disconnected(
        &mut self,
        node: DrmNode,
        _connector: connector::Info,
        crtc: crtc::Handle,
    ) {
        let device = match self.backend_data.backends.get_mut(&node) {
            Some(device) => device,
            None => return,
        };
        if let Some(surface) = device.surfaces.remove(&crtc) {
            self.space.unmap_output(&surface.output);
            self.tiling.remove_output(&surface.output);
            self.space.refresh();
        }
    }

    fn device_changed(&mut self, node: DrmNode) {
        let device = match self.backend_data.backends.get_mut(&node) {
            Some(device) => device,
            None => return,
        };

        let scan_result = match device.drm_scanner.scan_connectors(device.drm_output_manager.device()) {
            Ok(result) => result,
            Err(err) => {
                warn!("Failed to scan connectors: {}", err);
                return;
            }
        };

        for event in scan_result {
            match event {
                DrmScanEvent::Connected {
                    connector,
                    crtc: Some(crtc),
                } => self.connector_connected(node, connector, crtc),
                DrmScanEvent::Disconnected {
                    connector,
                    crtc: Some(crtc),
                } => self.connector_disconnected(node, connector, crtc),
                _ => {}
            }
        }
    }

    fn device_removed(&mut self, node: DrmNode) {
        let crtcs: Vec<_> = match self.backend_data.backends.get_mut(&node) {
            Some(device) => device
                .drm_scanner
                .crtcs()
                .map(|(info, crtc)| (info.clone(), crtc))
                .collect(),
            None => return,
        };

        for (connector, crtc) in crtcs {
            self.connector_disconnected(node, connector, crtc);
        }

        if let Some(device) = self.backend_data.backends.remove(&node) {
            self.handle.remove(device.registration_token);
        }
    }

    fn frame_finish(&mut self, node: DrmNode, crtc: crtc::Handle, metadata: &mut Option<DrmEventMetadata>) {
        let device = match self.backend_data.backends.get_mut(&node) {
            Some(device) => device,
            None => return,
        };
        let surface = match device.surfaces.get_mut(&crtc) {
            Some(surface) => surface,
            None => return,
        };

        let Some(output) = self
            .space
            .outputs()
            .find(|o| {
                o.user_data().get::<UdevOutputId>()
                    == Some(&UdevOutputId {
                        device_id: node,
                        crtc,
                    })
            })
            .cloned()
        else {
            return;
        };

        let Some(frame_duration) = output
            .current_mode()
            .map(|mode| Duration::from_secs_f64(1000.0 / mode.refresh as f64))
        else {
            return;
        };

        let (clock, flags) = match metadata.as_ref().map(|m| m.time) {
            Some(DrmEventTime::Monotonic(tp)) if !tp.is_zero() => (
                tp.into(),
                wp_presentation_feedback::Kind::Vsync
                    | wp_presentation_feedback::Kind::HwClock
                    | wp_presentation_feedback::Kind::HwCompletion,
            ),
            _ => (self.clock.now(), wp_presentation_feedback::Kind::Vsync),
        };
        let seq = metadata.as_ref().map(|m| m.sequence).unwrap_or(0);
        surface.last_presentation_time = Some(clock);

        let submit_result = surface
            .drm_output
            .frame_submitted()
            .map_err(Into::<smithay::backend::SwapBuffersError>::into);

        let schedule_render = match submit_result {
            Ok(user_data) => {
                if let Some(mut feedback) = user_data.flatten() {
                    feedback.presented(clock, Refresh::fixed(frame_duration), seq as u64, flags);
                }
                true
            }
            Err(err) => {
                warn!("Error finishing frame: {:?}", err);
                match err {
                    smithay::backend::SwapBuffersError::AlreadySwapped => true,
                    smithay::backend::SwapBuffersError::TemporaryFailure(err) => {
                        !matches!(err.downcast_ref::<DrmError>(), Some(DrmError::DeviceInactive))
                    }
                    smithay::backend::SwapBuffersError::ContextLost(err) => {
                        panic!("Rendering loop lost: {}", err)
                    }
                }
            }
        };

        if schedule_render {
            let next_frame_target = clock + frame_duration;
            let repaint_delay = Duration::from_secs_f64(frame_duration.as_secs_f64() * 0.6);
            self.handle
                .insert_source(Timer::from_duration(repaint_delay), move |_, _, data| {
                    data.render(node, Some(crtc), next_frame_target);
                    TimeoutAction::Drop
                })
                .expect("failed to schedule frame timer");
        }
    }

    fn render(&mut self, node: DrmNode, crtc: Option<crtc::Handle>, frame_target: Time<Monotonic>) {
        if let Some(crtc) = crtc {
            self.render_surface(node, crtc, frame_target);
        } else {
            let crtcs: Vec<_> = self
                .backend_data
                .backends
                .get(&node)
                .map(|b| b.surfaces.keys().copied().collect())
                .unwrap_or_default();
            for crtc in crtcs {
                self.render_surface(node, crtc, frame_target);
            }
        }
    }

    fn render_surface(&mut self, node: DrmNode, crtc: crtc::Handle, frame_target: Time<Monotonic>) {
        let Some(output) = self
            .space
            .outputs()
            .find(|o| {
                o.user_data().get::<UdevOutputId>()
                    == Some(&UdevOutputId {
                        device_id: node,
                        crtc,
                    })
            })
            .cloned()
        else {
            return;
        };

        let start = Instant::now();

        let frame = self
            .backend_data
            .pointer_image
            .get_image(1, self.clock.now().into());

        let pointer_images = &mut self.backend_data.pointer_images;
        let pointer_image = pointer_images
            .iter()
            .find_map(|(image, buffer)| (image == &frame).then(|| buffer.clone()))
            .unwrap_or_else(|| {
                let buffer = MemoryRenderBuffer::from_slice(
                    &frame.pixels_rgba,
                    Fourcc::Argb8888,
                    (frame.width as i32, frame.height as i32),
                    1,
                    Transform::Normal,
                    None,
                );
                pointer_images.push((frame, buffer.clone()));
                buffer
            });

        let pointer_location = self.pointer.current_location();

        let device = match self.backend_data.backends.get_mut(&node) {
            Some(device) => device,
            None => return,
        };
        let surface = match device.surfaces.get_mut(&crtc) {
            Some(surface) => surface,
            None => return,
        };
        let renderer = self
            .backend_data
            .renderer
            .as_mut()
            .expect("renderer not initialized");

        let result = render_surface(
            surface,
            renderer,
            &self.space,
            &output,
            pointer_location,
            &pointer_image,
            &mut self.backend_data.pointer_element,
            &self.dnd_icon,
            &mut self.cursor_status,
        );

        let reschedule = match result {
            Ok((has_rendered, states)) => {
                crate::state::post_repaint(self, &output, &states);
                !has_rendered
            }
            Err(err) => {
                warn!("Error during rendering: {:#?}", err);
                match err {
                    smithay::backend::SwapBuffersError::AlreadySwapped => false,
                    smithay::backend::SwapBuffersError::TemporaryFailure(err) => {
                        matches!(err.downcast_ref::<DrmError>(), Some(DrmError::DeviceInactive))
                    }
                    smithay::backend::SwapBuffersError::ContextLost(err) => {
                        panic!("Rendering loop lost: {}", err)
                    }
                }
            }
        };

        if reschedule {
            let Some(refresh) = output.current_mode().map(|m| m.refresh) else {
                return;
            };
            let next_frame_target = frame_target + Duration::from_millis(1_000_000 / refresh as u64);
            let reschedule_timeout =
                Duration::from(next_frame_target).saturating_sub(self.clock.now().into());
            self.handle
                .insert_source(Timer::from_duration(reschedule_timeout), move |_, _, data| {
                    data.render(node, Some(crtc), next_frame_target);
                    TimeoutAction::Drop
                })
                .expect("failed to schedule frame timer");
        } else {
            trace!(elapsed = ?start.elapsed(), "rendered {:?}", crtc);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_surface(
    surface: &mut SurfaceData,
    renderer: &mut GlesRenderer,
    space: &Space<Window>,
    output: &Output,
    pointer_location: Point<f64, Logical>,
    pointer_buffer: &MemoryRenderBuffer,
    pointer_element: &mut PointerElement,
    dnd_icon: &Option<crate::state::DndIcon>,
    cursor_status: &mut CursorImageStatus,
) -> Result<(bool, RenderElementStates), smithay::backend::SwapBuffersError> {
    use smithay::backend::SwapBuffersError;

    let output_geometry = space.output_geometry(output).unwrap();
    let scale = Scale::from(output.current_scale().fractional_scale());

    let mut custom_elements: Vec<UdevElements> = Vec::new();

    if output_geometry.to_f64().contains(pointer_location) {
        let hotspot = match cursor_status {
            CursorImageStatus::Surface(surface) => compositor::with_states(surface, |states| {
                states
                    .data_map
                    .get::<std::sync::Mutex<CursorImageAttributes>>()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .hotspot
            }),
            _ => (0, 0).into(),
        };
        let cursor_pos = pointer_location - output_geometry.loc.to_f64();

        pointer_element.set_buffer(pointer_buffer.clone());

        // Reset the cursor if the client surface went away.
        if let CursorImageStatus::Surface(surface) = cursor_status {
            if !surface.alive() {
                *cursor_status = CursorImageStatus::default_named();
            }
        }
        pointer_element.set_status(cursor_status.clone());

        custom_elements.extend(pointer_element.render_elements(
            renderer,
            (cursor_pos - hotspot.to_f64()).to_physical(scale).to_i32_round(),
            scale,
            1.0,
        ));

        // Draw the drag-and-drop icon, if any.
        if let Some(icon) = dnd_icon.as_ref() {
            if icon.surface.alive() {
                let dnd_pos = (cursor_pos + icon.offset.to_f64())
                    .to_physical(scale)
                    .to_i32_round();
                let tree = smithay::desktop::space::SurfaceTree::from_surface(&icon.surface);
                let icon_elements: Vec<crate::render::CursorRenderElement<GlesRenderer>> =
                    tree.render_elements(renderer, dnd_pos, scale, 1.0);
                custom_elements.extend(icon_elements.into_iter().map(OutputRenderElements::Cursor));
            }
        }
    }

    let space_elements = smithay::desktop::space::space_render_elements::<GlesRenderer, Window, _>(
        renderer,
        [space],
        output,
        1.0,
    )
    .expect("output without mode");
    let mut elements: Vec<UdevElements> = space_elements.into_iter().map(OutputRenderElements::Space).collect();
    elements.extend(custom_elements);

    let (rendered, states) = surface
        .drm_output
        .render_frame(renderer, &elements, CLEAR_COLOR, FrameFlags::DEFAULT)
        .map(|result| (!result.is_empty, result.states))
        .map_err(|err| match err {
            RenderFrameError::PrepareFrame(err) => SwapBuffersError::from(err),
            RenderFrameError::RenderFrame(OutputDamageTrackerError::Rendering(err)) => {
                SwapBuffersError::from(err)
            }
            _ => unreachable!(),
        })?;

    update_primary_scanout_output(space, output, cursor_status, dnd_icon, &states);

    if rendered {
        let feedback = take_presentation_feedback(output, space, &states);
        surface
            .drm_output
            .queue_frame(Some(feedback))
            .map_err(Into::<SwapBuffersError>::into)?;
    }

    Ok((rendered, states))
}
