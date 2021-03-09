use crate::core::escape::{Action, DeviceControlMode, Esc, OperatingSystemCommand, CSI};
use log::error;
use num;
use std::cell::RefCell;
use vtparse::{VTActor, VTParser};

/// The `Parser` struct holds the state machine that is used to decode
/// a sequence of bytes.  The byte sequence can be streaming into the
/// state machine.
/// You can either have the parser trigger a callback as `Action`s are
/// decoded, or have it return a `Vec<Action>` holding zero-or-more
/// decoded actions.
pub struct Parser {
    state_machine: VTParser,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub fn new() -> Self {
        Self { state_machine: VTParser::new() }
    }

    pub fn parse<F: FnMut(Action)>(&mut self, bytes: &[u8], mut callback: F) {
        let mut perform = Performer { callback: &mut callback };
        self.state_machine.parse(bytes, &mut perform);
    }

    /// A specialized version of the parser that halts after recognizing the
    /// first action from the stream of bytes.  The return value is the action
    /// that was recognized and the length of the byte stream that was fed in
    /// to the parser to yield it.
    pub fn parse_first(&mut self, bytes: &[u8]) -> Option<(Action, usize)> {
        // holds the first action.  We need to use RefCell to deal with
        // the Performer holding a reference to this via the closure we set up.
        let first = RefCell::new(None);
        // will hold the iterator index when we emit an action
        let mut first_idx = None;
        {
            let mut perform = Performer {
                callback: &mut |action| {
                    // capture the action, but only if it is the first one
                    // we've seen.  Preserve an existing one if any.
                    if first.borrow().is_some() {
                        return;
                    }
                    *first.borrow_mut() = Some(action);
                },
            };
            for (idx, b) in bytes.iter().enumerate() {
                self.state_machine.parse_byte(*b, &mut perform);
                if first.borrow().is_some() {
                    // if we recognized an action, record the iterator index
                    first_idx = Some(idx);
                    break;
                }
            }
        }

        match (first.into_inner(), first_idx) {
            // if we matched an action, transform the iterator index to
            // the length of the string that was consumed (+1)
            (Some(action), Some(idx)) => Some((action, idx + 1)),
            _ => None,
        }
    }

    pub fn parse_as_vec(&mut self, bytes: &[u8]) -> Vec<Action> {
        let mut result = Vec::new();
        self.parse(bytes, |action| result.push(action));
        result
    }

    /// Similar to `parse_first` but collects all actions from the first sequence.
    pub fn parse_first_as_vec(&mut self, bytes: &[u8]) -> Option<(Vec<Action>, usize)> {
        let mut actions = Vec::new();
        let mut first_idx = None;
        for (idx, b) in bytes.iter().enumerate() {
            self.state_machine
                .parse_byte(*b, &mut Performer { callback: &mut |action| actions.push(action) });
            if !actions.is_empty() {
                // if we recognized any actions, record the iterator index
                first_idx = Some(idx);
                break;
            }
        }
        first_idx.map(|idx| (actions, idx + 1))
    }
}

struct Performer<'a, F: FnMut(Action) + 'a> {
    callback: &'a mut F,
}

impl<'a, F: FnMut(Action)> VTActor for Performer<'a, F> {
    fn print(&mut self, c: char) {
        (self.callback)(Action::Print(c));
    }

    fn execute_c0_or_c1(&mut self, byte: u8) {
        match num::FromPrimitive::from_u8(byte) {
            Some(code) => (self.callback)(Action::Control(code)),
            None => error!("impossible C0/C1 control code {:?} was dropped", byte),
        }
    }

    fn dcs_hook(
        &mut self,
        params: &[i64],
        intermediates: &[u8],
        ignored_extra_intermediates: bool,
    ) {
        (self.callback)(Action::DeviceControl(Box::new(DeviceControlMode::Enter {
            params: params.to_vec(),
            intermediates: intermediates.to_vec(),
            ignored_extra_intermediates,
        })));
    }

    fn dcs_put(&mut self, data: u8) {
        (self.callback)(Action::DeviceControl(Box::new(DeviceControlMode::Data(data))));
    }

    fn dcs_unhook(&mut self) {
        (self.callback)(Action::DeviceControl(Box::new(DeviceControlMode::Exit)));
    }

    fn osc_dispatch(&mut self, osc: &[&[u8]]) {
        let osc = OperatingSystemCommand::parse(osc);
        (self.callback)(Action::OperatingSystemCommand(Box::new(osc)));
    }

    fn csi_dispatch(
        &mut self,
        params: &[i64],
        intermediates: &[u8],
        ignored_extra_intermediates: bool,
        control: u8,
    ) {
        for action in
            CSI::parse(params, intermediates, ignored_extra_intermediates, control as char)
        {
            (self.callback)(Action::CSI(action));
        }
    }

    fn esc_dispatch(
        &mut self,
        _params: &[i64],
        intermediates: &[u8],
        _ignored_extra_intermediates: bool,
        control: u8,
    ) {
        // It doesn't appear to be possible for params.len() > 1 due to the way
        // that the state machine in vte functions.  As such, it also seems to
        // be impossible for ignored_extra_intermediates to be true too.
        (self.callback)(Action::Esc(Esc::parse(
            if intermediates.len() == 1 { Some(intermediates[0]) } else { None },
            control,
        )));
    }
}
