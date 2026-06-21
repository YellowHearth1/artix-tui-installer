//! Screen dispatch. Every screen module exposes `draw`, `handle_key`, and may
//! optionally provide `tick` (background work) and `footer_hint` (custom keys).

pub mod widgets;

pub mod aur;
mod desktop;
mod disk;
mod finish;
mod keyboard;
mod kernel;
pub mod options;
mod language;
mod mode;
mod recovery;
pub mod packages;
pub mod summary;
mod storage;
mod timezone;
mod user;
pub mod wifi;

use crate::app::{App, Screen};
use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    match app.screen {
        Screen::Language => language::draw(f, app, area),
        Screen::Timezone => timezone::draw(f, app, area),
        Screen::Wifi => wifi::draw(f, app, area),
        Screen::Keyboard => keyboard::draw(f, app, area),
        Screen::Kernel => kernel::draw(f, app, area),
        Screen::Desktop => desktop::draw(f, app, area),
        Screen::Packages => packages::draw(f, app, area),
        Screen::Aur => aur::draw(f, app, area),
        Screen::Disk => disk::draw(f, app, area),
        Screen::Storage => storage::draw(f, app, area),
        Screen::User => user::draw(f, app, area),
        Screen::Security => options::draw(f, app, area),
        Screen::Options => options::draw(f, app, area),
        Screen::Summary => summary::draw(f, app, area),
        Screen::Finish => finish::draw(f, app, area),
        Screen::Mode => mode::draw(f, app, area),
        Screen::Recovery => recovery::draw(f, app, area),
    }
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    match app.screen {
        Screen::Language => language::handle_key(app, key),
        Screen::Timezone => timezone::handle_key(app, key),
        Screen::Wifi => wifi::handle_key(app, key),
        Screen::Keyboard => keyboard::handle_key(app, key),
        Screen::Kernel => kernel::handle_key(app, key),
        Screen::Desktop => desktop::handle_key(app, key),
        Screen::Packages => packages::handle_key(app, key),
        Screen::Aur => aur::handle_key(app, key),
        Screen::Disk => disk::handle_key(app, key),
        Screen::Storage => storage::handle_key(app, key),
        Screen::User => user::handle_key(app, key),
        Screen::Security => options::handle_key(app, key),
        Screen::Options => options::handle_key(app, key),
        Screen::Summary => summary::handle_key(app, key),
        Screen::Finish => finish::handle_key(app, key),
        Screen::Mode => mode::handle_key(app, key),
        Screen::Recovery => recovery::handle_key(app, key),
    }
}

pub fn tick(app: &mut App) {
    match app.screen {
        Screen::Summary => summary::tick(app),
        Screen::Wifi => wifi::tick(app),
        Screen::Packages => packages::tick(app),
        Screen::Aur => aur::tick(app),
        _ => {}
    }
}

/// Optional per-screen footer hint override.
pub fn footer_hint(app: &App) -> Option<String> {
    match app.screen {
        Screen::Wifi => Some(wifi::footer_hint(app)),
        Screen::Keyboard => Some(keyboard::footer_hint(app)),
        Screen::Kernel => Some(kernel::footer_hint(app)),
        Screen::Desktop => Some(desktop::footer_hint(app)),
        Screen::Disk => Some(disk::footer_hint(app)),
        Screen::Storage => Some(storage::footer_hint(app)),
        Screen::Packages => Some(packages::footer_hint(app)),
        Screen::Aur => Some(aur::footer_hint(app)),
        Screen::Timezone => Some(timezone::footer_hint(app)),
        Screen::Summary => Some(summary::footer_hint(app)),
        Screen::User => Some(user::footer_hint(app)),
        Screen::Security => Some(options::footer_hint(app)),
        Screen::Options => Some(options::footer_hint(app)),
        Screen::Recovery => Some(recovery::footer_hint(app)),
        _ => None,
    }
}
