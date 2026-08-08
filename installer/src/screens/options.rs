//! Step 11 — install options the user sets just before the final review:
//! passwordless sudo, disk encryption (LUKS) with scope + passphrase, and the
//! EFI bootloader entry name. Up/Down moves between rows; Left/Right (or Space)
//! toggles a choice row; text rows accept typing.

use crate::app::{App, Bootloader, Screen};
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
#[derive(Clone, Copy, PartialEq, Debug)]
enum Row {
    Sudo,
    Escalation,
    Chaotic,
    Auris,
    Mirrors,
    Encrypt,
    /// A non-interactive note shown in place of the encryption toggle when the
    /// install shares a disk with Windows (alongside/manual) — LUKS there isn't
    /// wired up in v1, so the toggle would do nothing.
    EncBlocked,
    EncScope,
    UsbKey,
    UsbMode,
    EncPass,
    /// A heading above the list of extra things that can be encrypted. Not
    /// selectable — it exists so the checkboxes below it read as a group and
    /// not as more options belonging to the root's own encryption.
    EncExtraHead,
    /// The separate /home, when it is on a disk of its own.
    EncHome,
    /// One per data partition that is going to be formatted. The index is into
    /// the list `enc_data_targets` returns, not into `extra_disks` — a disk
    /// kept as-is cannot be encrypted and must not take a row.
    EncData(usize),
    /// The sentence explaining where the keys for those live. Not selectable.
    EncExtraNote,
    Zswap,
    ZswapCompressor,
    ZswapPercent,
    EarlyOom,
    EarlyOomPercent,
    Bootloader,
    OsProber,
    BootId,
    SecureBoot,
}

/// The ordered list of rows currently visible, given the config state. This
/// module backs two screens: the "Bootloader & encryption" step (before the
/// storage step, so the root-encryption choice is made before per-disk choices)
/// and the later "System options" step. The encryption scope row only appears
/// with GRUB, since only GRUB can boot an encrypted /boot.
/// The separate /home, if there is one worth offering to encrypt.
///
/// Only a /home on a disk of ITS OWN: on the system disk it is inside whatever
/// the root already does, and on a disk shared with another OS it is not ours
/// to encrypt. Returns (device, disk) so the row can name both.
fn enc_home_target(app: &App) -> Option<(String, String)> {
    let c = &app.config;
    if !c.partition_mode.is_manual_family() || c.manual_home.is_empty() {
        return None;
    }
    let disk = c.manual_home_disk.clone();
    if disk.is_empty() {
        return None;
    }
    // A /home ON THE DRIVE WE ARE ALREADY PARTITIONING needs no further proof:
    // that disk is the one this plan is carving up, and its /home is a slot the
    // plan itself creates. Requiring a SEPARATE drive was my own addition and it
    // was wrong — a root and a /home side by side on one disk is the ordinary
    // layout, and it was the one case the checkbox never appeared for.
    //
    // A /home somewhere ELSE still has to prove the drive is ours: one carrying
    // somebody else's partitions is not ours to encrypt, even when the only
    // thing we plan to write there is a new /home. Being wrong in that direction
    // destroys data that was never ours.
    let with_root = !c.manual_root_disk.is_empty() && disk == c.manual_root_disk;
    if !with_root && !crate::screens::parts::home_disk_is_exclusively_ours(app) {
        return None;
    }
    Some((c.manual_home.clone(), disk))
}

#[cfg(test)]
mod home_encryption_tests {
    use super::*;
    use crate::app::PartitionMode;

    /// A /home BESIDE THE ROOT ON ONE DISK is the ordinary layout, and it was
    /// the one the encryption checkbox never appeared for.
    ///
    /// Reported from QEMU: a 30 GiB NVMe with an ESP, a root and a /home, root
    /// encryption on — and "Шифрування інших розділів" listed only a data
    /// partition on the other drive. The rule demanded /home have a drive to
    /// itself, which was my own addition, not the safety check the user asked
    /// for. That one is about drives holding somebody else's partitions.
    #[test]
    fn a_home_on_the_root_disk_can_still_be_encrypted() {
        let mut app = App::new();
        app.config.partition_mode = PartitionMode::Manual;
        app.config.manual_solo = true;
        app.config.manual_root = "/dev/nvme0n1p2".into();
        app.config.manual_root_disk = "/dev/nvme0n1".into();
        app.config.manual_home = "/dev/nvme0n1p3".into();
        app.config.manual_home_disk = "/dev/nvme0n1".into();

        let offered = enc_home_target(&app);
        assert_eq!(
            offered,
            Some(("/dev/nvme0n1p3".into(), "/dev/nvme0n1".into())),
            "a /home on the disk this very plan is partitioning was not offered"
        );

        // With no /home planned at all there is nothing to offer.
        app.config.manual_home.clear();
        assert!(enc_home_target(&app).is_none());
    }
}

