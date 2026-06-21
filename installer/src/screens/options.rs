//! Step 11 — install options the user sets just before the final review:
//! passwordless sudo, disk encryption (LUKS) with scope + passphrase, and the
//! EFI bootloader entry name. Up/Down moves between rows; Left/Right (or Space)
//! toggles a choice row; text rows accept typing.

use crate::app::{App, Screen};
use crate::i18n::t;
use crate::screens::widgets;
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

/// The kind of each visible row, so navigation and editing adapt to whether
/// encryption is enabled (which reveals the scope + passphrase rows).
#[derive(Clone, Copy, PartialEq)]
enum Row {
    Sudo,
    Chaotic,
    Mirrors,
    Encrypt,
    EncScope,
    UsbKey,
    UsbMode,
    EncPass,
    Bootloader,
    OsProber,
    BootId,
}

/// The ordered list of rows currently visible, given the config state. This
/// module backs two screens: the "Bootloader & encryption" step (before the
/// storage step, so the root-encryption choice is made before per-disk choices)
/// and the later "System options" step. The encryption scope row only appears
/// with GRUB, since only GRUB can boot an encrypted /boot.
fn rows_for(app: &App) -> Vec<Row> {
    // System options step: packaging tweaks and passwordless sudo only.
    if app.screen == Screen::Options {
        return vec![Row::Sudo, Row::Chaotic, Row::Mirrors];
    }
    // Bootloader & encryption step. Bootloader first so the rows below can adapt
    // to it; the encryption sub-rows follow the toggle so they stay attached.
    let mut v = vec![Row::Bootloader];
    if app.config.bootloader == "grub" {
        v.push(Row::OsProber);
    }
    v.push(Row::BootId);
    v.push(Row::Encrypt);
    if app.config.encrypt_disk {
        if app.config.bootloader == "grub" {
            v.push(Row::EncScope);
        }
        v.push(Row::UsbKey);
        if !app.config.usb_key_device.is_empty() {
            v.push(Row::UsbMode);
        }
        // Key-only USB mode needs NO passphrase from the user: a throwaway
        // secret is minted internally for setup and removed afterwards, so
        // the row disappears instead of demanding meaningless input.
        if !(app.config.usb_key_only && !app.config.usb_key_device.is_empty()) {
            v.push(Row::EncPass);
        }
    }
    v
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let visible = rows_for(app);
    // Clamp the cursor to the visible rows (it may have pointed at a row that
    // disappeared when encryption was toggled off).
    if app.cursor >= visible.len() {
        app.cursor = visible.len() - 1;
    }

    // Build the layout: intro + one block per visible row + spacer + actions.
    // Rows that can show a long red warning (the two USB rows when armed) get
    // an extra line so the wrapped warning text fits inside the frame instead
    // of overflowing past the right border.
    let mut constraints = vec![Constraint::Length(2)]; // intro
    for row in &visible {
        let tall = match row {
            Row::UsbKey => !app.config.usb_key_device.is_empty(),
            Row::UsbMode => app.config.usb_key_only,
            _ => false,
        };
        constraints.push(Constraint::Length(if tall { 5 } else { 3 }));
    }
    constraints.push(Constraint::Min(0)); // spacer
    constraints.push(Constraint::Length(3)); // actions
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .spacing(1)
        .split(area);

    let intro_key = if app.screen == Screen::Options {
        "opt.intro"
    } else {
        "sec.intro"
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("  {}", t(app.lang, intro_key)),
            theme::dim(),
        ))),
        rows[0],
    );

    for (i, row) in visible.iter().enumerate() {
        let area = rows[i + 1];
        let focused = app.cursor == i;
        match row {
            Row::Sudo => {
                let val = t(app.lang, if app.config.passwordless_sudo { "opt.sudo_nopass" } else { "opt.sudo_pass" });
                let hint = t(app.lang, if app.config.passwordless_sudo { "opt.sudo_nopass_hint" } else { "opt.sudo_pass_hint" });
                draw_choice_row(f, area, focused, &t(app.lang, "opt.sudo"), &val, &hint);
            }
            Row::Chaotic => {
                let val = t(app.lang, if app.config.chaotic_aur { "opt.on" } else { "opt.off" });
                draw_choice_row(f, area, focused, &t(app.lang, "opt.chaotic"), &val, &t(app.lang, "opt.chaotic_hint"));
            }
            Row::Mirrors => {
                let val = t(app.lang, if app.config.optimize_mirrors { "opt.on" } else { "opt.off" });
                draw_choice_row(f, area, focused, &t(app.lang, "opt.mirrors"), &val, &t(app.lang, "opt.mirrors_hint"));
            }
            Row::Encrypt => {
                let val = t(app.lang, if app.config.encrypt_disk { "opt.on" } else { "opt.off" });
                draw_choice_row(f, area, focused, &t(app.lang, "opt.encrypt"), &val, &t(app.lang, "opt.encrypt_hint"));
            }
            Row::OsProber => {
                let val = t(app.lang, if app.config.os_prober { "opt.on" } else { "opt.off" });
                draw_choice_row(f, area, focused, &t(app.lang, "opt.osprober"), &val, &t(app.lang, "opt.osprober_hint"));
            }
            Row::EncScope => {
                let val = t(app.lang, if app.config.encrypt_scope == "full" { "opt.scope_full" } else { "opt.scope_root" });
                draw_choice_row(f, area, focused, &t(app.lang, "opt.scope"), &val, &t(app.lang, "opt.scope_hint"));
            }
            Row::UsbKey => {
                let off = app.config.usb_key_device.is_empty();
                let val = if off {
                    t(app.lang, "opt.off")
                } else {
                    // Show model+size from the detection cache when we still
                    // have it; the bare device path otherwise.
                    app.usb_devices
                        .iter()
                        .find(|d| d.path == app.config.usb_key_device)
                        .map(|d| format!("{} · {} · {}", d.path, d.size, d.model))
                        .unwrap_or_else(|| app.config.usb_key_device.clone())
                };
                // With a stick selected the hint becomes a RED warning: the
                // stick will be wiped and reformatted.
                if off {
                    draw_choice_row(f, area, focused, &t(app.lang, "opt.usbkey"), &val, &t(app.lang, "opt.usbkey_hint"));
                } else {
                    let marker = if focused { "›" } else { " " };
                    let line = Line::from(vec![
                        Span::styled(format!("  {marker} "), theme::gold()),
                        Span::styled(format!("{}: ", t(app.lang, "opt.usbkey")), if focused { theme::gold() } else { theme::normal() }),
                        Span::styled(format!("‹ {val} ›"), if focused { theme::gold() } else { theme::mute() }),
                    ]);
                    let hint = Line::from(Span::styled(
                        format!("      {}", t(app.lang, "opt.usbkey_warn")),
                        theme::warn(),
                    ));
                    f.render_widget(Paragraph::new(vec![line, hint]).wrap(ratatui::widgets::Wrap { trim: true }), area);
                }
            }
            Row::UsbMode => {
                let key_only = app.config.usb_key_only;
                let val = t(app.lang, if key_only { "opt.usbmode_only" } else { "opt.usbmode_backup" });
                if key_only {
                    // Key-only deserves a PERMANENT red warning, not a dim hint.
                    let marker = if focused { "\u{203a}" } else { " " };
                    let line = Line::from(vec![
                        Span::styled(format!("  {marker} "), theme::gold()),
                        Span::styled(format!("{}: ", t(app.lang, "opt.usbmode")), if focused { theme::gold() } else { theme::normal() }),
                        Span::styled(format!("\u{2039} {val} \u{203a}"), if focused { theme::gold() } else { theme::mute() }),
                    ]);
                    let hint = Line::from(Span::styled(
                        format!("      {}", t(app.lang, "opt.usbmode_only_warn")),
                        theme::warn(),
                    ));
                    f.render_widget(Paragraph::new(vec![line, hint]).wrap(ratatui::widgets::Wrap { trim: true }), area);
                } else {
                    draw_choice_row(f, area, focused, &t(app.lang, "opt.usbmode"), &val, &t(app.lang, "opt.usbmode_hint"));
                }
            }
            Row::EncPass => {
                // Masked passphrase text field. The whole focused line shares
                // ONE intensity (gold = bold accent): mixing bold and non-bold
                // spans triggers the VT's unreliable intensity-reset handling
                // on incremental redraws (stale-bright first • while typing).
                let caret = if focused { "▏" } else { "" };
                let masked: String = "•".repeat(app.config.luks_passphrase.chars().count());
                let line = Line::from(vec![
                    Span::styled(format!("  {} ", if focused { "›" } else { " " }), theme::gold()),
                    Span::styled(format!("{}: ", t(app.lang, "opt.passphrase")), if focused { theme::gold() } else { theme::normal() }),
                    Span::styled(format!("[ {masked}{caret} ]"), if focused { theme::gold() } else { theme::mute() }),
                ]);
                let hint = Line::from(Span::styled(format!("      {}", t(app.lang, "opt.passphrase_hint")), theme::dim()));
                f.render_widget(Paragraph::new(vec![line, hint]).wrap(ratatui::widgets::Wrap { trim: true }), area);
            }
            Row::Bootloader => {
                let val = match app.config.bootloader.as_str() {
                    "refind" => "rEFInd",
                    "limine" => "Limine",
                    _ => "GRUB",
                };
                // Warn when the chosen bootloader can't do encrypted /boot.
                let hint_key = if app.config.encrypt_disk
                    && app.config.encrypt_scope == "full"
                    && app.config.bootloader != "grub"
                {
                    "opt.bootloader_warn"
                } else {
                    "opt.bootloader_hint"
                };
                draw_choice_row(f, area, focused, &t(app.lang, "opt.bootloader"), val, &t(app.lang, hint_key));
            }
            Row::BootId => {
                let caret = if focused { "▏" } else { "" };
                let line = Line::from(vec![
                    Span::styled(format!("  {} ", if focused { "›" } else { " " }), theme::gold()),
                    Span::styled(format!("{}: ", t(app.lang, "opt.bootid")), if focused { theme::gold() } else { theme::normal() }),
                    Span::styled(format!("[ {}{} ]", app.config.bootloader_id, caret), if focused { theme::gold() } else { theme::mute() }),
                ]);
                let hint = Line::from(Span::styled(format!("      {}", t(app.lang, "opt.bootid_hint")), theme::dim()));
                f.render_widget(Paragraph::new(vec![line, hint]).wrap(ratatui::widgets::Wrap { trim: true }), area);
            }
        }
    }

    // Can only advance if encryption-off, or encryption-on with a passphrase.
    // Also block the incompatible combo: full-disk encryption (encrypted /boot)
    // only works with GRUB, since rEFInd/Limine can't decrypt /boot.
    let enc_ok = !app.config.encrypt_disk
        || !app.config.luks_passphrase.is_empty()
        || (app.config.usb_key_only && !app.config.usb_key_device.is_empty());
    let boot_ok = !(app.config.encrypt_disk
        && app.config.encrypt_scope == "full"
        && app.config.bootloader != "grub");
    app.can_advance = enc_ok && boot_ok;
    let actions_area = rows[rows.len() - 1];
    widgets::action_row(f, actions_area, &t(app.lang, "app.back"), &t(app.lang, "app.next"), app.can_advance);
}

