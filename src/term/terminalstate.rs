use super::*;
use crate::core::escape::csi::{
    Cursor, DecPrivateMode, DecPrivateModeCode, Device, Edit, EraseInDisplay, EraseInLine, Mode,
    Sgr, TerminalMode, TerminalModeCode, Window,
};
use crate::core::escape::osc::{ChangeColorPair, ColorOrQuery};
use crate::core::escape::{
    Action, ControlCode, Esc, EscCode, OneBased, OperatingSystemCommand, CSI,
};
use crate::core::hyperlink::Rule as HyperlinkRule;
use crate::term::color::ColorPalette;
use anyhow::bail;
use log::{debug, error};
use std::fmt::Write;
use std::sync::Arc;

struct TabStop {
    tabs: Vec<bool>,
    tab_width: usize,
}

impl TabStop {
    fn new(screen_width: usize, tab_width: usize) -> Self {
        let mut tabs = Vec::with_capacity(screen_width);

        for i in 0..screen_width {
            tabs.push((i % tab_width) == 0);
        }
        Self { tabs, tab_width }
    }

    fn set_tab_stop(&mut self, col: usize) {
        self.tabs[col] = true;
    }

    fn find_next_tab_stop(&self, col: usize) -> Option<usize> {
        for i in col + 1..self.tabs.len() {
            if self.tabs[i] {
                return Some(i);
            }
        }
        None
    }

    fn resize(&mut self, screen_width: usize) {
        let current = self.tabs.len();
        if screen_width > current {
            for i in current..screen_width {
                self.tabs.push((i % self.tab_width) == 0);
            }
        }
    }
}

#[derive(Debug, Copy, Clone)]
struct SavedCursor {
    position: CursorPosition,
    wrap_next: bool,
    insert: bool,
}

struct ScreenOrAlt {
    screen: Screen,

    alt_screen: Screen,

    alt_screen_is_active: bool,
    saved_cursor: Option<SavedCursor>,
    alt_saved_cursor: Option<SavedCursor>,
}

impl Deref for ScreenOrAlt {
    type Target = Screen;

    fn deref(&self) -> &Screen {
        if self.alt_screen_is_active {
            &self.alt_screen
        } else {
            &self.screen
        }
    }
}

impl DerefMut for ScreenOrAlt {
    fn deref_mut(&mut self) -> &mut Screen {
        if self.alt_screen_is_active {
            &mut self.alt_screen
        } else {
            &mut self.screen
        }
    }
}

impl ScreenOrAlt {
    pub fn new(physical_rows: usize, physical_cols: usize, scrollback_size: usize) -> Self {
        let screen = Screen::new(physical_rows, physical_cols, scrollback_size);
        let alt_screen = Screen::new(physical_rows, physical_cols, 0);

        Self {
            screen,
            alt_screen,
            alt_screen_is_active: false,
            saved_cursor: None,
            alt_saved_cursor: None,
        }
    }

    pub fn resize(&mut self, physical_rows: usize, physical_cols: usize) {
        self.screen.resize(physical_rows, physical_cols);
        self.alt_screen.resize(physical_rows, physical_cols);
    }

    pub fn activate_alt_screen(&mut self) {
        self.alt_screen_is_active = true;
    }

    pub fn activate_primary_screen(&mut self) {
        self.alt_screen_is_active = false;
    }

    pub fn is_alt_screen_active(&self) -> bool {
        self.alt_screen_is_active
    }

    pub fn saved_cursor(&mut self) -> &mut Option<SavedCursor> {
        if self.alt_screen_is_active {
            &mut self.alt_saved_cursor
        } else {
            &mut self.saved_cursor
        }
    }
}

pub struct TerminalState {
    screen: ScreenOrAlt,
    pen: CellAttributes,
    cursor: CursorPosition,
    wrap_next: bool,
    insert: bool,
    scroll_region: Range<VisibleRowIndex>,
    application_cursor_keys: bool,
    application_keypad: bool,
    bracketed_paste: bool,
    sgr_mouse: bool,
    button_event_mouse: bool,
    current_mouse_button: MouseButton,
    mouse_position: CursorPosition,
    cursor_visible: bool,
    dec_line_drawing_mode: bool,
    current_highlight: Option<Arc<Hyperlink>>,
    last_mouse_click: Option<LastMouseClick>,
    pub(crate) viewport_offset: VisibleRowIndex,
    selection_start: Option<SelectionCoordinate>,
    selection_range: Option<SelectionRange>,
    tabs: TabStop,
    hyperlink_rules: Vec<HyperlinkRule>,
    title: String,
    palette: ColorPalette,
    pixel_width: usize,
    pixel_height: usize,
}

fn is_double_click_word(s: &str) -> bool {
    if s.len() > 1 {
        true
    } else if s.len() == 1 {
        match s.chars().nth(0).unwrap() {
            ' ' | '\t' | '\n' | '{' | '[' | '}' | ']' | '(' | ')' | '"' | '\'' => false,
            _ => true,
        }
    } else {
        false
    }
}

impl TerminalState {
    pub fn new(
        physical_rows: usize,
        physical_cols: usize,
        pixel_width: usize,
        pixel_height: usize,
        scrollback_size: usize,
        hyperlink_rules: Vec<HyperlinkRule>,
    ) -> TerminalState {
        let screen = ScreenOrAlt::new(physical_rows, physical_cols, scrollback_size);

        TerminalState {
            screen,
            pen: CellAttributes::default(),
            cursor: CursorPosition::default(),
            scroll_region: 0..physical_rows as VisibleRowIndex,
            wrap_next: false,
            insert: false,
            application_cursor_keys: false,
            application_keypad: false,
            bracketed_paste: false,
            sgr_mouse: false,
            button_event_mouse: false,
            cursor_visible: true,
            dec_line_drawing_mode: false,
            current_mouse_button: MouseButton::None,
            mouse_position: CursorPosition::default(),
            current_highlight: None,
            last_mouse_click: None,
            viewport_offset: 0,
            selection_range: None,
            selection_start: None,
            tabs: TabStop::new(physical_cols, 8),
            hyperlink_rules,
            title: "miro".to_string(),
            palette: ColorPalette::default(),
            pixel_height,
            pixel_width,
        }
    }

    pub fn get_title(&self) -> &str {
        &self.title
    }

    pub fn palette(&self) -> &ColorPalette {
        &self.palette
    }

    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    pub fn screen_mut(&mut self) -> &mut Screen {
        &mut self.screen
    }

    pub fn get_selection_text(&self) -> String {
        let mut s = String::new();

        if let Some(sel) = self.selection_range.as_ref().map(|r| r.normalize()) {
            let screen = self.screen();
            let mut last_was_wrapped = false;
            for y in sel.rows() {
                let idx = screen.scrollback_or_visible_row(y);
                let cols = sel.cols_for_row(y);
                let last_col_idx = cols.end.min(screen.lines[idx].cells().len()) - 1;
                if !s.is_empty() && !last_was_wrapped {
                    s.push('\n');
                }
                s.push_str(screen.lines[idx].columns_as_str(cols).trim_end());

                let last_cell = &screen.lines[idx].cells()[last_col_idx];

                last_was_wrapped = last_cell.attrs().wrapped() && last_cell.str() != " ";
            }
        }

        s
    }

    fn dirty_selection_lines(&mut self) {
        if let Some(sel) = self.selection_range.as_ref().map(|r| r.normalize()) {
            let screen = self.screen_mut();
            for y in screen.scrollback_or_visible_range(&sel.rows()) {
                screen.line_mut(y).set_dirty();
            }
        }
    }