/// The data partitions that CAN be encrypted: the ones being formatted.
///
/// A partition kept as-is is kept precisely because its contents matter, and
/// encrypting it would destroy them — so it never appears here, and there is
/// no checkbox to tick by mistake.
fn enc_data_targets(app: &App) -> Vec<(String, String)> {
    app.config
        .extra_disks
        .iter()
        .filter(|d| d.format && !d.mountpoint.is_empty() && d.mountpoint != "/home")
        .map(|d| (d.disk.clone(), d.mountpoint.clone()))
        .collect()
}

/// How many rows this entry needs when the screen is NOT compact.
///
/// One function, read by both the layout and the does-it-fit estimate: two
/// numbers describing one layout drift apart, and when they did, choosing a USB
/// key silently collapsed the whole screen.
fn row_height(app: &App, row: &Row, width: u16) -> u16 {
    // How tall a wrapped warning actually is at THIS width, rather than the
    // worst case. The fixed 5 was sized for a narrow console, so on a wide one
    // it left three empty rows under a one-line sentence — a gap twice the size
    // of the spacing between every other option, right in the middle of the
    // screen.
    let wrapped = |key: &str| -> u16 {
        let usable = width.saturating_sub(8).max(20) as usize;
        let n = t(app.lang, key).chars().count();
        // label + the warning's own lines, and never less than the plain row.
        (1 + n.div_ceil(usable)) as u16
    };
    match row {
        // A picked stick adds a red warning that has to fit inside the frame.
        Row::UsbKey if !app.config.usb_key_device.is_empty() => wrapped("opt.usbkey_warn").max(3),
        Row::UsbMode if app.config.usb_key_only => wrapped("opt.usbmode_only_warn").max(3),
        // The extra-encryption block is a list: a heading and one line each.
        Row::EncExtraHead | Row::EncHome | Row::EncData(_) => 1,
        Row::EncExtraNote => 2,
        _ => 3,
    }
}

