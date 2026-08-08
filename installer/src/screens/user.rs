//! Step 9 — accounts. The user picks an *account mode* (↑↓ to move between
//! fields, ←→ to switch the mode) and fills only the fields that mode needs:
//!
//!   • User + separate root password
//!   • User + root shares the user's password
//!   • User + root disabled (sudo-only via the wheel group)
//!   • Root only (no user; just a root password)
//!
//! Passwords live only in `App` and are never serialized to disk.

use crate::app::{AccountMode, App};
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

pub fn mode(app: &App) -> AccountMode {
    app.config.account_mode
}

fn mode_label(app: &App, m: AccountMode) -> String {
    let key = match m {
        AccountMode::UserSeparateRoot => "user.mode_sep",
        AccountMode::UserSameRoot => "user.mode_same",
        AccountMode::UserSudoOnly => "user.mode_sudo",
        AccountMode::RootOnly => "user.mode_root",
    };
    t(app.lang, key)
}

/// Build the ordered list of focusable fields for the current mode.
/// Field 0 is always the mode selector.
#[derive(Clone, Copy, PartialEq)]
enum Field {
    Mode,
    Hostname,
    Username,
    UserPass,
    UserConfirm,
    RootPass,
    RootConfirm,
}

fn fields(app: &App) -> Vec<Field> {
    let mut v = vec![Field::Mode, Field::Hostname];
    let m = mode(app);
    if m.needs_user() {
        v.push(Field::Username);
        v.push(Field::UserPass);
        v.push(Field::UserConfirm);
    }
    if m.needs_separate_root() {
        v.push(Field::RootPass);
        v.push(Field::RootConfirm);
    }
    v
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let flds = fields(app);
    if app.user_focus >= flds.len() {
        app.user_focus = flds.len() - 1;
    }

    // A boxed field (label + 3-row bordered input) is 4 rows; the mode selector
    // is a 3-row box; with spacing, even the 5-field default mode needs ~28 rows
    // and the 7-field separate-root mode ~38. The content panel on any normal
    // console (80x24 → ~15 rows, even 100x30 → ~21) is far smaller, so the boxed
    // form clips its inputs into borderless stubs. Below a tall threshold, render
    // each field as a single inline line with no spacing — the whole form fits.
    let compact = area.height < 26;
    let mode_h = if compact { 1 } else { 3 };
    let field_h = if compact { 1 } else { 4 };

    let mut constraints = vec![Constraint::Length(mode_h)]; // mode
    for fld in flds.iter().skip(1) {
        let _ = fld;
        constraints.push(Constraint::Length(field_h));
    }
    constraints.push(Constraint::Min(0)); // status
    constraints.push(Constraint::Length(3)); // actions
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .spacing(if compact { 0 } else { 1 })
        .split(area);

    let cur = flds[app.user_focus];

    // Row 0: mode selector (segmented over two lines is awkward; use a single
    // highlighted line showing the chosen mode + hint that ←→ changes it).
    let mode_style = if cur == Field::Mode {
        theme::selected()
    } else {
        theme::normal()
    };
    // Compact drops the "Account settings" prefix: the longest mode label plus
    // that prefix overruns a 59-col panel, and the mode values already read as
    // full sentences (the footer explains ←/→ switches them).
    let mode_line = if compact {
        Line::from(vec![
            Span::styled(
                if cur == Field::Mode {
                    "\u{203a} "
                } else {
                    "  "
                },
                theme::accent(),
            ),
            Span::styled(format!("‹ {} ›", mode_label(app, mode(app))), mode_style),
        ])
    } else {
        Line::from(vec![
            Span::styled(format!("  {}  ", t(app.lang, "user.mode")), theme::dim()),
            Span::styled(format!("‹ {} ›", mode_label(app, mode(app))), mode_style),
        ])
    };
    let mode_para = Paragraph::new(mode_line);
    // The rounded frame only when there's room; compact drops it to one line.
    f.render_widget(
        if compact {
            mode_para
        } else {
            mode_para.block(theme::box_rounded())
        },
        rows[0],
    );

    // Subsequent rows: render each field in order. enumerate() gives the row
    // index; offset by 1 because row 0 is the account-mode picker above.
    for (ri, fld) in flds.iter().enumerate().skip(1) {
        let focused = *fld == cur;
        let (label, value, mask): (String, &str, bool) = match fld {
            Field::Hostname => (
                t(app.lang, "user.hostname"),
                app.config.hostname.as_str(),
                false,
            ),
            Field::Username => (
                t(app.lang, "user.username"),
                app.config.username.as_str(),
                false,
            ),
            Field::UserPass => (
                t(app.lang, "user.password"),
                app.config.user_password.as_str(),
                true,
            ),
            Field::UserConfirm => (t(app.lang, "user.confirm"), app.user_confirm.as_str(), true),
            Field::RootPass => (
                t(app.lang, "user.root_password"),
                app.config.root_password.as_str(),
                true,
            ),
            Field::RootConfirm => (
                t(app.lang, "user.root_confirm"),
                app.root_confirm.as_str(),
                true,
            ),
            Field::Mode => continue,
        };
        if compact {
            widgets::input_inline(
                f,
                rows[ri],
                &label,
                value,
                focused,
                mask && !app.show_secrets,
            );
        } else {
            widgets::input(
                f,
                rows[ri],
                &label,
                value,
                focused,
                mask && !app.show_secrets,
            );
        }
    }

    // Status line.
    let (msg, ok) = validate(app);
    let status_idx = rows.len() - 2;
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("  {msg}"),
            if ok { theme::ok() } else { theme::warn() },
        ))),
        rows[status_idx],
    );

    app.can_advance = ok;
    let act_idx = rows.len() - 1;
    widgets::action_row(
        f,
        rows[act_idx],
        &t(app.lang, "app.back"),
        &t(app.lang, "app.next"),
        ok,
    );
}