    pub fn clear_selection(&mut self) {
        self.dirty_selection_lines();
        self.selection_range = None;
        self.selection_start = None;
    }

    fn clear_selection_if_intersects(
        &mut self,
        cols: Range<usize>,
        row: ScrollbackOrVisibleRowIndex,
    ) -> bool {
        let sel = self.selection_range.take();
        match sel {
            Some(sel) => {
                let sel = sel.normalize();
                let sel_cols = sel.cols_for_row(row);
                if intersects_range(cols, sel_cols) {
                    self.clear_selection();
                    true
                } else {
                    self.selection_range = Some(sel);
                    false
                }
            }
            None => true,
        }
    }

    fn clear_selection_if_intersects_rows(
        &mut self,
        rows: Range<ScrollbackOrVisibleRowIndex>,
    ) -> bool {
        let sel = self.selection_range.take();
        match sel {
            Some(sel) => {
                let sel_rows = sel.rows();
                if intersects_range(rows, sel_rows) {
                    self.clear_selection();
                    true
                } else {
                    self.selection_range = Some(sel);
                    false
                }
            }
            None => true,
        }
    }

    fn hyperlink_for_cell(
        &mut self,
        x: usize,
        y: ScrollbackOrVisibleRowIndex,
    ) -> Option<Arc<Hyperlink>> {
        let rules = &self.hyperlink_rules;

        let idx = self.screen.scrollback_or_visible_row(y);
        match self.screen.lines.get_mut(idx) {
            Some(ref mut line) => {
                line.scan_and_create_hyperlinks(rules);
                match line.cells().get(x) {
                    Some(cell) => cell.attrs().hyperlink.as_ref().cloned(),
                    None => None,
                }
            }
            None => None,
        }
    }

    fn invalidate_hyperlinks(&mut self) {
        let screen = self.screen_mut();
        for line in &mut screen.lines {
            if line.has_hyperlink() {
                line.set_dirty();
            }
        }
    }

    fn recompute_highlight(&mut self) {
        let line_idx = self.mouse_position.y as ScrollbackOrVisibleRowIndex
            - self.viewport_offset as ScrollbackOrVisibleRowIndex;
        let x = self.mouse_position.x;
        self.current_highlight = self.hyperlink_for_cell(x, line_idx);
        self.invalidate_hyperlinks();
    }

    fn mouse_single_click_left(
        &mut self,
        event: MouseEvent,
        host: &mut dyn TerminalHost,
    ) -> anyhow::Result<()> {
        self.selection_range = None;
        self.selection_start = Some(SelectionCoordinate {
            x: event.x,
            y: event.y as ScrollbackOrVisibleRowIndex
                - self.viewport_offset as ScrollbackOrVisibleRowIndex,
        });
        host.get_clipboard()?.set_contents(None)
    }

    fn mouse_double_click_left(
        &mut self,
        event: MouseEvent,
        host: &mut dyn TerminalHost,
    ) -> anyhow::Result<()> {
        let y = event.y as ScrollbackOrVisibleRowIndex
            - self.viewport_offset as ScrollbackOrVisibleRowIndex;

        let idx = self.screen().scrollback_or_visible_row(y);
        let selection_range = match self.screen().lines[idx]
            .compute_double_click_range(event.x, is_double_click_word)
        {
            DoubleClickRange::Range(click_range) => SelectionRange {
                start: SelectionCoordinate { x: click_range.start, y },
                end: SelectionCoordinate { x: click_range.end - 1, y },
            },
            DoubleClickRange::RangeWithWrap(range_start) => {
                let start_coord = SelectionCoordinate { x: range_start.start, y };

                let mut end_coord = SelectionCoordinate { x: range_start.end - 1, y };

                for y_cont in idx + 1..self.screen().lines.len() {
                    match self.screen().lines[y_cont]
                        .compute_double_click_range(0, is_double_click_word)
                    {
                        DoubleClickRange::Range(range_end) => {
                            if range_end.end > range_end.start {
                                end_coord = SelectionCoordinate {
                                    x: range_end.end - 1,
                                    y: y + (y_cont - idx) as i32,
                                };
                            }
                            break;
                        }
                        DoubleClickRange::RangeWithWrap(range_end) => {
                            end_coord = SelectionCoordinate {
                                x: range_end.end - 1,
                                y: y + (y_cont - idx) as i32,
                            };
                        }
                    }
                }

                SelectionRange { start: start_coord, end: end_coord }
            }
        };

        self.selection_start = Some(selection_range.start);
        self.selection_range = Some(selection_range);

        self.dirty_selection_lines();
        let text = self.get_selection_text();
        debug!("finish 2click selection {:?} '{}'", self.selection_range, text);
        host.get_clipboard()?.set_contents(Some(text))
    }

    fn mouse_triple_click_left(
        &mut self,
        event: MouseEvent,
        host: &mut dyn TerminalHost,
    ) -> anyhow::Result<()> {
        let y = event.y as ScrollbackOrVisibleRowIndex
            - self.viewport_offset as ScrollbackOrVisibleRowIndex;
        self.selection_start = Some(SelectionCoordinate { x: event.x, y });
        self.selection_range = Some(SelectionRange {
            start: SelectionCoordinate { x: 0, y },
            end: SelectionCoordinate { x: usize::max_value(), y },
        });
        self.dirty_selection_lines();
        let text = self.get_selection_text();
        debug!("finish 3click selection {:?} '{}'", self.selection_range, text);
        host.get_clipboard()?.set_contents(Some(text))
    }

    fn mouse_press_left(
        &mut self,
        event: MouseEvent,
        host: &mut dyn TerminalHost,
    ) -> anyhow::Result<()> {
        self.current_mouse_button = MouseButton::Left;
        self.dirty_selection_lines();
        match self.last_mouse_click.as_ref() {
            Some(&LastMouseClick { streak: 1, .. }) => {
                self.mouse_single_click_left(event, host)?;
            }
            Some(&LastMouseClick { streak: 2, .. }) => {
                self.mouse_double_click_left(event, host)?;
            }
            Some(&LastMouseClick { streak: 3, .. }) => {
                self.mouse_triple_click_left(event, host)?;
            }

            _ => {
                self.selection_range = None;
                self.selection_start = None;
                host.get_clipboard()?.set_contents(None)?;
            }
        }

        Ok(())
    }

    fn mouse_release_left(
        &mut self,
        event: MouseEvent,
        host: &mut dyn TerminalHost,
    ) -> anyhow::Result<()> {
        self.current_mouse_button = MouseButton::None;
        if let Some(&LastMouseClick { streak: 1, .. }) = self.last_mouse_click.as_ref() {
            let text = self.get_selection_text();
            if !text.is_empty() {
                debug!("finish drag selection {:?} '{}'", self.selection_range, text);
                host.get_clipboard()?.set_contents(Some(text))?;
            } else if let Some(link) = self.current_highlight() {
                host.click_link(&link);
            }
            Ok(())
        } else {
            self.mouse_button_release(event, host.writer())
        }
    }

    fn mouse_drag_left(&mut self, event: MouseEvent) -> anyhow::Result<()> {
        self.dirty_selection_lines();
        let end = SelectionCoordinate {
            x: event.x,
            y: event.y as ScrollbackOrVisibleRowIndex
                - self.viewport_offset as ScrollbackOrVisibleRowIndex,
        };
        let sel = match self.selection_range.take() {
            None => SelectionRange::start(self.selection_start.unwrap_or(end)).extend(end),
            Some(sel) => sel.extend(end),
        };
        self.selection_range = Some(sel);

        self.dirty_selection_lines();
        Ok(())
    }

