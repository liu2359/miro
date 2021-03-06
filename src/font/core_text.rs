use config::{Config, TextStyle};
use failure::Error;
use font::{FontSystem, NamedFont};

pub type FontSystemImpl = CoreTextSystem;

pub struct CoreTextSystem {}

impl CoreTextSystem {
    pub fn new() -> Self {
        Self {}
    }
}

impl FontSystem for CoreTextSystem {
    fn load_font(&self, config: &Config, style: &TextStyle) -> Result<Box<NamedFont>, Error> {
        bail!("load_font");
    }
}
