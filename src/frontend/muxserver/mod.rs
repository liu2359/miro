//! Implements the multiplexer server frontend
use crate::config::Config;
use crate::core::promise::*;
use crate::font::FontConfiguration;
use crate::frontend::FrontEnd;
use crate::mux::tab::Tab;
use crate::mux::window::WindowId;
use crate::mux::Mux;
use crate::server::listener::spawn_listener;
use failure::{bail, Error, Fallible};
use log::info;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

#[derive(Clone)]
struct MuxExecutor {
    tx: Sender<SpawnFunc>,
}

impl BasicExecutor for MuxExecutor {
    fn execute(&self, f: SpawnFunc) {
        self.tx.send(f).expect("MuxExecutor execute failed");
    }
}

impl Executor for MuxExecutor {
    fn clone_executor(&self) -> Box<dyn Executor> {
        Box::new(MuxExecutor { tx: self.tx.clone() })
    }
}

pub struct MuxServerFrontEnd {
    tx: Sender<SpawnFunc>,
    rx: Receiver<SpawnFunc>,
}

impl MuxServerFrontEnd {
    #[cfg_attr(feature = "cargo-clippy", allow(clippy::new_ret_no_self))]
    fn new(start_listener: bool) -> Result<Rc<dyn FrontEnd>, Error> {
        let (tx, rx) = mpsc::channel();

        if start_listener {
            let mux = Mux::get().unwrap();
            spawn_listener(mux.config())?;
        }
        Ok(Rc::new(Self { tx, rx }))
    }

    pub fn try_new() -> Result<Rc<dyn FrontEnd>, Error> {
        Self::new(true)
    }

    pub fn new_null() -> Result<Rc<dyn FrontEnd>, Error> {
        Self::new(false)
    }
}

impl FrontEnd for MuxServerFrontEnd {
    fn executor(&self) -> Box<dyn Executor> {
        Box::new(MuxExecutor { tx: self.tx.clone() })
    }

    fn run_forever(&self) -> Result<(), Error> {
        loop {
            match self.rx.recv() {
                Ok(func) => func(),
                Err(err) => bail!("while waiting for events: {:?}", err),
            }

            if Mux::get().unwrap().is_empty() {
                info!("No more tabs; all done!");
                return Ok(());
            }
        }
    }

    fn spawn_new_window(
        &self,
        _config: &Arc<Config>,
        _fontconfig: &Rc<FontConfiguration>,
        _tab: &Rc<dyn Tab>,
        _window_id: WindowId,
    ) -> Fallible<()> {
        Ok(())
    }
}