    fn mouse_wheel(
        &mut self,
        event: MouseEvent,
        writer: &mut dyn std::io::Write,
    ) -> anyhow::Result<()> {
        let (report_button, scroll_delta, key) = match event.button {
            MouseButton::WheelUp(amount) => (64, -(amount as i64), KeyCode::UpArrow),
            MouseButton::WheelDown(amount) => (65, amount as i64, KeyCode::DownArrow),
            _ => bail!("unexpected mouse event {:?}", event),
        };

        if self.sgr_mouse {
            writer.write_all(
                format!("\x1b[<{};{};{}M", report_button, event.x + 1, event.y + 1).as_bytes(),
            )?;
        } else if self.screen.is_alt_screen_active() {
            self.key_down(key, KeyModifiers::default(), writer)?;
        } else {
            self.scroll_viewport(scroll_delta)
        }
        Ok(())
    }

    fn mouse_button_press(
        &mut self,
        event: MouseEvent,
        host: &mut dyn TerminalHost,
    ) -> anyhow::Result<()> {
        self.current_mouse_button = event.button;
        if let Some(button) = match event.button {
            MouseButton::Left => Some(0),
            MouseButton::Middle => Some(1),
            MouseButton::Right => Some(2),
            _ => None,
        } {
            if self.sgr_mouse {
                host.writer().write_all(
                    format!("\x1b[<{};{};{}M", button, event.x + 1, event.y + 1).as_bytes(),
                )?;
            } else if event.button == MouseButton::Middle {
                let clip = host.get_clipboard()?.get_contents()?;
                self.send_paste(&clip, host.writer())?
            }
        }

        Ok(())
    }

    fn mouse_button_release(
        &mut self,
        event: MouseEvent,
        writer: &mut dyn std::io::Write,
    ) -> anyhow::Result<()> {
        if self.current_mouse_button != MouseButton::None {
            self.current_mouse_button = MouseButton::None;
            if self.sgr_mouse {
                write!(writer, "\x1b[<3;{};{}m", event.x + 1, event.y + 1)?;
            }
        }

        Ok(())
    }

    fn mouse_move(
        &mut self,
        event: MouseEvent,
        writer: &mut dyn std::io::Write,
    ) -> anyhow::Result<()> {
        if let Some(button) = match (self.current_mouse_button, self.button_event_mouse) {
            (MouseButton::Left, true) => Some(32),
            (MouseButton::Middle, true) => Some(33),
            (MouseButton::Right, true) => Some(34),
            (..) => None,
        } {
            if self.sgr_mouse {
                write!(writer, "\x1b[<{};{};{}M", button, event.x + 1, event.y + 1)?;
            }
        }
        Ok(())
    }

    pub fn mouse_event(
        &mut self,
        mut event: MouseEvent,
        host: &mut dyn TerminalHost,
    ) -> anyhow::Result<()> {
        event.y = event.y.min(self.screen().physical_rows as i64 - 1);
        event.x = event.x.min(self.screen().physical_cols - 1);

        let new_position = CursorPosition { x: event.x, y: event.y as VisibleRowIndex };

        if new_position != self.mouse_position {
            self.mouse_position = new_position;
            self.recompute_highlight();
        }

        let send_event = self.sgr_mouse && !event.modifiers.contains(KeyModifiers::SHIFT);

        if event.kind == MouseEventKind::Press {
            let click = match self.last_mouse_click.take() {
                None => LastMouseClick::new(event.button),
                Some(click) => click.add(event.button),
            };
            self.last_mouse_click = Some(click);
        }

        if !send_event {
            match (event, self.current_mouse_button) {
                (MouseEvent { kind: MouseEventKind::Press, button: MouseButton::Left, .. }, _) => {
                    return self.mouse_press_left(event, host);
                }
                (
                    MouseEvent { kind: MouseEventKind::Release, button: MouseButton::Left, .. },
                    _,
                ) => {
                    return self.mouse_release_left(event, host);
                }
                (MouseEvent { kind: MouseEventKind::Move, .. }, MouseButton::Left) => {
                    return self.mouse_drag_left(event);
                }
                _ => {}
            }
        }

        match event {
            MouseEvent { kind: MouseEventKind::Press, button: MouseButton::WheelUp(_), .. }
            | MouseEvent {
                kind: MouseEventKind::Press, button: MouseButton::WheelDown(_), ..
            } => self.mouse_wheel(event, host.writer()),
            MouseEvent { kind: MouseEventKind::Press, .. } => self.mouse_button_press(event, host),
            MouseEvent { kind: MouseEventKind::Release, .. } => {
                self.mouse_button_release(event, host.writer())
            }
            MouseEvent { kind: MouseEventKind::Move, .. } => self.mouse_move(event, host.writer()),
        }
    }

    pub fn send_paste(
        &mut self,
        text: &str,
        writer: &mut dyn std::io::Write,
    ) -> anyhow::Result<()> {
        if self.bracketed_paste {
            let buf = format!("\x1b[200~{}\x1b[201~", text);
            writer.write_all(buf.as_bytes())?;
        } else {
            writer.write_all(text.as_bytes())?;
        }
        Ok(())
    }