fn rows_for(app: &App) -> Vec<Row> {
    // System options step: packaging tweaks and passwordless sudo only.
    if app.screen == Screen::Options {
        // Memory tuning lives here, and its sub-rows only appear once the
        // feature is on — three dead rows would otherwise sit in front of
        // everyone who does not need them.
        let mut v = vec![
            Row::Sudo,
            Row::Escalation,
            Row::Chaotic,
            Row::Auris,
            Row::Mirrors,
        ];
        // zswap is a compressed cache IN FRONT OF the swap device. With no swap
        // there is nothing behind it, so the row is not shown at all rather than
        // offered and quietly ignored — a manual layout with no swap partition
        // was being asked to choose a compression algorithm for a cache that
        // could never hold anything.
        if app.config.has_swap() {
            v.push(Row::Zswap);
            if app.config.zswap {
                v.push(Row::ZswapCompressor);
                v.push(Row::ZswapPercent);
            }
        }
        v.push(Row::EarlyOom);
        if app.config.earlyoom {
            v.push(Row::EarlyOomPercent);
        }
        return v;
    }
    // Bootloader & encryption step. Bootloader first, with the encryption block
    // immediately after it (its sub-rows stay attached to the toggle) — the two
    // decisions belong together. Boot extras (os-prober, the UEFI entry name)
    // come after the encryption block.
    // On a disk SHARED with Windows (alongside/manual), v1 can't set up LUKS —
    // so instead of an encryption toggle that the plan silently ignores, show a
    // one-line note explaining why. A separate-disk or whole-disk install (Auto)
    // gets the real toggle: that disk is entirely Artix's.
    // "Shared" means another OS lives on this disk — that is what makes LUKS
    // unavailable, not manual partitioning as such. A SOLO manual install owns
    // the disk, so it gets the real toggle like any whole-disk install.
    let shared_disk = app.config.partition_mode.is_manual_family() && !app.config.manual_solo;
    let mut v = vec![Row::Bootloader];
    if shared_disk {
        v.push(Row::EncBlocked);
    } else {
        v.push(Row::Encrypt);
    }
    if app.config.encrypt_disk && !shared_disk {
        if app.config.bootloader == Bootloader::Grub {
            v.push(Row::EncScope);
        }
        // A USB key auto-unlocks ROOT from the initramfs. Full-disk encryption
        // (encrypted /boot) makes GRUB prompt for the passphrase BEFORE the
        // initramfs ever runs, so an auto-unlock key would be pointless — the
        // two are mutually exclusive. Offer the USB key ONLY with root-only
        // encryption, so the choice is structural (the row simply isn't there
        // under full-disk) instead of silently flipping the scope back to root
        // the moment a stick is picked.
        if app.config.encrypt_scope != "full" {
            v.push(Row::UsbKey);
            if !app.config.usb_key_device.is_empty() {
                v.push(Row::UsbMode);
            }
        }
        // Key-only USB mode needs NO passphrase from the user: a throwaway
        // secret is minted internally for setup and removed afterwards, so
        // the row disappears instead of demanding meaningless input.
        if !app.config.usb_key_only || app.config.usb_key_device.is_empty() {
            v.push(Row::EncPass);
        }
    }
    // EVERYTHING ELSE THAT CAN BE ENCRYPTED, in one place with the root.
    //
    // These used to be set with `e` in the partition editor, which put the same
    // word on two screens meaning two different mechanisms — and made the
    // system disk look unencryptable there, because it is not a "data disk".
    // One decision belongs in one place.
    let targets = enc_data_targets(app);
    if enc_home_target(app).is_some() || !targets.is_empty() {
        v.push(Row::EncExtraHead);
        if enc_home_target(app).is_some() {
            v.push(Row::EncHome);
        }
        for i in 0..targets.len() {
            v.push(Row::EncData(i));
        }
        v.push(Row::EncExtraNote);
    }
    if app.config.bootloader == Bootloader::Grub {
        v.push(Row::OsProber);
    }
    v.push(Row::BootId);
    // Secure Boot preparation is offered ONLY for EFISTUB — the one bootloader
    // where signing is clean (just the kernel via sbctl, no rebuilds, no shim,
    // no systemd). GRUB/rEFInd/Limine Secure Boot is far more fragile on Artix
    // and deliberately not offered here.
    if app.config.bootloader == Bootloader::Efistub {
        v.push(Row::SecureBoot);
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

    // Reserve the action row FIRST, as its own split. Everything below is laid
    // out inside what's left, so however many option rows the current config
    // produces (Security grows to ~10 with encryption armed), the Next button
    // can never be the constraint ratatui silently clips away.
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(area);
    let (body, actions_area) = (outer[0], outer[1]);

    // Roomy layout: intro(2) + 3 rows each (5 for the USB rows when armed, so a
    // wrapped red warning fits inside the frame) + 1 spacing between each. On a
    // 59x15 panel that needs ~23 rows and there are 12, so below the threshold
    // each option collapses to a single line — and only the FOCUSED one keeps
    // its explanatory hint, which is the line the user is actually reading.
    // THE ESTIMATE AND THE LAYOUT MUST AGREE, so both ask the same function.
    //
    // The estimate assumed three rows for every entry. That was already a
    // guess, and it became a wrong one: the extra-encryption block draws a
    // single line per row, while picking a USB key makes two rows five lines
    // tall. The sum overshot, the screen decided it did not fit, and EVERYTHING
    // collapsed to one line each — a wall of text, triggered by choosing a
    // stick. Two numbers describing one layout will always drift; there is now
    // one.
    let full_need: usize = 2
        + visible
            .iter()
            .map(|r| row_height(app, r, body.width) as usize)
            .sum::<usize>()
        + visible.len()
        + 1;
    let compact = (body.height as usize) < full_need;

    let mut constraints = vec![Constraint::Length(if compact { 1 } else { 2 })]; // intro
    for (i, row) in visible.iter().enumerate() {
        if compact {
            constraints.push(Constraint::Length(if i == app.cursor { 2 } else { 1 }));
        } else {
            constraints.push(Constraint::Length(row_height(app, row, body.width)));
        }
    }
    constraints.push(Constraint::Min(0)); // spacer
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .spacing(if compact { 0 } else { 1 })
        .split(body);

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
                let val = t(
                    app.lang,
                    if app.config.passwordless_sudo {
                        "opt.sudo_nopass"
                    } else {
                        "opt.sudo_pass"
                    },
                );
                let hint = t(
                    app.lang,
                    if app.config.passwordless_sudo {
                        "opt.sudo_nopass_hint"
                    } else {
                        "opt.sudo_pass_hint"
                    },
                );
                draw_choice_row(f, area, focused, &t(app.lang, "opt.sudo"), &val, &hint);
            }
            Row::Escalation => {
                let val = t(
                    app.lang,
                    if app.config.use_doas {
                        "opt.escalation_doas"
                    } else {
                        "opt.escalation_sudo"
                    },
                );
                let hint = t(
                    app.lang,
                    if app.config.use_doas {
                        "opt.escalation_doas_hint"
                    } else {
                        "opt.escalation_sudo_hint"
                    },
                );
                draw_choice_row(
                    f,
                    area,
                    focused,
                    &t(app.lang, "opt.escalation"),
                    &val,
                    &hint,
                );
            }
            Row::Zswap => {
                let val = t(
                    app.lang,
                    if app.config.zswap {
                        "opt.on"
                    } else {
                        "opt.off"
                    },
                );
                draw_choice_row(
                    f,
                    area,
                    focused,
                    &t(app.lang, "opt.zswap"),
                    &val,
                    &t(app.lang, "opt.zswap_hint"),
                );
            }
            Row::ZswapCompressor => {
                // Mark the ones this kernel actually reports. It is a hint, not
                // a filter: /proc/crypto lists what is REGISTERED, and a module
                // nobody has used yet is simply absent from it.
                let seen = crate::system::mem::available_compressors();
                // The hint follows the SELECTED algorithm, the way the filesystem
                // rows do. One sentence covering all five said only "zstd is best,
                // lzo-rle is lighter", which does not answer the question anyone
                // arriving here actually has: what does THIS one cost me.
                let mut hint = t(app.lang, algo_hint_key(&app.config.zswap_compressor));
                if !seen.contains(&app.config.zswap_compressor) {
                    // This used to be a bare " (?)" appended to the value, which
                    // read as "something is wrong with this choice". It isn't:
                    // /proc/crypto lists what is REGISTERED, and crypto modules
                    // register on first use, so an algorithm nobody has asked for
                    // yet is simply absent. Say that instead of punctuating it.
                    hint.push(' ');
                    hint.push_str(&t(app.lang, "opt.zswap_algo_unseen"));
                }
                draw_choice_row(
                    f,
                    area,
                    focused,
                    &t(app.lang, "opt.zswap_algo"),
                    &app.config.zswap_compressor,
                    &hint,
                );
            }
            Row::ZswapPercent => {
                draw_choice_row(
                    f,
                    area,
                    focused,
                    &t(app.lang, "opt.zswap_pct"),
                    &format!("{} %", app.config.zswap_percent),
                    &t(app.lang, "opt.zswap_pct_hint"),
                );
            }
            Row::EarlyOom => {
                let val = t(
                    app.lang,
                    if app.config.earlyoom {
                        "opt.on"
                    } else {
                        "opt.off"
                    },
                );
                draw_choice_row(
                    f,
                    area,
                    focused,
                    &t(app.lang, "opt.earlyoom"),
                    &val,
                    &t(app.lang, "opt.earlyoom_hint"),
                );
            }
            Row::EarlyOomPercent => {
                draw_choice_row(
                    f,
                    area,
                    focused,
                    &t(app.lang, "opt.earlyoom_pct"),
                    &format!("{} %", app.config.earlyoom_percent),
                    &t(app.lang, "opt.earlyoom_pct_hint"),
                );
            }
            Row::Auris => {
                let val = t(
                    app.lang,
                    if app.config.auris {
                        "opt.on"
                    } else {
                        "opt.off"
                    },
                );
                draw_choice_row(
                    f,
                    area,
                    focused,
                    &t(app.lang, "opt.auris"),
                    &val,
                    &t(app.lang, "opt.auris_hint"),
                );
            }
            Row::Chaotic => {
                let val = t(
                    app.lang,
                    if app.config.chaotic_aur {
                        "opt.on"
                    } else {
                        "opt.off"
                    },
                );
                draw_choice_row(
                    f,
                    area,
                    focused,
                    &t(app.lang, "opt.chaotic"),
                    &val,
                    &t(app.lang, "opt.chaotic_hint"),
                );
            }
            Row::Mirrors => {
                let val = t(
                    app.lang,
                    if app.config.optimize_mirrors {
                        "opt.on"
                    } else {
                        "opt.off"
                    },
                );
                draw_choice_row(
                    f,
                    area,
                    focused,
                    &t(app.lang, "opt.mirrors"),
                    &val,
                    &t(app.lang, "opt.mirrors_hint"),
                );
            }
            Row::Encrypt => {
                let val = t(
                    app.lang,
                    if app.config.encrypt_disk {
                        "opt.on"
                    } else {
                        "opt.off"
                    },
                );
                draw_choice_row(
                    f,
                    area,
                    focused,
                    &t(app.lang, "opt.encrypt"),
                    &val,
                    &t(app.lang, "opt.encrypt_hint"),
                );
            }
            Row::EncBlocked => {
                // A dimmed, non-interactive explanation in the encryption slot.
                f.render_widget(
                    Paragraph::new(vec![
                        Line::from(Span::styled(
                            format!("  {}", t(app.lang, "opt.encrypt_shared")),
                            theme::mute(),
                        )),
                        Line::from(Span::styled(
                            format!("  {}", t(app.lang, "opt.encrypt_shared_hint")),
                            theme::dim(),
                        )),
                    ]),
                    area,
                );
            }
            Row::OsProber => {
                let val = t(
                    app.lang,
                    if app.config.os_prober {
                        "opt.on"
                    } else {
                        "opt.off"
                    },
                );
                draw_choice_row(
                    f,
                    area,
                    focused,
                    &t(app.lang, "opt.osprober"),
                    &val,
                    &t(app.lang, "opt.osprober_hint"),
                );
            }
            Row::SecureBoot => {
                let val = t(
                    app.lang,
                    if app.config.prepare_secureboot {
                        "opt.on"
                    } else {
                        "opt.off"
                    },
                );
                draw_choice_row(
                    f,
                    area,
                    focused,
                    &t(app.lang, "opt.secureboot"),
                    &val,
                    &t(app.lang, "opt.secureboot_hint"),
                );
            }
            Row::EncScope => {
                let val = t(
                    app.lang,
                    if app.config.encrypt_scope == "full" {
                        "opt.scope_full"
                    } else {
                        "opt.scope_root"
                    },
                );
                draw_choice_row(
                    f,
                    area,
                    focused,
                    &t(app.lang, "opt.scope"),
                    &val,
                    &t(app.lang, "opt.scope_hint"),
                );
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
                    draw_choice_row(
                        f,
                        area,
                        focused,
                        &t(app.lang, "opt.usbkey"),
                        &val,
                        &t(app.lang, "opt.usbkey_hint"),
                    );
                } else {
                    let marker = if focused { "›" } else { " " };
                    let line = Line::from(vec![
                        Span::styled(format!("  {marker} "), theme::gold()),
                        Span::styled(
                            format!("{}: ", t(app.lang, "opt.usbkey")),
                            if focused {
                                theme::gold()
                            } else {
                                theme::normal()
                            },
                        ),
                        Span::styled(
                            format!("‹ {val} ›"),
                            if focused {
                                theme::gold()
                            } else {
                                theme::mute()
                            },
                        ),
                    ]);
                    let hint = Line::from(Span::styled(
                        format!("      {}", t(app.lang, "opt.usbkey_warn")),
                        theme::warn(),
                    ));
                    f.render_widget(
                        Paragraph::new(vec![line, hint])
                            .wrap(ratatui::widgets::Wrap { trim: true }),
                        area,
                    );
                }
            }
            Row::UsbMode => {
                let key_only = app.config.usb_key_only;
                let val = t(
                    app.lang,
                    if key_only {
                        "opt.usbmode_only"
                    } else {
                        "opt.usbmode_backup"
                    },
                );
                if key_only {
                    // Key-only deserves a PERMANENT red warning, not a dim hint.
                    let marker = if focused { "\u{203a}" } else { " " };
                    let line = Line::from(vec![
                        Span::styled(format!("  {marker} "), theme::gold()),
                        Span::styled(
                            format!("{}: ", t(app.lang, "opt.usbmode")),
                            if focused {
                                theme::gold()
                            } else {
                                theme::normal()
                            },
                        ),
                        Span::styled(
                            format!("\u{2039} {val} \u{203a}"),
                            if focused {
                                theme::gold()
                            } else {
                                theme::mute()
                            },
                        ),
                    ]);
                    let hint = Line::from(Span::styled(
                        format!("      {}", t(app.lang, "opt.usbmode_only_warn")),
                        theme::warn(),
                    ));
                    f.render_widget(
                        Paragraph::new(vec![line, hint])
                            .wrap(ratatui::widgets::Wrap { trim: true }),
                        area,
                    );
                } else {
                    draw_choice_row(
                        f,
                        area,
                        focused,
                        &t(app.lang, "opt.usbmode"),
                        &val,
                        &t(app.lang, "opt.usbmode_hint"),
                    );
                }
            }
            Row::EncPass => {
                // Masked passphrase text field. The whole focused line shares
                // ONE intensity (gold = bold accent): mixing bold and non-bold
                // spans triggers the VT's unreliable intensity-reset handling
                // on incremental redraws (stale-bright first • while typing).
                let caret = if focused { "|" } else { "" };
                // Revealed on request: the passphrase typed here is the one
                // the boot prompt will demand, on a layout the user may have
                // just changed. A typo here is found at the next boot, with
                // nothing left to fix it with.
                let masked: String = if app.show_secrets {
                    app.config.luks_passphrase.clone()
                } else {
                    "•".repeat(app.config.luks_passphrase.chars().count())
                };
                let line = Line::from(vec![
                    Span::styled(
                        format!("  {} ", if focused { "›" } else { " " }),
                        theme::gold(),
                    ),
                    Span::styled(
                        format!("{}: ", t(app.lang, "opt.passphrase")),
                        if focused {
                            theme::gold()
                        } else {
                            theme::normal()
                        },
                    ),
                    Span::styled(
                        format!("[ {masked}{caret} ]"),
                        if focused {
                            theme::gold()
                        } else {
                            theme::mute()
                        },
                    ),
                ]);
                let hint = Line::from(Span::styled(
                    format!("      {}", t(app.lang, "opt.passphrase_hint")),
                    theme::dim(),
                ));
                f.render_widget(
                    Paragraph::new(vec![line, hint]).wrap(ratatui::widgets::Wrap { trim: true }),
                    area,
                );
            }
            // The heading and the closing note are TEXT, not controls: they
            // group the checkboxes below so they do not read as more settings
            // belonging to the root's own encryption.
            Row::EncExtraHead => {
                f.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        format!("  {}", t(app.lang, "opt.enc_extra_head")),
                        theme::heading(),
                    ))),
                    area,
                );
            }
            Row::EncExtraNote => {
                f.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        format!("      {}", t(app.lang, "opt.enc_extra_note")),
                        theme::dim(),
                    )))
                    .wrap(ratatui::widgets::Wrap { trim: true }),
                    area,
                );
            }
            Row::EncHome | Row::EncData(_) => {
                let (dev, label, on) = match row {
                    Row::EncHome => {
                        let (dev, disk) = enc_home_target(app).unwrap_or_default();
                        (
                            dev,
                            format!("/home  ({} {})", disk, t(app.lang, "opt.enc_own_disk")),
                            app.config.manual_home_encrypt,
                        )
                    }
                    _ => {
                        let i = if let Row::EncData(i) = row { *i } else { 0 };
                        let t2 = enc_data_targets(app);
                        let (dev, mp) = t2.get(i).cloned().unwrap_or_default();
                        let on = app
                            .config
                            .extra_disks
                            .iter()
                            .any(|d| d.disk == dev && d.encrypt);
                        (dev.clone(), mp, on)
                    }
                };
                let marker = if focused { "\u{203a}" } else { " " };
                let box_ = if on { "[x]" } else { "[ ]" };
                f.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(format!("  {marker} "), theme::gold()),
                        Span::styled(
                            format!("{box_} "),
                            if on { theme::gold() } else { theme::mute() },
                        ),
                        Span::styled(
                            format!("{label:<28}"),
                            if focused {
                                theme::gold()
                            } else {
                                theme::normal()
                            },
                        ),
                        Span::styled(dev, theme::dim()),
                    ])),
                    area,
                );
            }
            Row::Bootloader => {
                let val = app.config.bootloader.display_name();
                // Warn only about the one incompatibility the bootloader choice
                // can hit: an encrypted /boot needs GRUB. EFISTUB is fine with
                // snapshot rollback, so no extra warning there.
                let hint_key = if app.config.encrypt_disk
                    && app.config.encrypt_scope == "full"
                    && app.config.bootloader != Bootloader::Grub
                {
                    "opt.bootloader_warn"
                } else if app.config.bootloader == Bootloader::Efistub {
                    "opt.bootloader_efistub_hint"
                } else {
                    "opt.bootloader_hint"
                };
                draw_choice_row(
                    f,
                    area,
                    focused,
                    &t(app.lang, "opt.bootloader"),
                    val,
                    &t(app.lang, hint_key),
                );
            }
            Row::BootId => {
                let caret = if focused { "|" } else { "" };
                let line = Line::from(vec![
                    Span::styled(
                        format!("  {} ", if focused { "›" } else { " " }),
                        theme::gold(),
                    ),
                    Span::styled(
                        format!("{}: ", t(app.lang, "opt.bootid")),
                        if focused {
                            theme::gold()
                        } else {
                            theme::normal()
                        },
                    ),
                    Span::styled(
                        format!("[ {}{} ]", app.config.bootloader_id, caret),
                        if focused {
                            theme::gold()
                        } else {
                            theme::mute()
                        },
                    ),
                ]);
                let hint = Line::from(Span::styled(
                    format!("      {}", t(app.lang, "opt.bootid_hint")),
                    theme::dim(),
                ));
                f.render_widget(
                    Paragraph::new(vec![line, hint]).wrap(ratatui::widgets::Wrap { trim: true }),
                    area,
                );
            }
        }
    }

    // Can only advance if encryption-off, or encryption-on with a passphrase.
    // Also block the one incompatible combo: full-disk encryption (encrypted
    // /boot) only works with GRUB, since rEFInd/Limine/EFISTUB can't decrypt
    // /boot. (EFISTUB, unlike the earlier UKI attempt, IS compatible with
    // snapshot rollback, so there's no rollback gate.)
    let enc_ok = !app.config.encrypt_disk
        || !app.config.luks_passphrase.is_empty()
        || (app.config.usb_key_only && !app.config.usb_key_device.is_empty());
    let boot_ok = !(app.config.encrypt_disk
        && app.config.encrypt_scope == "full"
        && app.config.bootloader != Bootloader::Grub);
    app.can_advance = enc_ok && boot_ok;
    widgets::action_row(
        f,
        actions_area,
        &t(app.lang, "app.back"),
        &t(app.lang, "app.next"),
        app.can_advance,
    );
}

