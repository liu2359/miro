use crate::gui::executor;
use crate::mux::domain::DomainId;
use crate::mux::renderable::Renderable;
use crate::mux::Mux;
use crate::pty::PtySize;
use crate::term::color::ColorPalette;
use crate::term::selection::SelectionRange;
use crate::term::{KeyCode, KeyModifiers, MouseEvent, TerminalHost};
use downcast_rs::{impl_downcast, Downcast};
use failure::Fallible;
use std::cell::RefMut;
use std::sync::{Arc, Mutex};

static TAB_ID: ::std::sync::atomic::AtomicUsize = ::std::sync::atomic::AtomicUsize::new(0);
pub type TabId = usize;

pub fn alloc_tab_id() -> TabId {
    TAB_ID.fetch_add(1, ::std::sync::atomic::Ordering::Relaxed)
}

const PASTE_CHUNK_SIZE: usize = 1024;

struct Paste {
    tab_id: TabId,
    text: String,
    offset: usize,
}

fn schedule_next_paste(paste: &Arc<Mutex<Paste>>) {
    let paste = Arc::clone(paste);
    crate::core::promise::Future::with_executor(executor(), move || {
        let mut locked = paste.lock().unwrap();
        let mux = Mux::get().unwrap();
        let tab = mux.get_tab(locked.tab_id).unwrap();

        let remain = locked.text.len() - locked.offset;
        let chunk = remain.min(PASTE_CHUNK_SIZE);
        let text_slice = &locked.text[locked.offset..locked.offset + chunk];
        tab.send_paste(text_slice).unwrap();

        if chunk < remain {
            // There is more to send
            locked.offset += chunk;
            schedule_next_paste(&paste);
        }

        Ok(())
    });
}

pub trait Tab: Downcast {
    fn tab_id(&self) -> TabId;
    fn renderer(&self) -> RefMut<dyn Renderable>;
    fn get_title(&self) -> String;
    fn send_paste(&self, text: &str) -> Fallible<()>;
    fn reader(&self) -> Fallible<Box<dyn std::io::Read + Send>>;
    fn writer(&self) -> RefMut<dyn std::io::Write>;
    fn resize(&self, size: PtySize) -> Fallible<()>;
    fn key_down(&self, key: KeyCode, mods: KeyModifiers) -> Fallible<()>;
    fn mouse_event(&self, event: MouseEvent, host: &mut dyn TerminalHost) -> Fallible<()>;
    fn advance_bytes(&self, buf: &[u8], host: &mut dyn TerminalHost);
    fn is_dead(&self) -> bool;
    fn palette(&self) -> ColorPalette;
    fn domain_id(&self) -> DomainId;

    /// Returns the selection range adjusted to the viewport
    /// (eg: it has been normalized and had clip_to_viewport called
    /// on it prior to being returned)
    fn selection_range(&self) -> Option<SelectionRange>;

    fn trickle_paste(&self, text: String) -> Fallible<()> {
        if text.len() <= PASTE_CHUNK_SIZE {
            // Send it all now
            self.send_paste(&text)?;
        } else {
            // It's pretty heavy, so we trickle it into the pty
            self.send_paste(&text[0..PASTE_CHUNK_SIZE])?;

            let paste = Arc::new(Mutex::new(Paste {
                tab_id: self.tab_id(),
                text,
                offset: PASTE_CHUNK_SIZE,
            }));
            schedule_next_paste(&paste);
        }
        Ok(())
    }
}
impl_downcast!(Tab);