    pub fn key_down(
        &mut self,
        key: KeyCode,
        mods: KeyModifiers,
        writer: &mut dyn std::io::Write,
    ) -> anyhow::Result<()> {
        const CTRL: KeyModifiers = KeyModifiers::CTRL;
        const SHIFT: KeyModifiers = KeyModifiers::SHIFT;
        const ALT: KeyModifiers = KeyModifiers::ALT;
        const NO: KeyModifiers = KeyModifiers::NONE;
        const APPCURSOR: bool = true;
        use crate::core::input::KeyCode::*;

        let ctrl = mods & CTRL;
        let shift = mods & SHIFT;
        let alt = mods & ALT;

        let mut buf = String::new();

        let to_send = match (key, ctrl, alt, shift, self.application_cursor_keys) {
            (Char(c), _, ALT, ..) if c.is_ascii_alphanumeric() || c.is_ascii_punctuation() => {
                buf.push(0x1b as char);
                buf.push(c);
                buf.as_str()
            }
            (Backspace, _, ALT, ..) => "\x1b\x08",
            (UpArrow, _, ALT, ..) => "\x1b\x1b[A",
            (DownArrow, _, ALT, ..) => "\x1b\x1b[B",
            (RightArrow, _, ALT, ..) => "\x1b\x1b[C",
            (LeftArrow, _, ALT, ..) => "\x1b\x1b[D",

            (Tab, ..) => "\t",
            (Enter, ..) => "\r",
            (Backspace, ..) => "\x08",
            (Escape, ..) => "\x1b",

            (Char('\x7f'), _, _, _, false) | (Delete, _, _, _, false) => "\x7f",
            (Char('\x7f'), _, _, _, true) | (Delete, _, _, _, true) => "\x7f",

            (Char(c), CTRL, _, SHIFT, _) if c <= 0xff as char && c > 0x40 as char => {
                buf.push((c as u8 - 0x40) as char);
                buf.as_str()
            }
            (Char(c), CTRL, ..) if c <= 0xff as char && c > 0x60 as char => {
                buf.push((c as u8 - 0x60) as char);
                buf.as_str()
            }
            (Char(c), ..) => {
                buf.push(c);
                buf.as_str()
            }

            (UpArrow, _, _, _, APPCURSOR) => "\x1bOA",
            (DownArrow, _, _, _, APPCURSOR) => "\x1bOB",
            (RightArrow, _, _, _, APPCURSOR) => "\x1bOC",
            (LeftArrow, _, _, _, APPCURSOR) => "\x1bOD",
            (Home, _, _, _, APPCURSOR) => "\x1bOH",
            (End, _, _, _, APPCURSOR) => "\x1bOF",
            (ApplicationUpArrow, ..) => "\x1bOA",
            (ApplicationDownArrow, ..) => "\x1bOB",
            (ApplicationRightArrow, ..) => "\x1bOC",
            (ApplicationLeftArrow, ..) => "\x1bOD",

            (UpArrow, ..) => "\x1b[A",
            (DownArrow, ..) => "\x1b[B",
            (RightArrow, ..) => "\x1b[C",
            (LeftArrow, ..) => "\x1b[D",
            (PageUp, _, _, SHIFT, _) => {
                let rows = self.screen().physical_rows as i64;
                self.scroll_viewport(-rows);
                ""
            }
            (PageDown, _, _, SHIFT, _) => {
                let rows = self.screen().physical_rows as i64;
                self.scroll_viewport(rows);
                ""
            }
            (PageUp, ..) => "\x1b[5~",
            (PageDown, ..) => "\x1b[6~",
            (Home, ..) => "\x1b[H",
            (End, ..) => "\x1b[F",
            (Insert, ..) => "\x1b[2~",

            (Function(n), ..) => {
                let modifier = match (ctrl, alt, shift) {
                    (NO, NO, NO) => "",
                    (NO, NO, SHIFT) => ";2",
                    (NO, ALT, NO) => ";3",
                    (NO, ALT, SHIFT) => ";4",
                    (CTRL, NO, NO) => ";5",
                    (CTRL, NO, SHIFT) => ";6",
                    (CTRL, ALT, NO) => ";7",
                    (CTRL, ALT, SHIFT) => ";8",
                    _ => unreachable!("invalid modifiers!?"),
                };

                if modifier.is_empty() && n < 5 {
                    match n {
                        1 => "\x1bOP",
                        2 => "\x1bOQ",
                        3 => "\x1bOR",
                        4 => "\x1bOS",
                        _ => unreachable!("wat?"),
                    }
                } else {
                    let intro = match n {
                        1 => "\x1b[11",
                        2 => "\x1b[12",
                        3 => "\x1b[13",
                        4 => "\x1b[14",
                        5 => "\x1b[15",
                        6 => "\x1b[17",
                        7 => "\x1b[18",
                        8 => "\x1b[19",
                        9 => "\x1b[20",
                        10 => "\x1b[21",
                        11 => "\x1b[23",
                        12 => "\x1b[24",
                        _ => bail!("unhandled fkey number {}", n),
                    };
                    write!(buf, "{}{}~", intro, modifier)?;
                    buf.as_str()
                }
            }

            (Numpad0, ..)
            | (Numpad1, ..)
            | (Numpad2, ..)
            | (Numpad3, ..)
            | (Numpad4, ..)
            | (Numpad5, ..)
            | (Numpad6, ..)
            | (Numpad7, ..)
            | (Numpad8, ..)
            | (Numpad9, ..)
            | (Multiply, ..)
            | (Add, ..)
            | (Separator, ..)
            | (Subtract, ..)
            | (Decimal, ..)
            | (Divide, ..) => "",

            (Control, ..)
            | (LeftControl, ..)
            | (RightControl, ..)
            | (Alt, ..)
            | (LeftAlt, ..)
            | (RightAlt, ..)
            | (Menu, ..)
            | (LeftMenu, ..)
            | (RightMenu, ..)
            | (Super, ..)
            | (Hyper, ..)
            | (Shift, ..)
            | (LeftShift, ..)
            | (RightShift, ..)
            | (Meta, ..)
            | (LeftWindows, ..)
            | (RightWindows, ..)
            | (NumLock, ..)
            | (ScrollLock, ..) => "",

            (Cancel, ..)
            | (Clear, ..)
            | (Pause, ..)
            | (CapsLock, ..)
            | (Select, ..)
            | (Print, ..)
            | (PrintScreen, ..)
            | (Execute, ..)
            | (Help, ..)
            | (Applications, ..)
            | (Sleep, ..)
            | (BrowserBack, ..)
            | (BrowserForward, ..)
            | (BrowserRefresh, ..)
            | (BrowserStop, ..)
            | (BrowserSearch, ..)
            | (BrowserFavorites, ..)
            | (BrowserHome, ..)
            | (VolumeMute, ..)
            | (VolumeDown, ..)
            | (VolumeUp, ..)
            | (MediaNextTrack, ..)
            | (MediaPrevTrack, ..)
            | (MediaStop, ..)
            | (MediaPlayPause, ..)
            | (InternalPasteStart, ..)
            | (InternalPasteEnd, ..) => "",
        };

        writer.write_all(to_send.as_bytes())?;

        if !to_send.is_empty() && self.viewport_offset != 0 {
            self.set_scroll_viewport(0);
        }

        Ok(())
    }

    pub fn resize(
        &mut self,
        physical_rows: usize,
        physical_cols: usize,
        pixel_width: usize,
        pixel_height: usize,
    ) {
        self.screen.resize(physical_rows, physical_cols);
        self.scroll_region = 0..physical_rows as i64;
        self.pixel_height = pixel_height;
        self.pixel_width = pixel_width;
        self.tabs.resize(physical_cols);
        self.set_scroll_viewport(0);

        self.set_cursor_pos(&Position::Relative(0), &Position::Relative(0));
    }

    pub fn get_dirty_lines(&self) -> Vec<(usize, &Line, Range<usize>)> {
        let mut res = Vec::new();

        let screen = self.screen();
        let height = screen.physical_rows;
        let len = screen.lines.len() - self.viewport_offset as usize;

        let selection = self.selection_range.map(|r| r.normalize());

        for (i, line) in screen.lines.iter().skip(len - height).enumerate() {
            if i >= height {
                break;
            }
            if line.is_dirty() {
                let selrange = match selection {
                    None => 0..0,
                    Some(sel) => {
                        let row = (i as ScrollbackOrVisibleRowIndex)
                            - self.viewport_offset as ScrollbackOrVisibleRowIndex;
                        sel.cols_for_row(row)
                    }
                };
                res.push((i, &*line, selrange));
            }
        }

        res
    }

    pub fn clean_dirty_lines(&mut self) {
        let screen = self.screen_mut();
        for line in &mut screen.lines {
            line.clear_dirty();
        }
    }

    pub fn make_all_lines_dirty(&mut self) {
        let screen = self.screen_mut();
        for line in &mut screen.lines {
            line.set_dirty();
        }
    }

    pub fn physical_dimensions(&self) -> (usize, usize) {
        let screen = self.screen();
        (screen.physical_rows, screen.physical_cols)
    }

    pub fn cursor_pos(&self) -> CursorPosition {
        CursorPosition { x: self.cursor.x, y: self.cursor.y + self.viewport_offset }
    }

    pub fn current_highlight(&self) -> Option<Arc<Hyperlink>> {
        self.current_highlight.as_ref().cloned()
    }

