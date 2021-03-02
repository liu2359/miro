#[macro_use]
extern crate bitflags;
#[macro_use]
extern crate failure;
extern crate egli;
extern crate euclid;
extern crate gl;
#[macro_use]
extern crate glium;
#[cfg(not(target_os = "macos"))]
extern crate fontconfig; // from servo-fontconfig
#[cfg(not(target_os = "macos"))]
extern crate freetype;
extern crate harfbuzz_sys;
extern crate libc;
extern crate mio;
extern crate serde;
extern crate unicode_width;
#[macro_use]
extern crate serde_derive;
extern crate palette;
extern crate toml;
#[macro_use]
pub mod log;

use clap::{crate_description, crate_name, crate_version, App, AppSettings, Arg};

use sysinfo::{System, SystemExt};

use config::{Config, Theme};

use term::color::RgbColor;

use failure::Error;
use mio::{Poll, PollOpt, Ready, Token};
use std::time::Duration;

extern crate xcb;
extern crate xcb_util;

use mio::unix::EventedFd;
use mio::Events;
use std::env;
use std::ffi::CStr;
use std::os::unix::io::AsRawFd;
use std::process::Command;
use std::str;

mod config;
mod font;
mod game_loop;
mod opengl;
mod term;
use font::{ftwrap, FontConfiguration};

mod pty;
mod sigchld;

#[cfg(all(unix, not(target_os = "macos")))]
mod x_window;
#[cfg(all(unix, not(target_os = "macos")))]
use x_window::xwin::TerminalWindow;

pub const ANIMATION_SPAN: u32 = 5;

/// Determine which shell to run.
/// We take the contents of the $SHELL env var first, then
/// fall back to looking it up from the password database.
fn get_shell() -> Result<String, Error> {
    env::var("SHELL").or_else(|_| {
        let ent = unsafe { libc::getpwuid(libc::getuid()) };

        if ent.is_null() {
            Ok("/bin/sh".into())
        } else {
            let shell = unsafe { CStr::from_ptr((*ent).pw_shell) };
            shell
                .to_str()
                .map(str::to_owned)
                .map_err(|e| format_err!("failed to resolve shell: {:?}", e))
        }
    })
}

fn run(theme: Theme) -> Result<(), Error> {
    let poll = Poll::new()?;
    let conn = x_window::Connection::new()?;
    let sys = System::new();
    let waiter = sigchld::ChildWaiter::new()?;

    let config = Config::new(theme);
    println!("Using configuration: {:#?}", config);

    // First step is to figure out the font metrics so that we know how
    // big things are going to be.

    let fontconfig = FontConfiguration::new(config.clone());
    let font = fontconfig.default_font()?;

    // we always load the cell_height for font 0,
    // regardless of which font we are shaping here,
    // so that we can scale glyphs appropriately
    let (cell_height, cell_width, _) = font.borrow_mut().get_metrics()?;

    let initial_cols = 80u16;
    let initial_rows = 24u16;
    let initial_pixel_width = initial_cols * cell_width.ceil() as u16;
    let initial_pixel_height = initial_rows * cell_height.ceil() as u16;

    let (master, slave) =
        pty::openpty(initial_rows, initial_cols, initial_pixel_width, initial_pixel_height)?;

    let cmd = Command::new(get_shell()?);
    let child = slave.spawn_command(cmd)?;
    eprintln!("spawned: {:?}", child);

    // Ask mio to watch the pty for input from the child process
    poll.register(&master, Token(0), Ready::readable(), PollOpt::edge())?;
    // Ask mio to monitor the X connection fd
    poll.register(&EventedFd(&conn.as_raw_fd()), Token(1), Ready::readable(), PollOpt::edge())?;

    poll.register(&waiter, Token(2), Ready::readable(), PollOpt::edge())?;

    let game_loop = game_loop::GameLoop::new();

    poll.register(&game_loop, Token(3), Ready::readable(), PollOpt::edge())?;

    let terminal = term::Terminal::new(
        initial_rows as usize,
        initial_cols as usize,
        config.scrollback_lines.unwrap_or(3500),
    );

    let mut window = TerminalWindow::new(
        &conn,
        initial_pixel_width,
        initial_pixel_height,
        terminal,
        master,
        child,
        config,
        sys,
    )?;

    window.show();

    let mut events = Events::with_capacity(8);
    conn.flush();

    let mut count = 0;
    loop {
        if poll.poll(&mut events, Some(Duration::new(0, 0)))? == 0 {
            // No immediately ready events.  Before we go to sleep,
            // make sure we've flushed out any pending X work.
            conn.flush();

            poll.poll(&mut events, None)?;
        }

        for event in &events {
            if event.token() == Token(3) {
                if count % ANIMATION_SPAN == 0 {
                    window.paint(true)?;
                }
                count += 1;
            }
            if event.token() == Token(0) && event.readiness().is_readable() {
                window.handle_pty_readable_event();
            }
            if event.token() == Token(1) && event.readiness().is_readable() {
                // Each time the XCB Connection FD shows as readable, we perform
                // a single poll against the connection and then eagerly consume
                // all of the queued events that came along as part of that batch.
                // This is important because we can't assume that one readiness
                // event from the kerenl maps to a single XCB event.  We need to be
                // sure that all buffered/queued events are consumed before we
                // allow the mio poll() routine to put us to sleep, otherwise we
                // will effectively hang without updating all the state.
                match conn.poll_for_event() {
                    Some(event) => {
                        window.dispatch_event(event)?;
                        // Since we read one event from the connection, we must
                        // now eagerly consume the rest of the queued events.
                        loop {
                            match conn.poll_for_queued_event() {
                                Some(event) => window.dispatch_event(event)?,
                                None => break,
                            }
                        }
                    }
                    None => {}
                }

                // If we got disconnected from the display server, we cannot continue
                conn.has_error()?;
            }

            if event.token() == Token(2) {
                println!("sigchld ready");
                let pid = waiter.read_one()?;
                println!("got sigchld from pid {}", pid);
                window.test_for_child_exit()?;
            }
        }
    }
}

fn main() {
    let matches = App::new(crate_name!())
        .version(crate_version!())
        .about(crate_description!())
        .setting(AppSettings::ColoredHelp)
        .setting(AppSettings::DeriveDisplayOrder)
        .setting(AppSettings::UnifiedHelpMessage)
        .arg(
            Arg::with_name("theme")
                .short("t")
                .long("theme")
                .help("Which theme to use.")
                .possible_values(&["mario", "sonic", "pika", "mega", "kirby"])
                .default_value("mario"),
        )
        .get_matches();

    let theme = match matches.value_of("theme") {
        Some("mario") => Theme {
            spritesheet_path: String::from("assets/gfx/mario.json"),
            header_color: RgbColor { red: 99, green: 137, blue: 250 },
        },
        Some("sonic") => Theme {
            spritesheet_path: String::from("assets/gfx/sonic.json"),
            header_color: RgbColor { red: 8, green: 129, blue: 0 },
        },
        Some("pika") => Theme {
            spritesheet_path: String::from("assets/gfx/pika.json"),
            header_color: RgbColor { red: 176, green: 139, blue: 24 },
        },
        Some("mega") => Theme {
            spritesheet_path: String::from("assets/gfx/mega.json"),
            header_color: RgbColor { red: 1, green: 135, blue: 147 },
        },
        Some("kirby") => Theme {
            spritesheet_path: String::from("assets/gfx/kirby.json"),
            header_color: RgbColor { red: 242, green: 120, blue: 141 },
        },
        _ => unreachable!("other values are not allowed"),
    };

    run(theme).unwrap();
}
