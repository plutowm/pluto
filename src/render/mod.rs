//! Render element types shared by the backends.
//!
//! Winit renders the desktop space directly. The udev backend additionally has
//! to composite its own cursor, so it uses [`OutputRenderElements`].

#[cfg(feature = "udev")]
pub mod cursor;

use smithay::backend::renderer::Color32F;

#[cfg(feature = "udev")]
pub use cursor::Cursor;

/// Background color used for every output.
pub static CLEAR_COLOR: Color32F = Color32F::new(0.12, 0.12, 0.16, 1.0);

#[cfg(feature = "udev")]
mod elements {
    use smithay::{
        backend::renderer::{
            ImportAll, ImportMem, Renderer, Texture,
            element::{
                AsRenderElements, Kind, RenderElement,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
            },
        },
        desktop::space::SpaceRenderElements,
        input::pointer::CursorImageStatus,
        render_elements,
        utils::{Physical, Point, Scale},
    };

    render_elements! {
        /// A cursor image, either a client-provided surface or one loaded from a
        /// cursor theme.
        pub CursorRenderElement<R> where
            R: ImportAll + ImportMem;
        Surface=WaylandSurfaceRenderElement<R>,
        Memory=MemoryRenderBufferRenderElement<R>,
    }

    impl<R: Renderer> std::fmt::Debug for CursorRenderElement<R> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Surface(arg0) => f.debug_tuple("Surface").field(arg0).finish(),
                Self::Memory(arg0) => f.debug_tuple("Memory").field(arg0).finish(),
                Self::_GenericCatcher(arg0) => f.debug_tuple("_GenericCatcher").field(arg0).finish(),
            }
        }
    }

    render_elements! {
        /// Everything the udev backend draws on an output: the desktop space plus
        /// custom overlay elements (the cursor, drag icons).
        pub OutputRenderElements<R, E> where
            R: ImportAll + ImportMem;
        Space=SpaceRenderElements<R, E>,
        Cursor=CursorRenderElement<R>,
    }

    impl<R, E> std::fmt::Debug for OutputRenderElements<R, E>
    where
        R: Renderer + ImportAll + ImportMem,
        E: RenderElement<R> + std::fmt::Debug,
    {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Space(arg0) => f.debug_tuple("Space").field(arg0).finish(),
                Self::Cursor(arg0) => f.debug_tuple("Cursor").field(arg0).finish(),
                Self::_GenericCatcher(arg0) => f.debug_tuple("_GenericCatcher").field(arg0).finish(),
            }
        }
    }

    /// Software cursor: holds the currently selected cursor buffer (loaded from
    /// the theme by the backend) and the logical cursor status.
    #[derive(Debug)]
    pub struct PointerElement {
        buffer: Option<MemoryRenderBuffer>,
        status: CursorImageStatus,
    }

    impl Default for PointerElement {
        fn default() -> Self {
            Self {
                buffer: None,
                status: CursorImageStatus::default_named(),
            }
        }
    }

    impl PointerElement {
        pub fn set_status(&mut self, status: CursorImageStatus) {
            self.status = status;
        }

        pub fn set_buffer(&mut self, buffer: MemoryRenderBuffer) {
            self.buffer = Some(buffer);
        }
    }

    impl<T: Texture + Clone + Send + 'static, R> AsRenderElements<R> for PointerElement
    where
        R: Renderer<TextureId = T> + ImportAll + ImportMem,
    {
        type RenderElement = CursorRenderElement<R>;

        fn render_elements<E>(
            &self,
            renderer: &mut R,
            location: Point<i32, Physical>,
            scale: Scale<f64>,
            alpha: f32,
        ) -> Vec<E>
        where
            E: From<CursorRenderElement<R>>,
        {
            match &self.status {
                CursorImageStatus::Hidden => vec![],
                // Always render the named shape from the theme buffer.
                CursorImageStatus::Named(_) => {
                    if let Some(buffer) = self.buffer.as_ref() {
                        vec![
                            CursorRenderElement::<R>::from(
                                MemoryRenderBufferRenderElement::from_buffer(
                                    renderer,
                                    location.to_f64(),
                                    buffer,
                                    None,
                                    None,
                                    None,
                                    Kind::Cursor,
                                )
                                .expect("Lost system pointer buffer"),
                            )
                            .into(),
                        ]
                    } else {
                        vec![]
                    }
                }
                CursorImageStatus::Surface(surface) => {
                    let elements: Vec<CursorRenderElement<R>> =
                        render_elements_from_surface_tree(
                            renderer,
                            surface,
                            location,
                            scale,
                            alpha,
                            Kind::Cursor,
                        );
                    elements.into_iter().map(E::from).collect()
                }
            }
        }
    }
}

#[cfg(feature = "udev")]
pub use elements::{CursorRenderElement, OutputRenderElements, PointerElement};