    fn set_cursor_pos(&mut self, x: &Position, y: &Position) {
        let x = match *x {
            Position::Relative(x) => (self.cursor.x as i64 + x).max(0),
            Position::Absolute(x) => x,
        };
        let y = match *y {
            Position::Relative(y) => (self.cursor.y + y).max(0),
            Position::Absolute(y) => y,
        };

        let rows = self.screen().physical_rows;
        let cols = self.screen().physical_cols;
        let old_y = self.cursor.y;
        let new_y = y.min(rows as i64 - 1);

        self.cursor.x = x.min(cols as i64 - 1) as usize;
        self.cursor.y = new_y;
        self.wrap_next = false;

        let screen = self.screen_mut();
        screen.dirty_line(old_y);
        screen.dirty_line(new_y);
    }

    fn set_scroll_viewport(&mut self, position: VisibleRowIndex) {
        self.clear_selection();
        let position = position.max(0);

        let rows = self.screen().physical_rows;
        let avail_scrollback = self.screen().lines.len() - rows;

        let position = position.min(avail_scrollback as i64);

        self.viewport_offset = position;
        let top = self.screen().lines.len() - (rows + position as usize);
        {
            let screen = self.screen_mut();
            for y in top..top + rows {
                screen.line_mut(y).set_dirty();
            }
        }
        self.recompute_highlight();
    }

    pub fn scroll_viewport(&mut self, delta: VisibleRowIndex) {
        let position = self.viewport_offset - delta;
        self.set_scroll_viewport(position);
    }

    fn scroll_up(&mut self, num_rows: usize) {
        self.clear_selection();
        let scroll_region = self.scroll_region.clone();
        self.screen_mut().scroll_up(&scroll_region, num_rows)
    }

    fn scroll_down(&mut self, num_rows: usize) {
        self.clear_selection();
        let scroll_region = self.scroll_region.clone();
        self.screen_mut().scroll_down(&scroll_region, num_rows)
    }

    fn new_line(&mut self, move_to_first_column: bool) {
        let x = if move_to_first_column { 0 } else { self.cursor.x };
        let y = self.cursor.y;
        let y = if y == self.scroll_region.end - 1 {
            self.scroll_up(1);
            y
        } else {
            y + 1
        };
        self.set_cursor_pos(&Position::Absolute(x as i64), &Position::Absolute(y as i64));
    }

    fn c1_index(&mut self) {
        let y = self.cursor.y;
        let y = if y == self.scroll_region.end - 1 {
            self.scroll_up(1);
            y
        } else {
            y + 1
        };
        self.set_cursor_pos(&Position::Relative(0), &Position::Absolute(y as i64));
    }

    fn c1_nel(&mut self) {
        self.new_line(true);
    }

    fn c1_hts(&mut self) {
        self.tabs.set_tab_stop(self.cursor.x);
    }

    fn c0_horizontal_tab(&mut self) {
        let x = match self.tabs.find_next_tab_stop(self.cursor.x) {
            Some(x) => x,
            None => self.screen().physical_cols - 1,
        };
        self.set_cursor_pos(&Position::Absolute(x as i64), &Position::Relative(0));
    }

    fn c1_reverse_index(&mut self) {
        let y = self.cursor.y;
        let y = if y == self.scroll_region.start {
            self.scroll_down(1);
            y
        } else {
            y - 1
        };
        self.set_cursor_pos(&Position::Relative(0), &Position::Absolute(y as i64));
    }

    fn set_hyperlink(&mut self, link: Option<Hyperlink>) {
        self.pen.hyperlink = match link {
            Some(hyperlink) => Some(Arc::new(hyperlink)),
            None => None,
        }
    }

    fn perform_device(&mut self, dev: Device, host: &mut dyn TerminalHost) {
        match dev {
            Device::DeviceAttributes(a) => error!("unhandled: {:?}", a),
            Device::SoftReset => {
                self.pen = CellAttributes::default();
            }
            Device::RequestPrimaryDeviceAttributes => {
                host.writer().write(DEVICE_IDENT).ok();
            }
            Device::RequestSecondaryDeviceAttributes => {
                host.writer().write(b"\x1b[>0;0;0c").ok();
            }
            Device::StatusReport => {
                host.writer().write(b"\x1b[0n").ok();
            }
        }
    }

