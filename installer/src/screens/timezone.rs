//! Step 2 — time zone. Full IANA list from chrono-tz with a live type-to-filter
//! search box, minus the excluded zones.

use crate::app::App;
use crate::i18n::t;
use crate::screens::widgets;
use crate::theme;
use chrono_tz::TZ_VARIANTS;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};
use std::sync::OnceLock;

/// Zones to drop entirely, plus the deprecated IANA aliases for Ukrainian
/// zones. IANA still keeps the old "Kiev" spelling and the collapsed
/// "Uzhgorod"/"Zaporozhye" zones as aliases of Europe/Kyiv — we hide all of
/// them and keep only the correct Europe/Kyiv.
fn is_excluded(name: &str) -> bool {
    const BLOCKED: &[&str] = &[
        // Deprecated Ukrainian aliases — keep only Europe/Kyiv.
        "Europe/Kiev",
        "Europe/Uzhgorod",
        "Europe/Zaporozhye",
        // Excluded zones.
        "Europe/Moscow",
        "Europe/Kaliningrad",
        "Europe/Samara",
        "Europe/Volgograd",
        "Europe/Saratov",
        "Europe/Astrakhan",
        "Europe/Ulyanovsk",
        "Europe/Kirov",
        "Asia/Yekaterinburg",
        "Asia/Omsk",
        "Asia/Novosibirsk",
        "Asia/Novokuznetsk",
        "Asia/Krasnoyarsk",
        "Asia/Barnaul",
        "Asia/Tomsk",
        "Asia/Irkutsk",
        "Asia/Chita",
        "Asia/Yakutsk",
        "Asia/Vladivostok",
        "Asia/Khandyga",
        "Asia/Ust-Nera",
        "Asia/Magadan",
        "Asia/Sakhalin",
        "Asia/Srednekolymsk",
        "Asia/Kamchatka",
        "Asia/Anadyr",
        "W-SU",
    ];
    BLOCKED.contains(&name)
}

fn all_zones() -> &'static Vec<String> {
    static Z: OnceLock<Vec<String>> = OnceLock::new();
    Z.get_or_init(|| {
        TZ_VARIANTS
            .iter()
            .map(|tz| tz.name().to_string())
            .filter(|n| !is_excluded(n))
            .collect()
    })
}

fn filtered(query: &str) -> Vec<String> {
    let q = query.to_lowercase();
    all_zones()
        .iter()
        .filter(|z| q.is_empty() || z.to_lowercase().contains(&q))
        .cloned()
        .collect()
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // search box
            Constraint::Min(0),    // list
            // THREE lines live here: the chosen zone, the clock switch, and the
            // sentence explaining what the switch is for. It was 2, so that
            // sentence — the one that says a machine with Windows beside it
            // needs local time or the clock jumps by the timezone offset — was
            // silently clipped on every console size there is. ratatui truncates
            // without a word, so nothing anywhere said it was missing.
            Constraint::Length(3), // hardware clock: value, switch, why
            Constraint::Length(3), // actions
        ])
        .spacing(1)
        .split(area);

    // Search box.
    let q = &app.tz_query;
    let search = Paragraph::new(Line::from(vec![
        Span::styled("  ", theme::dim()),
        Span::styled(
            if q.is_empty() {
                t(app.lang, "tz.hint")
            } else {
                q.clone()
            },
            if q.is_empty() {
                theme::mute()
            } else {
                theme::normal()
            },
        ),
        Span::styled("|", theme::accent()),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme::border())
            .title(format!(" {} ", t(app.lang, "app.search")))
            .title_style(theme::dim()),
    );
    f.render_widget(search, rows[0]);

    let list = filtered(q);
    let items: Vec<String> = list.iter().map(|z| format!("  {z}")).collect();
    if items.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled("  —", theme::mute()))),
            rows[1],
        );
        app.can_advance = false;
    } else {
        widgets::select_list_scrolled(f, rows[1], &items, app.cursor, app.marquee);
        app.can_advance = true;
        // NOT `app.config.timezone = list[cursor]` — see commit_choice().
        // Painting a screen must not decide anything.
    }

    // Hardware clock. The hidden GRUB menu used to ask this as `utc=`; it has to
    // live somewhere, and it belongs next to the timezone it interacts with.
    let (label, desc) = if app.config.rtc_utc {
        ("tz.clock_utc", "tz.clock_utc_desc")
    } else {
        ("tz.clock_local", "tz.clock_local_desc")
    };
    f.render_widget(
        Paragraph::new(vec![
            // What is CHOSEN, spelled out. The list opens at the top rather than
            // on the chosen zone, so without this line the choice would be
            // invisible — and a setting you cannot see is one you cannot trust
            // was kept.
            Line::from(vec![
                Span::styled(format!("  {}  ", t(app.lang, "tz.chosen")), theme::dim()),
                Span::styled(app.config.timezone.clone(), theme::accent()),
            ]),
            Line::from(vec![
                Span::styled(format!("  {}  ", t(app.lang, "tz.clock")), theme::dim()),
                Span::styled(format!("< {} >", t(app.lang, label)), theme::selected()),
            ]),
            Line::from(Span::styled(
                format!("  {}", t(app.lang, desc)),
                theme::mute(),
            )),
        ]),
        rows[2],
    );

    widgets::action_row(
        f,
        rows[3],
        &t(app.lang, "app.back"),
        &t(app.lang, "app.next"),
        app.can_advance,
    );
}

