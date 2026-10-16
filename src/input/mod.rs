//! Input handling: keyboard shortcuts and pointer interactions.

use smithay::{
    backend::input::{
        AbsolutePositionEvent, Axis, AxisSource, ButtonState, Event, InputBackend, InputEvent,
        KeyState, KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
    },
    input::{
        keyboard::{FilterResult, Keysym, ModifiersState},
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point, SERIAL_COUNTER},
};

use crate::{
    config::Config,
    state::{Backend, Pluto},
};

#[derive(Debug, Clone)]
pub enum Action {
    Quit,
    SpawnTerminal,
    CloseWindow,
    FocusNext,
    FocusPrev,
    Promote,
    MoveUp,
    MoveDown,
    VtSwitch(i32),
    None,
}

    fn process_keyboard_shortcut(modifiers: ModifiersState, keysym: Keysym, _config: &Config) -> Option<Action> {
    let logo = modifiers.logo;
    if logo && modifiers.shift && keysym == Keysym::Q {
        Some(Action::Quit)
    } else if logo && keysym == Keysym::Return {
        Some(Action::SpawnTerminal)
    } else if logo && keysym == Keysym::q {
        Some(Action::CloseWindow)
    } else if logo && keysym == Keysym::j {
        Some(Action::FocusNext)
    } else if logo && keysym == Keysym::k {
        Some(Action::FocusPrev)
    } else if logo && keysym == Keysym::space {
        Some(Action::Promote)
    } else if logo && modifiers.shift && keysym == Keysym::j {
        Some(Action::MoveDown)
    } else if logo && modifiers.shift && keysym == Keysym::k {
        Some(Action::MoveUp)
    } else {
        None
    }
}

