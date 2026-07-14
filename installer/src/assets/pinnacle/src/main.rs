use std::sync::Arc;
use std::sync::Mutex;

use pinnacle_api::input;
use pinnacle_api::input::Bind;
use pinnacle_api::input::Keysym;
use pinnacle_api::input::{Mod, MouseButton};
use pinnacle_api::layout;
use pinnacle_api::layout::LayoutGenerator;
use pinnacle_api::layout::LayoutNode;
use pinnacle_api::layout::LayoutResponse;
use pinnacle_api::layout::generators::Corner;
use pinnacle_api::layout::generators::CornerLocation;
use pinnacle_api::layout::generators::Cycle;
use pinnacle_api::layout::generators::Dwindle;
use pinnacle_api::layout::generators::Fair;
use pinnacle_api::layout::generators::Floating;
use pinnacle_api::layout::generators::MasterSide;
use pinnacle_api::layout::generators::MasterStack;
use pinnacle_api::layout::generators::Spiral;
use pinnacle_api::output;
use pinnacle_api::pinnacle;
use pinnacle_api::pinnacle::Backend;
use pinnacle_api::process::Command;
use pinnacle_api::signal::OutputSignal;
use pinnacle_api::signal::WindowSignal;
use pinnacle_api::tag;
use pinnacle_api::util::{Axis, Batch};
use pinnacle_api::window;