    fn perform_csi_mode(&mut self, mode: Mode) {
        match mode {
            Mode::SetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::StartBlinkingCursor,
            ))
            | Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::StartBlinkingCursor,
            )) => {}

            Mode::SetMode(TerminalMode::Code(TerminalModeCode::Insert)) => {
                self.insert = true;
            }
            Mode::ResetMode(TerminalMode::Code(TerminalModeCode::Insert)) => {
                self.insert = false;
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::BracketedPaste)) => {
                self.bracketed_paste = true;
            }
            Mode::ResetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::BracketedPaste)) => {
                self.bracketed_paste = false;
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::EnableAlternateScreen,
            )) => {
                if !self.screen.is_alt_screen_active() {
                    self.screen.activate_alt_screen();
                    self.set_scroll_viewport(0);
                }
            }
            Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::EnableAlternateScreen,
            )) => {
                if self.screen.is_alt_screen_active() {
                    self.screen.activate_primary_screen();
                    self.set_scroll_viewport(0);
                }
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::ApplicationCursorKeys,
            )) => {
                self.application_cursor_keys = true;
            }
            Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::ApplicationCursorKeys,
            )) => {
                self.application_cursor_keys = false;
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::ShowCursor)) => {
                self.cursor_visible = true;
            }
            Mode::ResetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::ShowCursor)) => {
                self.cursor_visible = false;
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::MouseTracking))
            | Mode::ResetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::MouseTracking)) => {
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::HighlightMouseTracking,
            ))
            | Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::HighlightMouseTracking,
            )) => {}

            Mode::SetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::ButtonEventMouse)) => {
                self.button_event_mouse = true;
            }
            Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::ButtonEventMouse,
            )) => {
                self.button_event_mouse = false;
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::AnyEventMouse))
            | Mode::ResetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::AnyEventMouse)) => {
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::SGRMouse)) => {
                self.sgr_mouse = true;
            }
            Mode::ResetDecPrivateMode(DecPrivateMode::Code(DecPrivateModeCode::SGRMouse)) => {
                self.sgr_mouse = false;
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::ClearAndEnableAlternateScreen,
            )) => {
                if !self.screen.is_alt_screen_active() {
                    self.save_cursor();
                    self.screen.activate_alt_screen();
                    self.set_cursor_pos(&Position::Absolute(0), &Position::Absolute(0));
                    self.erase_in_display(EraseInDisplay::EraseDisplay);
                    self.set_scroll_viewport(0);
                }
            }
            Mode::ResetDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::ClearAndEnableAlternateScreen,
            )) => {
                if self.screen.is_alt_screen_active() {
                    self.screen.activate_primary_screen();
                    self.restore_cursor();
                    self.set_scroll_viewport(0);
                }
            }
            Mode::SaveDecPrivateMode(DecPrivateMode::Code(_))
            | Mode::RestoreDecPrivateMode(DecPrivateMode::Code(_)) => {
                error!("save/restore dec mode unimplemented")
            }

            Mode::SetDecPrivateMode(DecPrivateMode::Unspecified(n))
            | Mode::ResetDecPrivateMode(DecPrivateMode::Unspecified(n))
            | Mode::SaveDecPrivateMode(DecPrivateMode::Unspecified(n))
            | Mode::RestoreDecPrivateMode(DecPrivateMode::Unspecified(n)) => {
                error!("unhandled DecPrivateMode {}", n);
            }

            Mode::SetMode(TerminalMode::Unspecified(n))
            | Mode::ResetMode(TerminalMode::Unspecified(n)) => {
                error!("unhandled TerminalMode {}", n);
            }

            Mode::SetMode(m) | Mode::ResetMode(m) => {
                error!("unhandled TerminalMode {:?}", m);
            }
        }
    }

    fn checksum_rectangle(&mut self, left: u32, top: u32, right: u32, bottom: u32) -> u16 {
        let screen = self.screen_mut();
        let mut checksum = 0;
        debug!("checksum left={} top={} right={} bottom={}", left, top, right, bottom);
        for y in top..=bottom {
            let line_idx = screen.phys_row(VisibleRowIndex::from(y));
            let line = screen.line_mut(line_idx);
            for (col, cell) in line.cells().iter().enumerate().skip(left as usize) {
                if col > right as usize {
                    break;
                }

                let ch = cell.str().chars().nth(0).unwrap() as u32;
                debug!("y={} col={} ch={:x} cell={:?}", y, col, ch, cell);

                checksum += u16::from(ch as u8);
            }
        }
        checksum
    }

    fn perform_csi_window(&mut self, window: Window, host: &mut dyn TerminalHost) {
        match window {
            Window::ReportTextAreaSizeCells => {
                let screen = self.screen();
                let height = Some(screen.physical_rows as i64);
                let width = Some(screen.physical_cols as i64);

                let response = Window::ResizeWindowCells { width, height };
                write!(host.writer(), "{}", CSI::Window(response)).ok();
            }
            Window::ChecksumRectangularArea { request_id, top, left, bottom, right, .. } => {
                let checksum = self.checksum_rectangle(
                    left.as_zero_based(),
                    top.as_zero_based(),
                    right.as_zero_based(),
                    bottom.as_zero_based(),
                );
                write!(host.writer(), "\x1bP{}!~{:04x}\x1b\\", request_id, checksum).ok();
            }
            Window::Iconify | Window::DeIconify => {}
            Window::PopIconAndWindowTitle
            | Window::PopWindowTitle
            | Window::PopIconTitle
            | Window::PushIconAndWindowTitle
            | Window::PushIconTitle
            | Window::PushWindowTitle => {}
            _ => error!("unhandled Window CSI {:?}", window),
        }
    }

    fn erase_in_display(&mut self, erase: EraseInDisplay) {
        let cy = self.cursor.y;
        let pen = self.pen.clone_sgr_only();
        let rows = self.screen().physical_rows as VisibleRowIndex;
        let col_range = 0..usize::max_value();
        let row_range = match erase {
            EraseInDisplay::EraseToEndOfDisplay => {
                self.perform_csi_edit(Edit::EraseInLine(EraseInLine::EraseToEndOfLine));
                cy + 1..rows
            }
            EraseInDisplay::EraseToStartOfDisplay => {
                self.perform_csi_edit(Edit::EraseInLine(EraseInLine::EraseToStartOfLine));
                0..cy
            }
            EraseInDisplay::EraseDisplay => 0..rows,
            EraseInDisplay::EraseScrollback => {
                error!("TODO: ed: no support for xterm Erase Saved Lines yet");
                return;
            }
        };

        {
            let screen = self.screen_mut();
            for y in row_range.clone() {
                screen.clear_line(y, col_range.clone(), &pen);
            }
        }

        for y in row_range {
            if self
                .clear_selection_if_intersects(col_range.clone(), y as ScrollbackOrVisibleRowIndex)
            {
                break;
            }
        }
    }

    fn perform_csi_edit(&mut self, edit: Edit) {
        match edit {
            Edit::DeleteCharacter(n) => {
                let y = self.cursor.y;
                let x = self.cursor.x;
                let limit = (x + n as usize).min(self.screen().physical_cols);
                {
                    let screen = self.screen_mut();
                    for _ in x..limit as usize {
                        screen.erase_cell(x, y);
                    }
                }
                self.clear_selection_if_intersects(x..limit, y as ScrollbackOrVisibleRowIndex);
            }
            Edit::DeleteLine(n) => {
                if self.scroll_region.contains(&self.cursor.y) {
                    let scroll_region = self.cursor.y..self.scroll_region.end;
                    self.screen_mut().scroll_up(&scroll_region, n as usize);

                    let scrollback_region = self.cursor.y as ScrollbackOrVisibleRowIndex
                        ..self.scroll_region.end as ScrollbackOrVisibleRowIndex;
                    self.clear_selection_if_intersects_rows(scrollback_region);
                }
            }
            Edit::EraseCharacter(n) => {
                let y = self.cursor.y;
                let x = self.cursor.x;
                let limit = (x + n as usize).min(self.screen().physical_cols);
                {
                    let blank = Cell::new(' ', self.pen.clone_sgr_only());
                    let screen = self.screen_mut();
                    for x in x..limit as usize {
                        screen.set_cell(x, y, &blank);
                    }
                }
                self.clear_selection_if_intersects(x..limit, y as ScrollbackOrVisibleRowIndex);
            }

            Edit::EraseInLine(erase) => {
                let cx = self.cursor.x;
                let cy = self.cursor.y;
                let pen = self.pen.clone_sgr_only();
                let cols = self.screen().physical_cols;
                let range = match erase {
                    EraseInLine::EraseToEndOfLine => cx..cols,
                    EraseInLine::EraseToStartOfLine => 0..cx,
                    EraseInLine::EraseLine => 0..cols,
                };

                self.screen_mut().clear_line(cy, range.clone(), &pen);
                self.clear_selection_if_intersects(range, cy as ScrollbackOrVisibleRowIndex);
            }
            Edit::InsertCharacter(n) => {
                let y = self.cursor.y;
                let x = self.cursor.x;

                let limit = (x + n as usize).min(self.screen().physical_cols);
                {
                    let screen = self.screen_mut();
                    for x in x..limit as usize {
                        screen.insert_cell(x, y);
                    }
                }
                self.clear_selection_if_intersects(x..limit, y as ScrollbackOrVisibleRowIndex);
            }
            Edit::InsertLine(n) => {
                if self.scroll_region.contains(&self.cursor.y) {
                    let scroll_region = self.cursor.y..self.scroll_region.end;
                    self.screen_mut().scroll_down(&scroll_region, n as usize);

                    let scrollback_region = self.cursor.y as ScrollbackOrVisibleRowIndex
                        ..self.scroll_region.end as ScrollbackOrVisibleRowIndex;
                    self.clear_selection_if_intersects_rows(scrollback_region);
                }
            }
            Edit::ScrollDown(n) => self.scroll_down(n as usize),
            Edit::ScrollUp(n) => self.scroll_up(n as usize),
            Edit::EraseInDisplay(erase) => self.erase_in_display(erase),
            Edit::Repeat(n) => {
                let y = self.cursor.y;
                let x = self.cursor.x;
                let to_copy = x.saturating_sub(1);
                let screen = self.screen_mut();
                let line_idx = screen.phys_row(y);
                let line = screen.line_mut(line_idx);
                if let Some(cell) = line.cells().get(to_copy).cloned() {
                    line.fill_range(x..=x + n as usize, &cell);
                    self.set_cursor_pos(&Position::Relative(i64::from(n)), &Position::Relative(0))
                }
            }
        }
    }

    fn perform_csi_cursor(&mut self, cursor: Cursor, host: &mut dyn TerminalHost) {
        match cursor {
            Cursor::SetTopAndBottomMargins { top, bottom } => {
                let rows = self.screen().physical_rows;
                let mut top = i64::from(top.as_zero_based()).min(rows as i64 - 1).max(0);
                let mut bottom = i64::from(bottom.as_zero_based()).min(rows as i64 - 1).max(0);
                if top > bottom {
                    std::mem::swap(&mut top, &mut bottom);
                }
                self.scroll_region = top..bottom + 1;
            }
            Cursor::ForwardTabulation(n) => {
                for _ in 0..n {
                    self.c0_horizontal_tab();
                }
            }
            Cursor::BackwardTabulation(_) => {}
            Cursor::TabulationClear(_) => {}
            Cursor::TabulationControl(_) => {}
            Cursor::LineTabulation(_) => {}

            Cursor::Left(n) => {
                self.set_cursor_pos(&Position::Relative(-(i64::from(n))), &Position::Relative(0))
            }
            Cursor::Right(n) => {
                self.set_cursor_pos(&Position::Relative(i64::from(n)), &Position::Relative(0))
            }
            Cursor::Up(n) => {
                self.set_cursor_pos(&Position::Relative(0), &Position::Relative(-(i64::from(n))))
            }
            Cursor::Down(n) => {
                self.set_cursor_pos(&Position::Relative(0), &Position::Relative(i64::from(n)))
            }
            Cursor::CharacterAndLinePosition { line, col } | Cursor::Position { line, col } => self
                .set_cursor_pos(
                    &Position::Absolute(i64::from(col.as_zero_based())),
                    &Position::Absolute(i64::from(line.as_zero_based())),
                ),
            Cursor::CharacterAbsolute(col) | Cursor::CharacterPositionAbsolute(col) => self
                .set_cursor_pos(
                    &Position::Absolute(i64::from(col.as_zero_based())),
                    &Position::Relative(0),
                ),
            Cursor::CharacterPositionBackward(col) => {
                self.set_cursor_pos(&Position::Relative(-(i64::from(col))), &Position::Relative(0))
            }
            Cursor::CharacterPositionForward(col) => {
                self.set_cursor_pos(&Position::Relative(i64::from(col)), &Position::Relative(0))
            }
            Cursor::LinePositionAbsolute(line) => self.set_cursor_pos(
                &Position::Relative(0),
                &Position::Absolute((i64::from(line)).saturating_sub(1)),
            ),
            Cursor::LinePositionBackward(line) => {
                self.set_cursor_pos(&Position::Relative(0), &Position::Relative(-(i64::from(line))))
            }
            Cursor::LinePositionForward(line) => {
                self.set_cursor_pos(&Position::Relative(0), &Position::Relative(i64::from(line)))
            }
            Cursor::NextLine(n) => {
                for _ in 0..n {
                    self.new_line(true);
                }
            }
            Cursor::PrecedingLine(n) => {
                self.set_cursor_pos(&Position::Absolute(0), &Position::Relative(-(i64::from(n))))
            }
            Cursor::ActivePositionReport { .. } => {}
            Cursor::RequestActivePositionReport => {
                let line = OneBased::from_zero_based(self.cursor.y as u32);
                let col = OneBased::from_zero_based(self.cursor.x as u32);
                let report = CSI::Cursor(Cursor::ActivePositionReport { line, col });
                write!(host.writer(), "{}", report).ok();
            }
            Cursor::SaveCursor => self.save_cursor(),
            Cursor::RestoreCursor => self.restore_cursor(),
            Cursor::CursorStyle(style) => error!("unhandled: CursorStyle {:?}", style),
        }
    }

    fn save_cursor(&mut self) {
        let saved =
            SavedCursor { position: self.cursor, insert: self.insert, wrap_next: self.wrap_next };
        debug!("saving cursor {:?} is_alt={}", saved, self.screen.is_alt_screen_active());
        *self.screen.saved_cursor() = Some(saved);
    }
    fn restore_cursor(&mut self) {
        let saved = self.screen.saved_cursor().unwrap_or_else(|| SavedCursor {
            position: CursorPosition::default(),
            insert: false,
            wrap_next: false,
        });
        debug!("restore cursor {:?} is_alt={}", saved, self.screen.is_alt_screen_active());
        let x = saved.position.x;
        let y = saved.position.y;
        self.set_cursor_pos(&Position::Absolute(x as i64), &Position::Absolute(y));
        self.wrap_next = saved.wrap_next;
        self.insert = saved.insert;
    }

    fn perform_csi_sgr(&mut self, sgr: Sgr) {
        debug!("{:?}", sgr);
        match sgr {
            Sgr::Reset => {
                let link = self.pen.hyperlink.take();
                self.pen = CellAttributes::default();
                self.pen.hyperlink = link;
            }
            Sgr::Intensity(intensity) => {
                self.pen.set_intensity(intensity);
            }
            Sgr::Underline(underline) => {
                self.pen.set_underline(underline);
            }
            Sgr::Blink(blink) => {
                self.pen.set_blink(blink);
            }
            Sgr::Italic(italic) => {
                self.pen.set_italic(italic);
            }
            Sgr::Inverse(inverse) => {
                self.pen.set_reverse(inverse);
            }
            Sgr::Invisible(invis) => {
                self.pen.set_invisible(invis);
            }
            Sgr::StrikeThrough(strike) => {
                self.pen.set_strikethrough(strike);
            }
            Sgr::Foreground(col) => {
                self.pen.set_foreground(col);
            }
            Sgr::Background(col) => {
                self.pen.set_background(col);
            }
            Sgr::Font(_) => {}
        }
    }
}