/// Returns (message, is_valid).
fn validate(app: &App) -> (String, bool) {
    let m = mode(app);
    if m.needs_user() {
        if app.config.username.is_empty() {
            return (t(app.lang, "user.need_name"), false);
        }
        if !valid_username(&app.config.username) {
            return (t(app.lang, "user.bad_name"), false);
        }
        if app.config.user_password.is_empty() {
            return (t(app.lang, "user.need_pass"), false);
        }
        if app.config.user_password != app.user_confirm {
            return (t(app.lang, "user.mismatch"), false);
        }
    }
    if m.needs_separate_root() {
        if app.config.root_password.is_empty() {
            return (t(app.lang, "user.need_root"), false);
        }
        if app.config.root_password != app.root_confirm {
            return (t(app.lang, "user.root_mismatch"), false);
        }
    }
    (t(app.lang, "user.ok"), true)
}

fn valid_username(name: &str) -> bool {
    // lowercase start, then lowercase/digits/_/-, max 32.
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    let flds = fields(app);
    let cur = flds[app.user_focus.min(flds.len() - 1)];

    match key.code {
        KeyCode::Down | KeyCode::Tab => {
            app.user_focus = (app.user_focus + 1).min(flds.len() - 1);
            return;
        }
        KeyCode::Up | KeyCode::BackTab => {
            app.user_focus = app.user_focus.saturating_sub(1);
            return;
        }
        KeyCode::Esc => {
            // Step to the previous field; first-field Esc is intercepted by the
            // global handler to leave the screen.
            app.user_focus = app.user_focus.saturating_sub(1);
            return;
        }
        KeyCode::Enter => {
            // Enter steps to the next field; on the LAST field it finalises and
            // advances (matching the installer-wide convention). If validation
            // fails on the last field, stay put and let the inline error show.
            if app.user_focus + 1 < flds.len() {
                app.user_focus += 1;
            } else {
                let (_, ok) = validate(app);
                if ok {
                    finalize(app);
                    app.goto_next();
                }
            }
            return;
        }
        _ => {}
    }

    if cur == Field::Mode {
        // ←→ cycle the account mode.
        let idx = AccountMode::ALL
            .iter()
            .position(|m| *m == mode(app))
            .unwrap_or(0);
        let n = AccountMode::ALL.len();
        let new = match key.code {
            KeyCode::Left => AccountMode::ALL[(idx + n - 1) % n],
            KeyCode::Right => AccountMode::ALL[(idx + 1) % n],
            _ => return,
        };
        app.config.account_mode = new;
        app.user_focus = 0;
        return;
    }

    // Text editing for the focused field.
    let target: Option<&mut String> = match cur {
        Field::Hostname => Some(&mut app.config.hostname),
        Field::Username => Some(&mut app.config.username),
        Field::UserPass => Some(&mut app.config.user_password),
        Field::UserConfirm => Some(&mut app.user_confirm),
        Field::RootPass => Some(&mut app.config.root_password),
        Field::RootConfirm => Some(&mut app.root_confirm),
        Field::Mode => None,
    };
    if let Some(s) = target {
        match key.code {
            KeyCode::Char(c) => {
                // Hostnames are limited to letters, digits and hyphens (RFC
                // 1123). For other fields accept any character.
                if cur == Field::Hostname {
                    if (c.is_ascii_alphanumeric() || c == '-') && s.chars().count() < 63 {
                        s.push(c.to_ascii_lowercase());
                    }
                } else {
                    s.push(c);
                }
            }
            KeyCode::Backspace => {
                s.pop();
            }
            _ => {}
        }
    }
}

/// Reconcile derived fields before leaving (root_same_as_user etc.).
fn finalize(app: &mut App) {
    match mode(app) {
        AccountMode::UserSameRoot => {
            app.config.root_password = app.config.user_password.clone();
            app.config.root_same_as_user = true;
        }
        AccountMode::UserSeparateRoot | AccountMode::RootOnly => {
            app.config.root_same_as_user = false;
        }
        AccountMode::UserSudoOnly => {
            // root login disabled; clear any root password.
            app.config.root_password.clear();
            app.config.root_same_as_user = false;
        }
    }
}

pub fn footer_hint(app: &App) -> String {
    // The hint changes with the focused field: on the mode selector ←/→ switch
    // the account mode; on the text fields you just type. Enter always advances,
    // Esc/← always goes back.
    let flds = fields(app);
    let cur = flds
        .get(app.user_focus.min(flds.len().saturating_sub(1)))
        .copied();
    let base = match cur {
        Some(Field::Mode) => t(app.lang, "user.footer_mode"),
        _ => t(app.lang, "user.footer_field"),
    };
    // The reveal key is only worth advertising where there is a password to
    // reveal, and the label says what the NEXT press does.
    // Every field that is masked, confirmations included — you are just as
    // likely to mistype the second one, and that is the mismatch that stops you
    // from continuing with no way to see why.
    if matches!(
        cur,
        Some(Field::UserPass)
            | Some(Field::UserConfirm)
            | Some(Field::RootPass)
            | Some(Field::RootConfirm)
    ) {
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