fn draw_choice_row(f: &mut Frame, area: Rect, focused: bool, label: &str, value: &str, hint: &str) {
    let marker = if focused { "›" } else { " " };
    let label_style = if focused { theme::gold() } else { theme::normal() };
    // The focused value GLOWS bright bold cyan — same family as the rest of
    // the UI. (A reversed-video fill was tried here and looked muddy on real
    // fbcon palettes: grey text on a dark-cyan slab. Plain bright text wins.)
    let value_style = if focused { theme::gold() } else { theme::mute() };
    let line = Line::from(vec![
        Span::styled(format!("  {marker} "), theme::gold()),
        Span::styled(format!("{label}: "), label_style),
        Span::styled(format!("‹ {value} ›"), value_style),
    ]);
    let hint_line = Line::from(Span::styled(format!("      {hint}"), theme::dim()));
    f.render_widget(
        Paragraph::new(vec![line, hint_line]).wrap(ratatui::widgets::Wrap { trim: true }),
        area,
    );
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    let visible = rows_for(app);
    let cur = visible.get(app.cursor).copied().unwrap_or(Row::Sudo);

    // Up/Down navigation is common to all rows.
    match key.code {
        KeyCode::Up => {
            app.cursor = app.cursor.saturating_sub(1);
            return;
        }
        KeyCode::Down => {
            app.cursor = (app.cursor + 1).min(visible.len() - 1);
            return;
        }
        KeyCode::Esc => {
            // Esc steps focus to the previous row. (When already on the first
            // row, the global handler intercepts Esc and leaves to the previous
            // screen, so reaching here always means there's a row above.)
            app.cursor = app.cursor.saturating_sub(1);
            return;
        }
        _ => {}
    }

    // Text rows (passphrase, bootid) accept typing.
    match cur {
        Row::EncPass => match key.code {
            KeyCode::Char(c) => {
                if app.config.luks_passphrase.chars().count() < 128 {
                    app.config.luks_passphrase.push(c);
                }
            }
            KeyCode::Backspace => {
                app.config.luks_passphrase.pop();
            }
            KeyCode::Enter => enter_step(app),
            _ => {}
        },
        Row::BootId => match key.code {
            KeyCode::Char(c) if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' => {
                if app.config.bootloader_id.chars().count() < 32 {
                    app.config.bootloader_id.push(c);
                }
            }
            KeyCode::Backspace => {
                app.config.bootloader_id.pop();
            }
            KeyCode::Enter => enter_step(app),
            _ => {}
        },
        // Choice rows toggle with Left/Right/Space; Enter steps to the next row
        // (and only advances the page from the LAST row — like a normal
        // installer). Left moves BACKWARD through multi-state rows (bootloader),
        // Right/Space forward — so the arrows are never a one-way street.
        _ => match key.code {
            KeyCode::Right | KeyCode::Char(' ') => toggle(app, cur, true),
            KeyCode::Left => toggle(app, cur, false),
            KeyCode::Enter => enter_step(app),
            _ => {}
        },
    }
}