pub(crate) struct Performer<'a> {
    pub state: &'a mut TerminalState,
    pub host: &'a mut dyn TerminalHost,
    print: Option<String>,
}

impl<'a> Deref for Performer<'a> {
    type Target = TerminalState;

    fn deref(&self) -> &TerminalState {
        self.state
    }
}

impl<'a> DerefMut for Performer<'a> {
    fn deref_mut(&mut self) -> &mut TerminalState {
        &mut self.state
    }
}

impl<'a> Drop for Performer<'a> {
    fn drop(&mut self) {
        self.flush_print();
    }
}

impl<'a> Performer<'a> {
    pub fn new(state: &'a mut TerminalState, host: &'a mut dyn TerminalHost) -> Self {
        Self { state, host, print: None }
    }

    fn flush_print(&mut self) {
        let p = match self.print.take() {
            Some(s) => s,
            None => return,
        };

        let mut x_offset = 0;

        for g in unicode_segmentation::UnicodeSegmentation::graphemes(p.as_str(), true) {
            let g = if self.dec_line_drawing_mode {
                match g {
                    "j" => "┘",
                    "k" => "┐",
                    "l" => "┌",
                    "m" => "└",
                    "n" => "┼",
                    "q" => "─",
                    "t" => "├",
                    "u" => "┤",
                    "v" => "┴",
                    "w" => "┬",
                    "x" => "│",
                    _ => g,
                }
            } else {
                g
            };

            if !self.insert && self.wrap_next {
                self.new_line(true);
            }

            let x = self.cursor.x;
            let y = self.cursor.y;
            let width = self.screen().physical_cols;

            let mut pen = self.pen.clone();

            let print_width = unicode_column_width(g).max(1);

            if !self.insert && x + print_width >= width {
                pen.set_wrapped(true);
            }

            let cell = Cell::new_grapheme(g, pen);

            if self.insert {
                let screen = self.screen_mut();
                for _ in x..x + print_width as usize {
                    screen.insert_cell(x + x_offset, y);
                }
            }

            self.screen_mut().set_cell(x + x_offset, y, &cell);

            self.clear_selection_if_intersects(
                x..x + print_width,
                y as ScrollbackOrVisibleRowIndex,
            );

            if self.insert {
                x_offset += print_width;
            } else if x + print_width < width {
                self.cursor.x += print_width;
                self.wrap_next = false;
            } else {
                self.wrap_next = true;
            }
        }
    }

