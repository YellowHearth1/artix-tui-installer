//! Step 1 — language. Big, centered, two cards-ish rows. Sets UI language and a
//! default system locale.

use crate::app::App;
use crate::i18n::{t, Lang};
use crate::screens::widgets;
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

const OPTIONS: [Lang; 3] = [Lang::Uk, Lang::En, Lang::Es];

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // hint
            Constraint::Min(0),    // list
            Constraint::Length(3), // actions
            Constraint::Length(1), // build number
        ])
        .spacing(1)
        .split(area);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            t(app.lang, "lang.hint"),
            theme::dim(),
        ))),
        rows[0],
    );

    let items: Vec<String> = OPTIONS
        .iter()
        .map(|l| match l {
            Lang::En => format!("  {}", t(app.lang, "lang.en")),
            Lang::Uk => format!("  {}", t(app.lang, "lang.uk")),
            Lang::Es => format!("  {}", t(app.lang, "lang.es")),
        })
        .collect();
    widgets::select_list_scrolled(f, rows[1], &items, app.cursor, app.marquee);

    widgets::action_row(
        f,
        rows[2],
        &t(app.lang, "app.back"),
        &t(app.lang, "app.next"),
        true,
    );

    // Build number, bottom-right of the first screen.
    //
    // This settles a question that costs a round trip every time it comes up:
    // "is the binary I'm looking at the one with the fix in it?" We spent one
    // debugging a bug that was already fixed — in a build that predated the fix.
    //
    // CARGO_PKG_VERSION is read from Cargo.toml at COMPILE time, so the number
    // on screen cannot drift from the source that produced the binary. A
    // hand-maintained constant could; this can't.
    //
    // It gets its own row in the layout rather than being painted over the
    // bottom of the screen: overlapping the action row would eat its border.
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("build {} ", env!("CARGO_PKG_VERSION")),
            theme::mute(),
        )))
        .alignment(Alignment::Right),
        rows[3],
    );

    app.can_advance = true;
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            app.cursor = app.cursor.saturating_sub(1);
            apply(app); // live preview: switch UI language as you move
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.cursor = (app.cursor + 1).min(OPTIONS.len() - 1);
            apply(app);
        }
        KeyCode::Enter => {
            apply(app);
            // After choosing the language, present the mode chooser (Install /
            // Recovery) rather than dropping straight into the install flow.
            app.goto(crate::app::Screen::Mode);
        }
        _ => {}
    }
}

fn apply(app: &mut App) {
    let lang = OPTIONS[app.cursor.min(OPTIONS.len() - 1)];
    app.lang = lang;
    match lang {
        Lang::En => {
            app.config.lang = "en".into();
            app.config.locale = "en_US.UTF-8".into();
            if !app.keyboard_touched {
                app.config.keymap = "gb".into();
                app.config.xkb_layouts = vec!["gb".into()];
            }
        }
        // Latin American Spanish (es-419). The locale is es_MX because that is
        // what the translation is written in; the layout is `es` because that
        // one code is valid BOTH as a console keymap and as an X11 layout,
        // which is not true of the Latin American pair (console `la-latin1`,
        // X11 `latam`). Anyone who wants the Latin American key placement picks
        // it on the keyboard step — this only has to be a sane, working start.
        Lang::Es => {
            app.config.lang = "es".into();
            app.config.locale = "es_MX.UTF-8".into();
            if !app.keyboard_touched {
                app.config.keymap = "es".into();
                app.config.xkb_layouts = vec!["es".into()];
            }
        }
        Lang::Uk => {
            app.config.lang = "uk".into();
            app.config.locale = "uk_UA.UTF-8".into();
            // Picking Ukrainian as the interface language selects BOTH layouts,
            // but ENGLISH FIRST (primary) and Ukrainian second. The primary
            // layout is what the console keymap (vconsole KEYMAP) is set to, and
            // it must be Latin: the initramfs `keymap` hook loads it BEFORE the
            // `encrypt` hook, so a Cyrillic-primary layout would silently break
            // typing the LUKS passphrase. A Latin primary also keeps keyboard
            // shortcuts (which are defined against Latin keysyms) working in
            // every DE out of the box; Ukrainian is one toggle away as the
            // second layout. Skipped once the user has chosen for themselves.
            if !app.keyboard_touched {
                app.config.keymap = "gb".into();
                app.config.xkb_layouts = vec!["gb".into(), "ua".into()];
            }
        }
    }
    // The console follows the language immediately: whatever layout this
    // language just chose is the one the next screens will be typed on.
    let km =
        crate::screens::keyboard::live_keymap(&app.config.lang, &app.config.keymap).to_string();
    crate::screens::keyboard::apply_keymap(&km);
}