/// Enter steps the cursor to the next visible row; on the LAST row it tries to
/// advance to the next screen (subject to validation). This matches the
/// installer-wide convention: Enter = next field, then next page.
fn enter_step(app: &mut App) {
    let visible = rows_for(app);
    if app.cursor + 1 < visible.len() {
        app.cursor += 1;
    } else {
        advance(app);
    }
}

fn advance(app: &mut App) {
    if app.config.bootloader_id.trim().is_empty() {
        app.config.bootloader_id = "Artix".into();
    }
    // Block advancing if encryption is on but no passphrase was set — UNLESS
    // key-only USB mode is active, where a passphrase is intentionally absent
    // (a throwaway key is minted internally and removed afterwards). This must
    // mirror `enc_ok` in handle_key, or the screen silently refuses to advance.
    let key_only = app.config.usb_key_only && !app.config.usb_key_device.is_empty();
    if app.config.encrypt_disk && app.config.luks_passphrase.is_empty() && !key_only {
        return;
    }
    // Block the incompatible combo: encrypted /boot needs GRUB.
    if app.config.encrypt_disk
        && app.config.encrypt_scope == "full"
        && app.config.bootloader != "grub"
    {
        return;
    }
    app.goto_next();
}

fn toggle(app: &mut App, row: Row, forward: bool) {
    match row {
        Row::Sudo => app.config.passwordless_sudo = !app.config.passwordless_sudo,
        Row::Chaotic => app.config.chaotic_aur = !app.config.chaotic_aur,
        Row::OsProber => app.config.os_prober = !app.config.os_prober,
        Row::Mirrors => app.config.optimize_mirrors = !app.config.optimize_mirrors,
        Row::Encrypt => app.config.encrypt_disk = !app.config.encrypt_disk,
        Row::UsbKey => {
            // Refresh the removable-device list on every press so a stick
            // plugged in while on this screen shows up immediately. The
            // install disk itself is excluded even if it's removable.
            app.usb_devices = crate::system::disk::list()
                .unwrap_or_default()
                .into_iter()
                .filter(|d| d.removable && d.path != app.config.disk)
                .collect();
            let mut cycle: Vec<String> = vec![String::new()];
            cycle.extend(app.usb_devices.iter().map(|d| d.path.clone()));
            let i = cycle
                .iter()
                .position(|p| *p == app.config.usb_key_device)
                .unwrap_or(0);
            let n = if forward {
                (i + 1) % cycle.len()
            } else {
                (i + cycle.len() - 1) % cycle.len()
            };
            app.config.usb_key_device = cycle[n].clone();
            // The USB key unlocks ROOT in the initramfs; GRUB's own prompt
            // for an encrypted /boot would defeat it, so force root-only.
            if !app.config.usb_key_device.is_empty() {
                app.config.encrypt_scope = "root".into();
            } else {
                // No stick: key-only mode is meaningless; reset it so the
                // dangerous flag can't survive invisibly.
                app.config.usb_key_only = false;
            }
        }
        Row::UsbMode => {
            app.config.usb_key_only = !app.config.usb_key_only;
            if app.config.usb_key_only {
                app.config.luks_passphrase.clear();
            }
        }
        Row::EncScope => {
            // "full" (encrypted /boot) only works with GRUB. With another
            // bootloader, lock the scope to root-only.
            if app.config.bootloader == "grub" {
                app.config.encrypt_scope =
                    if app.config.encrypt_scope == "full" { "root".into() } else { "full".into() };
                // Encrypted /boot means GRUB prompts before the initramfs ever
                // runs — the USB auto-unlock key would be pointless, so the
                // two options are mutually exclusive.
                if app.config.encrypt_scope == "full" {
                    app.config.usb_key_device.clear();
                }
            } else {
                app.config.encrypt_scope = "root".into();
            }
        }
        Row::Bootloader => {
            // Cycle grub ↔ refind ↔ limine in BOTH directions:
            // Right: grub → refind → limine → grub; Left: the reverse.
            const ORDER: [&str; 3] = ["grub", "refind", "limine"];
            let i = ORDER.iter().position(|b| *b == app.config.bootloader).unwrap_or(0);
            let n = if forward { (i + 1) % ORDER.len() } else { (i + ORDER.len() - 1) % ORDER.len() };
            app.config.bootloader = ORDER[n].into();
            // If we moved away from GRUB, an encrypted /boot is no longer
            // possible, so force the scope back to root-only.
            if app.config.bootloader != "grub" && app.config.encrypt_scope == "full" {
                app.config.encrypt_scope = "root".into();
            }
        }
        _ => {}
    }
}

/// Display-manager cycle order. ids are stored in config; SDDM first (the
/// default for graphical desktops), then the greetd greeters (Arch Wiki set
/// available as repo packages), then none (boot to TTY).
// Only greeters available in the OFFICIAL Artix repositories are offered, so
// every choice installs cleanly without the AUR. Confirmed in repos:
// greetd-tuigreet (world) and greetd-regreet (galaxy). NOT in repos (AUR-only,
// so deliberately excluded): greetd-gtkgreet, greetd-wlgreet — and agreety,
// which was dropped earlier for being a getty-replacement that doesn't switch
// to greetd's VT. SDDM is the full DM; "none" boots to a TTY.
pub const DM_ORDER: [&str; 4] = ["sddm", "tuigreet", "regreet", "none"];

/// UI label for a display-manager id.
pub fn dm_label(id: &str) -> &'static str {
    match id {
        "sddm" => "SDDM",
        "tuigreet" => "greetd + tuigreet",
        "regreet" => "greetd + ReGreet (cage)",
        _ => "—",
    }
}

pub fn footer_hint(app: &App) -> String {
    t(app.lang, "opt.footer")
}