    pub fn perform(&mut self, action: Action) {
        debug!("perform {:?}", action);
        match action {
            Action::Print(c) => self.print(c),
            Action::Control(code) => self.control(code),
            Action::DeviceControl(ctrl) => error!("Unhandled {:?}", ctrl),
            Action::OperatingSystemCommand(osc) => self.osc_dispatch(*osc),
            Action::Esc(esc) => self.esc_dispatch(esc),
            Action::CSI(csi) => self.csi_dispatch(csi),
        }
    }

    fn print(&mut self, c: char) {
        self.print.get_or_insert_with(String::new).push(c);
    }

    fn control(&mut self, control: ControlCode) {
        self.flush_print();
        match control {
            ControlCode::LineFeed | ControlCode::VerticalTab | ControlCode::FormFeed => {
                self.new_line(false)
            }
            ControlCode::CarriageReturn => {
                self.set_cursor_pos(&Position::Absolute(0), &Position::Relative(0));
            }
            ControlCode::Backspace => {
                self.set_cursor_pos(&Position::Relative(-1), &Position::Relative(0));
            }
            ControlCode::HorizontalTab => self.c0_horizontal_tab(),
            ControlCode::Bell => error!("Ding! (this is the bell)"),
            _ => error!("unhandled ControlCode {:?}", control),
        }
    }

    fn csi_dispatch(&mut self, csi: CSI) {
        self.flush_print();
        match csi {
            CSI::Sgr(sgr) => self.state.perform_csi_sgr(sgr),
            CSI::Cursor(cursor) => self.state.perform_csi_cursor(cursor, self.host),
            CSI::Edit(edit) => self.state.perform_csi_edit(edit),
            CSI::Mode(mode) => self.state.perform_csi_mode(mode),
            CSI::Device(dev) => self.state.perform_device(*dev, self.host),
            CSI::Mouse(mouse) => error!("mouse report sent by app? {:?}", mouse),
            CSI::Window(window) => self.state.perform_csi_window(window, self.host),
            CSI::Unspecified(unspec) => {
                error!("unknown unspecified CSI: {:?}", format!("{}", unspec))
            }
        };
    }

    fn esc_dispatch(&mut self, esc: Esc) {
        self.flush_print();
        match esc {
            Esc::Code(EscCode::StringTerminator) => {}
            Esc::Code(EscCode::DecApplicationKeyPad) => {
                debug!("DECKPAM on");
                self.application_keypad = true;
            }
            Esc::Code(EscCode::DecNormalKeyPad) => {
                debug!("DECKPAM off");
                self.application_keypad = false;
            }
            Esc::Code(EscCode::ReverseIndex) => self.c1_reverse_index(),
            Esc::Code(EscCode::Index) => self.c1_index(),
            Esc::Code(EscCode::NextLine) => self.c1_nel(),
            Esc::Code(EscCode::HorizontalTabSet) => self.c1_hts(),
            Esc::Code(EscCode::DecLineDrawing) => {
                self.dec_line_drawing_mode = true;
            }
            Esc::Code(EscCode::AsciiCharacterSet) => {
                self.dec_line_drawing_mode = false;
            }
            Esc::Code(EscCode::DecSaveCursorPosition) => self.save_cursor(),
            Esc::Code(EscCode::DecRestoreCursorPosition) => self.restore_cursor(),
            _ => error!("ESC: unhandled {:?}", esc),
        }
    }

    fn osc_dispatch(&mut self, osc: OperatingSystemCommand) {
        self.flush_print();
        match osc {
            OperatingSystemCommand::SetIconNameAndWindowTitle(title)
            | OperatingSystemCommand::SetWindowTitle(title) => {
                self.title = title.clone();
                self.host.set_title(&title);
            }
            OperatingSystemCommand::SetIconName(_) => {}
            OperatingSystemCommand::SetHyperlink(link) => {
                self.set_hyperlink(link);
            }
            OperatingSystemCommand::Unspecified(unspec) => {
                let mut output = String::new();
                write!(&mut output, "Unhandled OSC ").ok();
                for item in unspec {
                    write!(&mut output, " {}", String::from_utf8_lossy(&item)).ok();
                }
                error!("{}", output);
            }

            OperatingSystemCommand::ClearSelection(_) => {
                if let Ok(clip) = self.host.get_clipboard() {
                    clip.set_contents(None).ok();
                }
            }
            OperatingSystemCommand::QuerySelection(_) => {}
            OperatingSystemCommand::SetSelection(_, selection_data) => {
                if let Ok(clip) = self.host.get_clipboard() {
                    match clip.set_contents(Some(selection_data)) {
                        Ok(_) => (),
                        Err(err) => {
                            error!("failed to set clipboard in response to OSC 52: {:?}", err)
                        }
                    }
                }
            }
            OperatingSystemCommand::SystemNotification(message) => {
                error!("Application sends SystemNotification: {}", message);
            }
            OperatingSystemCommand::ChangeColorNumber(specs) => {
                error!("ChangeColorNumber: {:?}", specs);
                for pair in specs {
                    match pair.color {
                        ColorOrQuery::Query => {
                            let response =
                                OperatingSystemCommand::ChangeColorNumber(vec![ChangeColorPair {
                                    palette_index: pair.palette_index,
                                    color: ColorOrQuery::Color(
                                        self.palette.colors.0[pair.palette_index as usize],
                                    ),
                                }]);
                            write!(self.host.writer(), "{}", response).ok();
                        }
                        ColorOrQuery::Color(c) => {
                            self.palette.colors.0[pair.palette_index as usize] = c;
                        }
                    }
                }
                self.make_all_lines_dirty();
            }
            OperatingSystemCommand::ChangeDynamicColors(first_color, colors) => {
                error!("ChangeDynamicColors: {:?} {:?}", first_color, colors);
                use crate::core::escape::osc::DynamicColorNumber;
                let mut idx: u8 = first_color as u8;
                for color in colors {
                    let which_color: Option<DynamicColorNumber> = num::FromPrimitive::from_u8(idx);
                    if let Some(which_color) = which_color {
                        macro_rules! set_or_query {
                            ($name:ident) => {
                                match color {
                                    ColorOrQuery::Query => {
                                        let response = OperatingSystemCommand::ChangeDynamicColors(
                                            which_color,
                                            vec![ColorOrQuery::Color(self.palette.$name)],
                                        );
                                        write!(self.host.writer(), "{}", response).ok();
                                    }
                                    ColorOrQuery::Color(c) => self.palette.$name = c,
                                }
                            };
                        }
                        match which_color {
                            DynamicColorNumber::TextForegroundColor => set_or_query!(foreground),
                            DynamicColorNumber::TextBackgroundColor => set_or_query!(background),
                            DynamicColorNumber::TextCursorColor => set_or_query!(cursor_bg),
                            DynamicColorNumber::HighlightForegroundColor => {
                                set_or_query!(selection_fg)
                            }
                            DynamicColorNumber::HighlightBackgroundColor => {
                                set_or_query!(selection_bg)
                            }
                            DynamicColorNumber::MouseForegroundColor
                            | DynamicColorNumber::MouseBackgroundColor
                            | DynamicColorNumber::TektronixForegroundColor
                            | DynamicColorNumber::TektronixBackgroundColor
                            | DynamicColorNumber::TektronixCursorColor => {}
                        }
                    }
                    idx += 1;
                }
                self.make_all_lines_dirty();
            }
        }
    }
}