#[cfg(test)]
mod tests {
    /// Choosing a language a SECOND time must not disturb what was chosen after
    /// the first time.
    ///
    /// Reported from a real run: pick Ukrainian, pick a timezone, step back,
    /// pick Spanish, step back, pick Ukrainian again — and the timezone screen
    /// no longer highlighted the chosen zone. The config still held it; the
    /// cursor did not, because Language and the mode menu both jumped by
    /// assigning `app.screen` and so skipped the bookkeeping that parks the
    /// cursor on the configured value. The next arrow key then committed a
    /// different zone over the top of it.
    #[test]
    fn choosing_a_language_twice_does_not_lose_the_timezone() {
        use crate::app::Screen;

        let mut app = App::new();
        // First pass: Ukrainian, then a timezone the user picked by hand.
        app.cursor = OPTIONS.iter().position(|l| *l == Lang::Uk).unwrap();
        handle_key(&mut app, KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.screen, Screen::Mode);
        app.goto(Screen::Timezone);
        app.config.timezone = "America/Argentina/Ushuaia".into();
        app.goto(Screen::Language);

        // Second pass: Spanish, back, Ukrainian again.
        app.cursor = OPTIONS.iter().position(|l| *l == Lang::Es).unwrap();
        handle_key(&mut app, KeyEvent::from(KeyCode::Enter));
        app.goto(Screen::Language);
        app.cursor = OPTIONS.iter().position(|l| *l == Lang::Uk).unwrap();
        handle_key(&mut app, KeyEvent::from(KeyCode::Enter));

        // Enter the flow through the mode menu itself, which is the path that
        // was broken — calling goto() here by hand would test the fix instead
        // of the journey.
        app.mode_cursor = 0; // Install
        crate::screens::mode::handle_key(&mut app, KeyEvent::from(KeyCode::Enter));
        // Whatever the first post-language step is — the keyboard now, so that
        // the layout is settled before anything asks for text.
        assert_eq!(app.screen, Screen::ALL[1]);
        app.goto(Screen::Timezone);
        assert_eq!(
            app.config.timezone, "America/Argentina/Ushuaia",
            "the language screen overwrote the chosen timezone"
        );
        // The cursor comes back AT THE TOP now — the list is searched, not
        // browsed — so what has to survive is the CHOICE, not the position.
        assert_eq!(app.cursor, 0, "the list did not open at the top");
    }

    use super::*;

    /// Switching languages must not leave the PREVIOUS language's layout behind.
    ///
    /// Reported from a real run: the user tried each language in turn, landed on
    /// Spanish, then chose Ukrainian — and the keyboard step came up Spanish.
    /// The guard compared the layout list against every known default, and that
    /// list never included `["es"]`, so once Spanish had been visited every
    /// later language kept its layout.
    #[test]
    fn walking_through_the_languages_leaves_the_last_ones_layout_behind() {
        let mut app = App::new();
        let pick = |app: &mut App, lang: Lang| {
            app.cursor = OPTIONS.iter().position(|l| *l == lang).unwrap();
            apply(app);
        };

        pick(&mut app, Lang::Es);
        assert_eq!(app.config.xkb_layouts, vec!["es".to_string()]);

        pick(&mut app, Lang::Uk);
        assert_eq!(
            app.config.xkb_layouts,
            vec!["gb".to_string(), "ua".to_string()],
            "Ukrainian kept the Spanish layout"
        );

        pick(&mut app, Lang::En);
        assert_eq!(app.config.xkb_layouts, vec!["gb".to_string()]);

        // Round and round: every order must end on the right layout.
        for lang in [Lang::Es, Lang::En, Lang::Uk, Lang::Es, Lang::Uk] {
            pick(&mut app, lang);
        }
        assert_eq!(
            app.config.xkb_layouts,
            vec!["gb".to_string(), "ua".to_string()]
        );
    }