/// The zone a fresh install starts on: the first of the list, and nothing more
/// than that.
///
/// It used to be `Europe/Kyiv`, written into the config as a literal. That is
/// the author's zone, and this installer is trilingual and aimed at people
/// arriving from other distributions — a Spaniard was being handed Kyiv. It is
/// not only a clock, either: the mirror ranking takes the timezone as its hint
/// about where you are.
///
/// Derived rather than written down, so it cannot drift from the list it is
/// supposed to be the first of.
pub fn first_zone() -> &'static str {
    all_zones()
        .first()
        .map(String::as_str)
        .unwrap_or("Africa/Abidjan")
}

/// Where a given zone sits in the unfiltered list — used only by the tests now
/// that the list opens at the top rather than on the configured zone. Kept
/// because it is what those tests assert AGAINST: that the default really is
/// entry zero, and that nothing parks the cursor anywhere else.
#[cfg(test)]
/// Where a given zone sits in the unfiltered list — so the cursor can be parked
/// on the zone the config already holds, instead of on whatever sorts first.
pub fn index_of(zone: &str) -> Option<usize> {
    filtered("").iter().position(|z| z == zone)
}

/// Write the highlighted zone into the config.
///
/// This belongs to the KEY HANDLER, not to draw(). It used to sit inside
/// draw(), which meant every repaint re-derived the timezone from the cursor —
/// and on a fresh screen the cursor is 0, so the very first frame overwrote the
/// default (Europe/Kyiv) with whatever happens to sort first in the zone list
/// (Africa/Abidjan). The user's zone was gone before they touched a key.
///
/// Rendering shows state. It must never decide it.
fn commit_choice(app: &mut App) {
    let list = filtered(&app.tz_query);
    if let Some(zone) = list.get(app.cursor) {
        app.config.timezone = zone.clone();
    }
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    let len = filtered(&app.tz_query).len();
    // Movement is the shared nav component; a move re-commits, so the config
    // always matches what's highlighted on screen — and marks the list as USED,
    // which is what lets a bare Enter keep an earlier choice instead of
    // replacing it with whatever the top of the list happens to be.
    if super::nav::move_cursor(key.code, &mut app.cursor, len) {
        app.tz_touched = true;
        commit_choice(app);
        return;
    }
    match key.code {
        // ←/→ flips the hardware clock. Not a list row: it is a second question
        // about the same thing, and burying it in a 400-entry timezone list is
        // how it would never be found.
        KeyCode::Left | KeyCode::Right => app.config.rtc_utc = !app.config.rtc_utc,
        KeyCode::Char(c) => {
            app.tz_query.push(c);
            app.cursor = 0;
            app.tz_touched = true;
        }
        KeyCode::Backspace => {
            app.tz_query.pop();
            app.cursor = 0;
            app.tz_touched = true;
        }
        KeyCode::Enter if len > 0 => {
            // Commit BEFORE leaving: goto_next() is the last chance to record
            // what the cursor was pointing at — but ONLY if the list was used.
            // The screen opens at the top now, so an untouched Enter would
            // otherwise trade the zone you chose last time for Africa/Abidjan.
            if app.tz_touched {
                commit_choice(app);
            }
            app.goto_next();
            return;
        }
        _ => {}
    }
    // Every movement and every edit of the filter re-commits, so the config
    // always matches what's highlighted on screen.
    if app.tz_touched {
        commit_choice(app);
    }
}

