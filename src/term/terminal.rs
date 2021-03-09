use super::*;
use crate::core::escape::parser::Parser;
use crate::core::hyperlink::Rule as HyperlinkRule;
use failure::Fallible;
use std::sync::Arc;

pub trait Clipboard {
    fn get_contents(&self) -> Fallible<String>;
    fn set_contents(&self, data: Option<String>) -> Fallible<()>;
}

/// Represents the host of the terminal.
/// Provides a means for sending data to the connected pty,
/// and for operating on the clipboard
pub trait TerminalHost {
    /// Returns an object that can be used to send data to the
    /// slave end of the associated pty.
    fn writer(&mut self) -> &mut dyn std::io::Write;

    /// Returns the clipboard manager
    fn get_clipboard(&mut self) -> Fallible<Arc<dyn Clipboard>>;

    /// Change the title of the window
    fn set_title(&mut self, title: &str);

    /// Called when a URL is clicked
    fn click_link(&mut self, link: &Arc<Hyperlink>);

    /// Switch to a specific tab
    fn activate_tab(&mut self, _tab: usize) {}

    /// Activate a relative tab number
    fn activate_tab_relative(&mut self, _delta: isize) {}

    /// Toggle full-screen mode
    fn toggle_full_screen(&mut self) {}

    /// Increase font size by one step
    fn increase_font_size(&mut self) {}

    /// Decrease font size by one step
    fn decrease_font_size(&mut self) {}

    /// Reset font size
    fn reset_font_size(&mut self) {}
}

pub struct Terminal {
    /// The terminal model/state
    state: TerminalState,
    /// Baseline terminal escape sequence parser
    parser: Parser,
}

impl Deref for Terminal {
    type Target = TerminalState;

    fn deref(&self) -> &TerminalState {
        &self.state
    }
}

impl DerefMut for Terminal {
    fn deref_mut(&mut self) -> &mut TerminalState {
        &mut self.state
    }
}

impl Terminal {
    pub fn new(
        physical_rows: usize,
        physical_cols: usize,
        pixel_width: usize,
        pixel_height: usize,
        scrollback_size: usize,
        hyperlink_rules: Vec<HyperlinkRule>,
    ) -> Terminal {
        Terminal {
            state: TerminalState::new(
                physical_rows,
                physical_cols,
                pixel_height,
                pixel_width,
                scrollback_size,
                hyperlink_rules,
            ),
            parser: Parser::new(),
        }
    }

    /// Feed the terminal parser a slice of bytes of input.
    pub fn advance_bytes<B: AsRef<[u8]>>(&mut self, bytes: B, host: &mut dyn TerminalHost) {
        let bytes = bytes.as_ref();

        let mut performer = Performer::new(&mut self.state, host);

        self.parser.parse(bytes, |action| performer.perform(action));
    }
}