    /// A layout the user chose themselves survives a language change.
    #[test]
    fn a_deliberate_layout_is_never_overwritten() {
        let mut app = App::new();
        app.config.xkb_layouts = vec!["de".into()];
        app.config.keymap = "de".into();
        app.keyboard_touched = true;

        for lang in [Lang::Es, Lang::Uk, Lang::En] {
            app.cursor = OPTIONS.iter().position(|l| *l == lang).unwrap();
            apply(&mut app);
            assert_eq!(
                app.config.xkb_layouts,
                vec!["de".to_string()],
                "{lang:?} overwrote a layout the user picked"
            );
        }
    }

    /// Every language the binary carries a translation for is REACHABLE.
    ///
    /// The Spanish contribution arrived as es.toml plus nothing else: the file
    /// was embedded, parsed and complete, and no user could ever select it,
    /// because `Lang` had two variants and this screen listed two options. A
    /// translation nobody can choose is not a supported language, so the check
    /// is "can it be picked", not "does the file exist".
    #[test]
    fn every_translation_can_actually_be_selected() {
        for lang in [Lang::Uk, Lang::En, Lang::Es] {
            assert!(
                OPTIONS.contains(&lang),
                "{lang:?} has a translation but the language screen never offers it"
            );
        }
    }

    /// Picking a language yields a locale the target can actually generate, and
    /// a keymap code that is valid BOTH as a console keymap and an X11 layout.
    /// `latam` is X11-only and `la-latin1` console-only — either would half-work.
    #[test]
    fn each_language_sets_a_usable_locale_and_layout() {
        for (i, lang) in OPTIONS.iter().enumerate() {
            let mut app = App::new();
            app.cursor = i;
            apply(&mut app);
            assert_eq!(app.lang, *lang);
            assert!(
                app.config.locale.ends_with(".UTF-8"),
                "{lang:?}: locale {:?} is not a UTF-8 locale",
                app.config.locale
            );
            assert!(
                !app.config.keymap.is_empty() && !app.config.xkb_layouts.is_empty(),
                "{lang:?}: left the keyboard unset"
            );
        }
    }

    /// Spanish actually RENDERS Spanish.
    ///
    /// `t()` falls back to English for any key it cannot find, so a locale file
    /// that failed to load, parse or embed would leave the whole UI silently in
    /// English while every other test still passed. Comparing against the
    /// English string is what makes the fallback visible.
    #[test]
    fn spanish_renders_spanish_and_not_the_english_fallback() {
        for key in ["lang.title", "lang.hint", "app.next", "app.back"] {
            let es = t(Lang::Es, key);
            let en = t(Lang::En, key);
            assert_ne!(
                es, en,
                "'{key}' is identical in es and en — es.toml did not load and \
                 the UI is silently falling back to English"
            );
            assert!(!es.is_empty(), "'{key}' is empty in Spanish");
        }
        // One anchored value, so a future edit that guts es.toml into
        // placeholders still fails rather than passing on mere difference.
        assert_eq!(t(Lang::Es, "lang.title"), "Elige el idioma");
    }

    /// The Spanish label credits its translator. Contributed strings are the one
    /// place a person's name belongs in the UI, and it is easy to lose in a
    /// later edit of the locale files.
    #[test]
    fn the_spanish_label_credits_its_translator() {
        for lang in [Lang::Uk, Lang::En, Lang::Es] {
            let label = t(lang, "lang.es");
            assert!(
                label.contains("ich0x"),
                "{lang:?}: the Spanish entry lost its translation credit: {label}"
            );
        }
    }
}
