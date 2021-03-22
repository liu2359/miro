extern crate gl_generator;

use gl_generator::{Api, Fallbacks, Profile, Registry};
use std::env;
use std::fs::File;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=src/window/build.rs");
    let dest = PathBuf::from(&env::var("OUT_DIR").unwrap());
    let target = env::var("TARGET").unwrap();
    if !target.contains("macos") {
        let mut file = File::create(&dest.join("egl_bindings.rs")).unwrap();
        let reg = Registry::new(
            Api::Egl,
            (1, 5),
            Profile::Core,
            Fallbacks::All,
            [
                "EGL_KHR_create_context",
                "EGL_EXT_create_context_robustness",
                "EGL_KHR_create_context_no_error",
                "EGL_KHR_platform_x11",
                "EGL_KHR_platform_android",
                "EGL_KHR_platform_wayland",
                "EGL_KHR_platform_gbm",
                "EGL_EXT_platform_base",
                "EGL_EXT_platform_x11",
                "EGL_MESA_platform_gbm",
                "EGL_EXT_platform_wayland",
                "EGL_EXT_platform_device",
                "EGL_KHR_swap_buffers_with_damage",
            ],
        );

        reg.write_bindings(gl_generator::StructGenerator, &mut file).unwrap()
    }
}