async fn config() {
    let mod_key = match pinnacle::backend() {
        Backend::Tty => Mod::SUPER,
        Backend::Window => Mod::ALT,
    };

    let terminal = "kitty";

//------------------------
// Autostart             |
//------------------------
Command::new("dbus-update-activation-environment")
    .args(["WAYLAND_DISPLAY", "XDG_CURRENT_DESKTOP=wlroots", "XDG_SESSION_TYPE"])
    .once()
    .spawn();
Command::new("/usr/lib/xdg-desktop-portal-wlr").unique().spawn();
Command::new("/usr/lib/xdg-desktop-portal").unique().spawn();
Command::new("/usr/lib/lxsession/lxpolkit").once().spawn();
Command::new("wl-paste").args(["--watch", "cliphist", "store"]).once().spawn();
// waybar — the status bar. A plain independent spawn: our earlier dinit
// debugging showed that bundled shell chains (bash -c "sleep && …") are
// unreliable here, while one Command per applet works. `unique()` keeps a
// config restart from stacking a second bar on top of the first.
Command::new("waybar").unique().spawn();
Command::new("swaync").once().spawn();
Command::new("waypaper").args(["--restore"]).once().spawn();
Command::new("nm-applet").unique().spawn();
Command::new("pasystray").unique().spawn();
Command::new("kdeconnectd").unique().spawn();
Command::new("kdeconnect-indicator").unique().spawn();

    //------------------------
    // Mousebinds            |
    //------------------------

    input::mousebind(mod_key, MouseButton::Left)
        .on_press(|| { window::begin_move(MouseButton::Left); })
        .group("Mouse")
        .description("Move window");

    input::mousebind(mod_key, MouseButton::Right)
        .on_press(|| { window::begin_resize(MouseButton::Right); })
        .group("Mouse")
        .description("Resize window");

    //------------------------
    // Keybinds              |
    //------------------------

    // ФІКС: Пряме перемикання плаваючого режиму на Super + S
    input::keybind(mod_key, 's')
        .on_press(|| { 
            if let Some(win) = window::get_focused() { 
                win.toggle_floating(); 
                win.raise();
            } 
        })
        .group("Window")
        .description("Toggle floating (fixes OBS capture)");

    #[cfg(not(feature = "snowcap"))]
    input::keybind(mod_key | Mod::SHIFT, 'q')
        .set_as_quit()
        .group("Compositor")
        .description("Quit Pinnacle");

    #[cfg(feature = "snowcap")]
    {
        input::keybind(mod_key | Mod::SHIFT, 'q')
            .on_press(|| { pinnacle_api::snowcap::QuitPrompt::new().show(); })
            .group("Compositor")
            .description("Show quit prompt");

        input::keybind(mod_key | Mod::CTRL | Mod::SHIFT, 'q')
            .set_as_quit()
            .group("Compositor")
            .description("Quit Pinnacle without prompt");
    }

    input::keybind(mod_key | Mod::SHIFT, 'r')
        .set_as_reload_config()
        .group("Compositor")
        .description("Reload the config");

    input::keybind(mod_key | Mod::CTRL, 'r')
        .set_as_reload_config()
        .group("Compositor")
        .description("Reload the config");

    input::keybind(mod_key, Keysym::Return)
        .on_press(move || { Command::new(terminal).spawn(); })
        .group("Process")
        .description("Spawn a terminal");

    input::keybind(mod_key, 'q')
        .on_press(move || { Command::new(terminal).spawn(); })
        .group("Process")
        .description("Spawn a terminal");

    input::keybind(mod_key, 'r')
        .on_press(|| { Command::new("wofi").args(["--show", "drun"]).spawn(); })
        .group("Process")
        .description("Open app launcher");

    input::keybind(mod_key, 'e')
        .on_press(|| { Command::new("caja").spawn(); })
        .group("Process")
        .description("Open file manager");

    input::keybind(mod_key, 'b')
        .on_press(|| { Command::new("firefox").spawn(); })
        .group("Process")
        .description("Open browser");

    input::keybind(mod_key, 'o')
        .on_press(|| { Command::new("flameshot").arg("gui").spawn(); })
        .group("Process")
        .description("Screenshot");

    input::keybind(mod_key, 'v')
        .on_press(|| { Command::new("sh").arg("-c").arg("~/.config/pinnacle/scripts/clipboard.sh").spawn(); })
        .group("Process")
        .description("Clipboard manager");

    input::keybind(mod_key, 'n')
        .on_press(|| { Command::new("swaync-client").arg("-t").spawn(); })
        .group("Process")
        .description("Toggle notification panel");

    input::keybind(mod_key, 'c')
        .on_press(|| { if let Some(window) = window::get_focused() { window.close(); } })
        .group("Window")
        .description("Close focused window");

    // Ці бінди залишаю як дублюючі
    input::keybind(mod_key | Mod::CTRL, Keysym::space)
        .on_press(|| { if let Some(window) = window::get_focused() { window.toggle_floating(); window.raise(); } })
        .group("Window")
        .description("Toggle floating");

    input::keybind(mod_key | Mod::SHIFT, Keysym::space)
        .on_press(|| { if let Some(window) = window::get_focused() { window.toggle_floating(); window.raise(); } })
        .group("Window")
        .description("Toggle floating");

    input::keybind(mod_key, 'f')
        .on_press(|| { if let Some(window) = window::get_focused() { window.toggle_fullscreen(); window.raise(); } })
        .group("Window")
        .description("Toggle fullscreen");

    input::keybind(mod_key, 'm')
        .on_press(|| { if let Some(window) = window::get_focused() { window.toggle_maximized(); window.raise(); } })
        .group("Window")
        .description("Toggle maximized");

    // Media keybinds
    input::keybind(Mod::empty(), Keysym::XF86_AudioRaiseVolume)
        .on_press(|| { Command::new("wpctl").args(["set-volume", "@DEFAULT_AUDIO_SINK@", "0.02+", "-l", "1.0"]).spawn(); })
        .allow_when_locked()
        .group("Media")
        .description("Increase volume");

    input::keybind(Mod::empty(), Keysym::XF86_AudioLowerVolume)
        .on_press(|| { Command::new("wpctl").args(["set-volume", "@DEFAULT_AUDIO_SINK@", "0.02-", "-l", "1.0"]).spawn(); })
        .allow_when_locked()
        .group("Media")
        .description("Decrease volume");

    input::keybind(Mod::empty(), Keysym::XF86_AudioMute)
        .on_press(|| { Command::new("wpctl").args(["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"]).spawn(); })
        .allow_when_locked()
        .group("Media")
        .description("Toggle mute");

    input::keybind(Mod::empty(), Keysym::XF86_AudioMicMute)
        .on_press(|| { Command::new("wpctl").args(["set-mute", "@DEFAULT_AUDIO_SOURCE@", "toggle"]).spawn(); })
        .allow_when_locked()
        .group("Media")
        .description("Toggle mic mute");

    input::keybind(Mod::empty(), Keysym::XF86_AudioPlay)
        .on_press(|| { Command::new("playerctl").arg("play-pause").spawn(); })
        .allow_when_locked()
        .group("Media")
        .description("Play/pause");

    input::keybind(Mod::empty(), Keysym::XF86_AudioStop)
        .on_press(|| { Command::new("playerctl").arg("stop").spawn(); })
        .allow_when_locked()
        .group("Media")
        .description("Stop media");

    input::keybind(Mod::empty(), Keysym::XF86_AudioNext)
        .on_press(|| { Command::new("playerctl").arg("next").spawn(); })
        .allow_when_locked()
        .group("Media")
        .description("Next track");

    input::keybind(Mod::empty(), Keysym::XF86_AudioPrev)
        .on_press(|| { Command::new("playerctl").arg("previous").spawn(); })
        .allow_when_locked()
        .group("Media")
        .description("Previous track");

    input::keybind(Mod::empty(), Keysym::XF86_MonBrightnessUp)
        .on_press(|| { Command::new("brightnessctl").args(["--class=backlight", "set", "+10%"]).spawn(); })
        .allow_when_locked()
        .group("Display")
        .description("Increase brightness");

    input::keybind(Mod::empty(), Keysym::XF86_MonBrightnessDown)
        .on_press(|| { Command::new("brightnessctl").args(["--class=backlight", "set", "10%-"]).spawn(); })
        .allow_when_locked()
        .group("Display")
        .description("Decrease brightness");

    //------------------------
    // Layouts               |
    //------------------------

    fn into_box<'a, T: LayoutGenerator + Send + 'a>(g: T) -> Box<dyn LayoutGenerator + Send + 'a> {
        Box::new(g) as _
    }

    let cycler = Arc::new(Mutex::new(Cycle::new([
        into_box(MasterStack { reversed: true, ..Default::default() }),
        into_box(MasterStack { master_side: MasterSide::Right, ..Default::default() }),
        into_box(MasterStack { master_side: MasterSide::Top, ..Default::default() }),
        into_box(MasterStack { master_side: MasterSide::Bottom, ..Default::default() }),
        into_box(Dwindle::default()),
        into_box(Spiral::default()),
        into_box(Corner::default()),
        into_box(Corner { corner_loc: CornerLocation::TopRight, ..Default::default() }),
        into_box(Corner { corner_loc: CornerLocation::BottomLeft, ..Default::default() }),
        into_box(Corner { corner_loc: CornerLocation::BottomRight, ..Default::default() }),
        into_box(Fair::default()),
        into_box(Fair { axis: Axis::Horizontal, ..Default::default() }),
        into_box(Floating::default()),
    ])));

    let layout_requester = layout::manage({
        let cycler = cycler.clone();
        move |args| {
            let Some(tag) = args.tags.first() else {
                return LayoutResponse { root_node: LayoutNode::new(), tree_id: 0 };
            };
            let mut c = cycler.lock().unwrap();
            c.set_current_tag(tag.clone());
            LayoutResponse { root_node: c.layout(args.window_count), tree_id: c.current_tree_id() }
        }
    });

    input::keybind(mod_key, Keysym::space)
        .on_press({
            let cycler = cycler.clone();
            let requester = layout_requester.clone();
            move || {
                let Some(out) = output::get_focused() else { return; };
                let Some(tag) = out.tags().batch_find(|t| Box::pin(t.active_async()), |a| *a) else { return; };
                cycler.lock().unwrap().cycle_layout_forward(&tag);
                requester.request_layout_on_output(&out);
            }
        })
        .group("Layout")
        .description("Cycle layout forward");

    input::keybind(mod_key | Mod::SHIFT, Keysym::space)
        .on_press(move || {
            let Some(out) = output::get_focused() else { return; };
            let Some(tag) = out.tags().batch_find(|t| Box::pin(t.active_async()), |a| *a) else { return; };
            cycler.lock().unwrap().cycle_layout_backward(&tag);
            layout_requester.request_layout_on_output(&out);
        })
        .group("Layout")
        .description("Cycle layout backward");

    //------------------------
    // Tags                  |
    //------------------------

    let tag_names = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];

    output::for_each_output(move |output| {
        match output.name().as_str() {
            "DVI-D-1" => output.set_loc(0, 0),
            "HDMI-A-1" => output.set_loc(1920, 0),
            _ => {}
        }
        let mut tags = tag::add(&output, tag_names);
        tags.next().unwrap().set_active(true);
    });

    for tag_name in tag_names {
        input::keybind(mod_key, tag_name)
            .on_press(move || {
                if let Some(tag) = tag::get(tag_name) {
                    tag.switch_to();
                }
            })
            .group("Tag")
            .description(format!("Switch to tag {tag_name}"));

        input::keybind(mod_key | Mod::CTRL, tag_name)
            .on_press(move || {
                if let Some(tag) = tag::get(tag_name) {
                    tag.toggle_active();
                }
            })
            .group("Tag")
            .description(format!("Toggle tag {tag_name}"));

        input::keybind(mod_key | Mod::SHIFT, tag_name)
            .on_press(move || {
                if let Some(tag) = tag::get(tag_name)
                    && let Some(win) = window::get_focused()
                {
                    win.move_to_tag(&tag);
                }
            })
            .group("Tag")
            .description(format!("Move window to tag {tag_name}"));

        input::keybind(mod_key | Mod::CTRL | Mod::SHIFT, tag_name)
            .on_press(move || {
                if let Some(tg) = tag::get(tag_name)
                    && let Some(win) = window::get_focused()
                {
                    win.toggle_tag(&tg);
                }
            })
            .group("Tag")
            .description(format!("Toggle tag {tag_name} on window"));
    }

    //------------------------
    // Input                 |
    //------------------------

    input::set_xkb_config(input::XkbConfig {
        layout: Some("us,ua".to_string()),
        options: Some("grp:alt_shift_toggle".to_string()),
        ..Default::default()
    });

    //------------------------
    // Signals               |
    //------------------------

    window::connect_signal(WindowSignal::PointerEnter(Box::new(|win| {
        win.set_focused(true);
    })));

    output::connect_signal(OutputSignal::PointerEnter(Box::new(|output| {
        output.focus();
    })));

    //------------------------
    // Snowcap               |
    //------------------------

    #[cfg(feature = "snowcap")]
    {
        use pinnacle_api::snowcap::FocusBorder;
        use snowcap_api::widget::Color;

        for win in window::get_all() {
            let _ = FocusBorder {
                focused_color: Color::rgb(0.75, 0.6, 0.0),
                unfocused_color: Color::rgb(0.08, 0.08, 0.08),
                ..FocusBorder::new_with_titlebar(&win)
            }.decorate();
        }

        window::add_window_rule(move |window| {
            window.set_decoration_mode(window::DecorationMode::ServerSide);
            let _ = FocusBorder {
                focused_color: Color::rgb(0.75, 0.6, 0.0),
                unfocused_color: Color::rgb(0.08, 0.08, 0.08),
                ..FocusBorder::new_with_titlebar(&window)
            }.decorate();
        });
    }

    #[cfg(feature = "snowcap")]
    if let Some(error) = pinnacle_api::pinnacle::take_last_error() {
        pinnacle_api::snowcap::ConfigCrashedMessage::new(error).show();
    }

    //------------------------
    // Autostart terminal    |
    //------------------------

    Command::new(terminal).once().spawn();
}

pinnacle_api::main!(config);