impl<B: Backend + 'static> Pluto<B> {
    pub fn process_input_event<I: InputBackend>(&mut self, event: InputEvent<I>) {
        match event {
            InputEvent::Keyboard { event, .. } => {
                let action = self.keyboard_key_to_action::<I>(event);
                self.dispatch_action(action);
            }
            InputEvent::PointerMotion { event, .. } => {
                self.on_pointer_motion::<I>(event);
            }
            InputEvent::PointerMotionAbsolute { event, .. } => {
                self.on_pointer_motion_absolute::<I>(event);
            }
            InputEvent::PointerButton { event, .. } => {
                self.on_pointer_button::<I>(event);
            }
            InputEvent::PointerAxis { event, .. } => {
                self.on_pointer_axis::<I>(event);
            }
            _ => {}
        }
    }

    fn dispatch_action(&mut self, action: Action) {
        match action {
            Action::Quit => self.stop(),
            Action::SpawnTerminal => {
                let cmd = self.config.terminal.clone();
                let _ = std::process::Command::new(cmd).spawn();
            }
            Action::CloseWindow => self.close_focused(),
            Action::FocusNext => self.focus_cycle(true),
            Action::FocusPrev => self.focus_cycle(false),
            Action::Promote => self.promote_focused(),
            Action::MoveUp => self.move_focused_up(),
            Action::MoveDown => self.move_focused_down(),
            Action::VtSwitch(_) => {}
            Action::None => {}
        }
    }

    fn keyboard_key_to_action<I: InputBackend>(&mut self, event: I::KeyboardKeyEvent) -> Action {
        let keycode = event.key_code();
        let state = event.state();
        let serial = SERIAL_COUNTER.next_serial();
        let time = event.time();
        let config = self.config.clone();

        let keyboard = self.seat.get_keyboard().unwrap();
        keyboard
            .input::<Action, _>(self, keycode, state, serial, time, |_, modifiers, handle| {
                let keysym = handle.modified_sym();
                if KeyState::Pressed == state {
                    if let Some(action) = process_keyboard_shortcut(*modifiers, keysym, &config) {
                        FilterResult::Intercept(action)
                    } else {
                        FilterResult::Forward
                    }
                } else {
                    FilterResult::Forward
                }
            })
            .unwrap_or(Action::None)
    }

    fn surface_under(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        let (window, location) = self.window_under(pos)?;
        let surface = window.toplevel()?.wl_surface().clone();
        Some((surface, location))
    }

    /// Focus-follows-mouse: move keyboard focus to whatever window is under the
    /// cursor. Windows behind the cursor keep focus while the cursor is over
    /// empty space, so typing isn't interrupted by briefly moving off a window.
    fn focus_under_cursor(&mut self, pos: Point<f64, Logical>) {
        if let Some((window, _)) = self.window_under(pos) {
            if self.focused_window.as_ref() != Some(&window) {
                self.focus_window(Some(window));
            }
        }
    }

    fn on_pointer_motion<I: InputBackend>(&mut self, event: I::PointerMotionEvent) {
        let delta = event.delta();
        let serial = SERIAL_COUNTER.next_serial();
        let pos = self.pointer.current_location() + delta;
        let under = self.surface_under(pos);
        self.focus_under_cursor(pos);
        let pointer = self.pointer.clone();
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pos,
                serial,
                time: event.time(),
            },
        );
        pointer.frame(self);
    }

    fn on_pointer_motion_absolute<I: InputBackend>(&mut self, event: I::PointerMotionAbsoluteEvent) {
        let serial = SERIAL_COUNTER.next_serial();
        let output_size = self
            .space
            .outputs()
            .next()
            .and_then(|o| self.space.output_geometry(o))
            .map(|g| g.size)
            .unwrap_or((1, 1).into());
        let pos = event.position_transformed(output_size);
        let under = self.surface_under(pos);
        self.focus_under_cursor(pos);
        let pointer = self.pointer.clone();
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: pos,
                serial,
                time: event.time(),
            },
        );
        pointer.frame(self);
    }

    fn on_pointer_button<I: InputBackend>(&mut self, event: I::PointerButtonEvent) {
        let serial = SERIAL_COUNTER.next_serial();
        let button = event.button_code();
        let state = event.state();

        if ButtonState::Pressed == state {
            let pos = self.pointer.current_location();
            if let Some((window, _)) = self.window_under(pos) {
                self.focus_window(Some(window));
            }
        }

        let pointer = self.pointer.clone();
        pointer.button(
            self,
            &ButtonEvent {
                button,
                state: state.try_into().unwrap(),
                serial,
                time: event.time(),
            },
        );
        pointer.frame(self);
    }

    fn on_pointer_axis<I: InputBackend>(&mut self, event: I::PointerAxisEvent) {
        // Mouse wheels only report discrete steps (`amount_v120`); touchpads and
        // other continuous sources report pixel amounts. Fall back to the v120
        // value scaled to pixels so a mouse wheel scrolls too.
        let horizontal_amount = event
            .amount(Axis::Horizontal)
            .unwrap_or_else(|| event.amount_v120(Axis::Horizontal).unwrap_or(0.0) * 15.0 / 120.0);
        let vertical_amount = event
            .amount(Axis::Vertical)
            .unwrap_or_else(|| event.amount_v120(Axis::Vertical).unwrap_or(0.0) * 15.0 / 120.0);
        let horizontal_discrete = event.amount_v120(Axis::Horizontal);
        let vertical_discrete = event.amount_v120(Axis::Vertical);

        let mut frame = AxisFrame::new(event.time()).source(event.source());
        if horizontal_amount != 0.0 {
            frame = frame.relative_direction(
                Axis::Horizontal,
                event.relative_direction(Axis::Horizontal),
            );
            frame = frame.value(Axis::Horizontal, horizontal_amount);
            if let Some(discrete) = horizontal_discrete {
                frame = frame.v120(Axis::Horizontal, discrete as i32);
            }
        }
        if vertical_amount != 0.0 {
            frame =
                frame.relative_direction(Axis::Vertical, event.relative_direction(Axis::Vertical));
            frame = frame.value(Axis::Vertical, vertical_amount);
            if let Some(discrete) = vertical_discrete {
                frame = frame.v120(Axis::Vertical, discrete as i32);
            }
        }

        if event.source() == AxisSource::Finger {
            if event.amount(Axis::Horizontal) == Some(0.0) {
                frame = frame.stop(Axis::Horizontal);
            }
            if event.amount(Axis::Vertical) == Some(0.0) {
                frame = frame.stop(Axis::Vertical);
            }
        }

        let pointer = self.pointer.clone();
        pointer.axis(self, frame);
        pointer.frame(self);
    }
}