pub fn footer_hint(app: &App) -> String {
    t(app.lang, "tz.footer")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::i18n::Lang;
    use ratatui::{backend::TestBackend, Terminal};

    /// THE CLOCK EXPLANATION MUST ACTUALLY BE ON SCREEN.
    ///
    /// Three lines are drawn into this area — the chosen zone, the UTC/local
    /// switch, and the sentence saying why the switch matters — and the area
    /// was two rows tall. ratatui truncates in silence, so the sentence was
    /// never visible on any console size, and nothing reported it. It is the
    /// one line that tells somebody with Windows on the same machine that
    /// their clock will otherwise jump by the timezone offset.
    #[test]
    fn the_hardware_clock_explanation_is_not_clipped_away() {
        for utc in [true, false] {
            let mut app = App::new();
            app.config.rtc_utc = utc;
            let mut term = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
            term.draw(|f| draw(f, &mut app, f.area())).expect("draw");
            let text: String = term
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect();
            // Looked up rather than written out: the assertion must follow the
            // translation, not a copy of it that can drift.
            let key = if utc {
                "tz.clock_utc_desc"
            } else {
                "tz.clock_local_desc"
            };
            let desc = crate::i18n::t(Lang::Uk, key);
            let needle: String = desc.chars().take(12).collect();
            assert!(
                text.contains(&needle),
                "the clock explanation is not on screen (rtc_utc={utc})"
            );
        }
    }

    use crate::app::Screen;
    use crossterm::event::KeyEvent;

    /// The list opens AT THE TOP with an empty filter, every single time.
    ///
    /// It used to open on the zone already configured, which sounds helpful and
    /// was not: the config moves as you move, so each re-entry landed somewhere
    /// else and it read as arbitrary. 567 entries are searched, not browsed.
    #[test]
    fn the_list_always_opens_at_the_top() {
        let mut app = App::new();
        app.config.timezone = "Europe/Madrid".into();
        app.cursor = 300;
        app.tz_query = "mad".into();
        app.goto(Screen::Timezone);
        assert_eq!(app.cursor, 0, "the list did not open at the top");
        assert!(app.tz_query.is_empty(), "a stale filter survived");
    }

    /// Walking through the step without touching the list KEEPS the zone chosen
    /// earlier. With the cursor at the top, a bare Enter would otherwise trade
    /// it for whatever sorts first — the same "it forgot my timezone" in a new
    /// costume.
    #[test]
    fn stepping_through_untouched_keeps_the_chosen_zone() {
        let mut app = App::new();
        app.goto(Screen::Timezone);
        app.config.timezone = "Europe/Madrid".into();
        app.goto(Screen::Language);
        app.goto(Screen::Timezone);
        assert_eq!(app.cursor, 0);
        handle_key(&mut app, KeyEvent::from(KeyCode::Enter));
        assert_eq!(
            app.config.timezone, "Europe/Madrid",
            "a bare Enter replaced the zone that was already chosen"
        );
    }

    /// And touching it DOES choose: a move commits what is highlighted.
    #[test]
    fn moving_the_cursor_chooses() {
        let mut app = App::new();
        app.config.timezone = "Europe/Madrid".into();
        app.goto(Screen::Timezone);
        handle_key(&mut app, KeyEvent::from(KeyCode::Down));
        assert_ne!(app.config.timezone, "Europe/Madrid");
        assert!(app.tz_touched);
    }

    /// The default names no country: it is the first of the list, derived from
    /// the list itself. It was `Europe/Kyiv` — the author's zone, handed to a
    /// Spaniard, and used as the hint for ranking mirrors as well.
    #[test]
    fn the_default_zone_is_simply_the_first_one() {
        let app = App::new();
        assert_eq!(app.config.timezone, first_zone());
        assert_eq!(index_of(&app.config.timezone), Some(0));
    }
}