fn draw_choice_row(f: &mut Frame, area: Rect, focused: bool, label: &str, value: &str, hint: &str) {
    let marker = if focused { "›" } else { " " };
    let label_style = if focused {
        theme::gold()
    } else {
        theme::normal()
    };
    // The focused value GLOWS bright bold cyan — same family as the rest of
    // the UI. (A reversed-video fill was tried here and looked muddy on real
    // fbcon palettes: grey text on a dark-cyan slab. Plain bright text wins.)
    let value_style = if focused {
        theme::gold()
    } else {
        theme::mute()
    };
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

    // Up/Down navigation is common to all rows. The EncBlocked note is inert,
    // so navigation steps over it (landing there would look like a lost cursor).
    // The heading and the closing note of the extra-encryption block are text,
    // like EncBlocked: landing on either would look like a lost cursor.
    let is_inert = |i: usize| {
        matches!(
            visible.get(i),
            Some(&Row::EncBlocked) | Some(&Row::EncExtraHead) | Some(&Row::EncExtraNote)
        )
    };
    match key.code {
        KeyCode::Up | KeyCode::Esc => {
            let mut n = app.cursor.saturating_sub(1);
            if is_inert(n) {
                n = n.saturating_sub(1);
            }
            app.cursor = n;
            return;
        }
        KeyCode::Down => {
            let mut n = (app.cursor + 1).min(visible.len() - 1);
            if is_inert(n) {
                n = (n + 1).min(visible.len() - 1);
            }
            app.cursor = n;
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
        && app.config.bootloader != Bootloader::Grub
    {
        return;
    }
    app.goto_next();
}

fn toggle(app: &mut App, row: Row, forward: bool) {
    match row {
        Row::Zswap => app.config.zswap = !app.config.zswap,
        Row::EarlyOom => app.config.earlyoom = !app.config.earlyoom,
        Row::ZswapCompressor => {
            let list = crate::system::mem::COMPRESSORS;
            let i = list
                .iter()
                .position(|x| *x == app.config.zswap_compressor)
                .unwrap_or(0);
            let next = if forward {
                (i + 1) % list.len()
            } else {
                (i + list.len() - 1) % list.len()
            };
            app.config.zswap_compressor = list[next].to_string();
        }
        // Percentages step in fives: the difference between 20 and 21 is not
        // something anyone can feel, and one press per point would be cruel.
        Row::ZswapPercent => {
            let v = app.config.zswap_percent as i16 + if forward { 5 } else { -5 };
            app.config.zswap_percent = v.clamp(5, 50) as u8;
        }
        Row::EarlyOomPercent => {
            let v = app.config.earlyoom_percent as i16 + if forward { 2 } else { -2 };
            app.config.earlyoom_percent = v.clamp(2, 30) as u8;
        }
        Row::Sudo => app.config.passwordless_sudo = !app.config.passwordless_sudo,
        Row::Escalation => app.config.use_doas = !app.config.use_doas,
        Row::Chaotic => app.config.chaotic_aur = !app.config.chaotic_aur,
        Row::Auris => app.config.auris = !app.config.auris,
        Row::OsProber => app.config.os_prober = !app.config.os_prober,
        Row::SecureBoot => app.config.prepare_secureboot = !app.config.prepare_secureboot,
        Row::Mirrors => app.config.optimize_mirrors = !app.config.optimize_mirrors,
        Row::Encrypt => app.config.encrypt_disk = !app.config.encrypt_disk,
        // Extra targets: one flag each, both living where the root's own
        // encryption is decided. Setting them in the partition editor put the
        // same word on two screens for two different mechanisms, and made the
        // system disk look unencryptable there because it is not "data".
        Row::EncHome => app.config.manual_home_encrypt = !app.config.manual_home_encrypt,
        Row::EncData(i) => {
            if let Some((dev, _)) = enc_data_targets(app).get(i).cloned() {
                // Through the shared toggle, which refuses a partition that
                // is being kept as-is: encrypting one would destroy the data it
                // is being kept for.
                crate::screens::parts::toggle_data_encryption(&mut app.config, &dev);
            }
        }
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
            // Picking a stick as the key retracts any plan to use it as an
            // ordinary extra disk. The Additional-disks screen hides the key
            // stick, but a mountpoint chosen BEFORE the key was picked would
            // still be sitting in extra_disks — and build_plan reads that list
            // directly, so it would format the stick out from under the key.
            // Drop the entry (and anything on its partitions) here, where the
            // decision is actually made.
            if !app.config.usb_key_device.is_empty() {
                let key = app.config.usb_key_device.clone();
                app.config
                    .extra_disks
                    .retain(|d| d.disk != key && !d.disk.starts_with(&key));
            }
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
            if app.config.bootloader == Bootloader::Grub {
                app.config.encrypt_scope = if app.config.encrypt_scope == "full" {
                    "root".into()
                } else {
                    "full".into()
                };
                // Encrypted /boot means GRUB prompts before the initramfs ever
                // runs — the USB auto-unlock key would be pointless, so the
                // two options are mutually exclusive. Clear the whole USB-key
                // state (device AND key-only mode) so no stale flag survives
                // invisibly under full-disk, where the rows aren't shown.
                if app.config.encrypt_scope == "full" {
                    app.config.usb_key_device.clear();
                    app.config.usb_key_only = false;
                }
            } else {
                app.config.encrypt_scope = "root".into();
            }
        }
        Row::Bootloader => {
            // Cycle grub ↔ refind ↔ limine ↔ efistub in BOTH directions:
            // Right advances, Left reverses. EFISTUB boots the kernel directly
            // via a UEFI boot entry (Artix kernels are already EFI stubs — no
            // extra package, no systemd, unlike UKI which needs systemd-stub).
            // It's UEFI-only and, like rEFInd/Limine, cannot decrypt /boot.
            // Unlike UKI, EFISTUB IS compatible with snapshot rollback: kernel,
            // initramfs and cmdline stay separate files, so we register extra
            // UEFI entries for the rescue pair (rollback + rescue), mirroring
            // the GRUB/rEFInd/Limine flow.
            // The cycle order shown on this screen. Typed, so adding a variant
            // to Bootloader without adding it here is a compile error rather
            // than a bootloader silently missing from the picker.
            const ORDER: [Bootloader; 4] = [
                Bootloader::Grub,
                Bootloader::Refind,
                Bootloader::Limine,
                Bootloader::Efistub,
            ];
            let i = ORDER
                .iter()
                .position(|b| *b == app.config.bootloader)
                .unwrap_or(0);
            let n = if forward {
                (i + 1) % ORDER.len()
            } else {
                (i + ORDER.len() - 1) % ORDER.len()
            };
            app.config.bootloader = ORDER[n];
            // If we moved away from GRUB, an encrypted /boot is no longer
            // possible, so force the scope back to root-only.
            if app.config.bootloader != Bootloader::Grub && app.config.encrypt_scope == "full" {
                app.config.encrypt_scope = "root".into();
            }
            // Secure Boot prep is EFISTUB-only; if we moved off EFISTUB, drop it
            // so a hidden flag can't linger from a previous choice.
            if app.config.bootloader != Bootloader::Efistub {
                app.config.prepare_secureboot = false;
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

/// Which per-algorithm explanation belongs to a compressor name.
///
/// Falls back to zstd's line for anything unrecognised, so adding a compressor
/// to `mem::COMPRESSORS` without a string cannot leave a blank hint on screen.
fn algo_hint_key(name: &str) -> &'static str {
    match name {
        "lzo-rle" => "opt.zswap_algo_lzorle",
        "lzo" => "opt.zswap_algo_lzo",
        "lz4" => "opt.zswap_algo_lz4",
        "deflate" => "opt.zswap_algo_deflate",
        _ => "opt.zswap_algo_zstd",
    }
}

pub fn footer_hint(app: &App) -> String {
    let base = t(app.lang, "opt.footer");
    // The LUKS passphrase is the one string that must be typed again at every
    // boot, on a layout chosen during this install. Being able to LOOK at it
    // matters more here than anywhere else in the installer — so say so.
    //
    // ONLY on the passphrase row. It used to be advertised for the whole screen
    // whenever encryption was on, so "F2 show password" sat under the bootloader
    // row, the os-prober row and the UEFI-entry row — none of which have a
    // password, and pressing F2 there does nothing anyone can see.
    let rows = rows_for(app);
    let on_passphrase = rows
        .get(app.cursor.min(rows.len().saturating_sub(1)))
        .is_some_and(|r| *r == Row::EncPass);
    if on_passphrase {
        let key = if app.show_secrets {
            t(app.lang, "user.reveal_hide")
        } else {
            t(app.lang, "user.reveal_show")
        };
        format!("{base} · {key}")
    } else {
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, PartitionMode, Screen};

    /// zswap caches pages on their way to swap. With no swap there is nothing
    /// behind the cache, so the row is not offered at all.
    ///
    /// The trap this guards is `swap_gib`: it defaults to 4 and the manual
    /// editor never touches it, so asking the auto field about a manual layout
    /// answers "4 GiB of swap" for a disk that has none. Reported from a manual
    /// Artix-only install that was offered a compression algorithm for a cache
    /// that could never hold a page.
    #[test]
    fn zswap_is_not_offered_when_there_is_no_swap() {
        let mut app = App::new();
        app.screen = Screen::Options;

        // Auto with the default 4 GiB of swap: offered.
        assert!(rows_for(&app).contains(&Row::Zswap));

        // Auto, swap turned off on the disk step: gone.
        app.config.swap_gib = 0;
        assert!(!rows_for(&app).contains(&Row::Zswap));

        // Manual with no swap partition: gone, even though swap_gib is back to
        // its default — the manual editor writes manual_swap, not swap_gib.
        app.config.swap_gib = 4;
        app.config.partition_mode = PartitionMode::Manual;
        assert!(
            !rows_for(&app).contains(&Row::Zswap),
            "a manual layout with no swap partition was offered zswap"
        );

        // Manual WITH a swap partition: offered again.
        app.config.manual_swap = "/dev/vda3".into();
        assert!(rows_for(&app).contains(&Row::Zswap));

        // And a swap partition that is only planned counts too.
        app.config.manual_swap.clear();
        app.config.manual_swap_new_mib = 4096;
        assert!(rows_for(&app).contains(&Row::Zswap));
    }

    /// The reveal key is advertised on the row that has a password, and nowhere
    /// else. It used to be advertised for the whole screen whenever encryption
    /// was on, so it followed the cursor onto rows where F2 does nothing.
    #[test]
    fn the_reveal_hint_belongs_to_the_passphrase_row_only() {
        let mut app = App::new();
        app.screen = Screen::Security;
        app.config.encrypt_disk = true;
        let rows = rows_for(&app);
        let pass = rows
            .iter()
            .position(|r| *r == Row::EncPass)
            .expect("no passphrase row with encryption on");

        app.cursor = pass;
        assert!(
            footer_hint(&app).contains(&t(app.lang, "user.reveal_show")),
            "the passphrase row does not advertise the reveal key"
        );

        for (i, row) in rows.iter().enumerate() {
            if *row == Row::EncPass {
                continue;
            }
            app.cursor = i;
            assert!(
                !footer_hint(&app).contains(&t(app.lang, "user.reveal_show")),
                "{row:?} is not a password field but advertises F2"
            );
        }
    }

    /// Every compressor `mem::COMPRESSORS` offers has an explanation of its own.
    /// A missing one would silently fall back to zstd's line, describing the
    /// wrong algorithm rather than none at all.
    #[test]
    fn every_compressor_explains_itself() {
        for lang in [
            crate::i18n::Lang::Uk,
            crate::i18n::Lang::En,
            crate::i18n::Lang::Es,
        ] {
            let mut seen: Vec<String> = Vec::new();
            for name in crate::system::mem::COMPRESSORS {
                let text = t(lang, algo_hint_key(name));
                assert!(
                    !text.starts_with("opt."),
                    "{name}: no description in {lang:?}"
                );
                assert!(
                    !seen.contains(&text),
                    "{name} in {lang:?} reuses another algorithm's description"
                );
                seen.push(text);
            }
        }
    }
}
