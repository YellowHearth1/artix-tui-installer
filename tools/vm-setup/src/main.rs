//! Pick the QEMU test stand — how many virtual drives, what each one pretends
//! to be, and whether the LUKS-key USB stick is attached.
//!
//! # It decides; it does not act
//!
//! This program never creates, renames or deletes a file. It reads the stand,
//! lets you rearrange it, and prints a PLAN on stdout for `scripts/qemu-test.sh`
//! to carry out:
//!
//! ```text
//! create 2 hdd 50
//! rename disk1-ssd.qcow2 disk1-nvme.qcow2
//! delete disk3-nvme.qcow2
//! usbkey on
//! ```
//!
//! That is the same split the installer itself is built on — the partition
//! editor collects intent and only the plan touches a disk — and it earns the
//! same two things here. There is ONE implementation of each action (in the
//! shell, which still works when this tool cannot be built), and a drive holding
//! an installed system cannot be lost to a stray keypress: nothing happens until
//! Enter, and Esc leaves the stand exactly as it was.
//!
//! The interface draws on STDERR so stdout carries nothing but the plan.
//!
//! Cancelling exits 1 with an empty plan, which the caller reads as "leave
//! everything alone".

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame, Terminal,
};
use std::io;

// ── Language ────────────────────────────────────────────────────────────────
// The installer speaks three languages, and so does this. The person who reads
// it is the same person: someone weighing up a move to Artix, working in their
// own language.
//
// The strings are TRIPLETS rather than three lookup tables. The installer's
// TOML files need a test to prove they define the same keys, because they can
// drift apart; a triplet cannot — a missing translation is a missing struct
// field, and the compiler refuses it. Thirty strings do not need a file format.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Lang {
    Uk,
    En,
    Es,
}

/// One string in every language it is shown in.
struct S(&'static str, &'static str, &'static str);

impl S {
    fn get(&self, l: Lang) -> &'static str {
        match l {
            Lang::Uk => self.0,
            Lang::En => self.1,
            Lang::Es => self.2,
        }
    }
}

impl Lang {
    /// Same order and same names the installer's own language screen uses.
    const ALL: [Lang; 3] = [Lang::Uk, Lang::En, Lang::Es];

    fn tag(self) -> &'static str {
        match self {
            Lang::Uk => "uk",
            Lang::En => "en",
            Lang::Es => "es",
        }
    }

    /// Each language named IN ITSELF — the one rule a language switcher cannot
    /// break, because "Ukrainian" is no help to someone who only reads
    /// Ukrainian.
    fn name(self) -> &'static str {
        match self {
            Lang::Uk => "Українська",
            Lang::En => "English",
            Lang::Es => "Español",
        }
    }

    fn step(self, forward: bool) -> Lang {
        let i = Lang::ALL.iter().position(|l| *l == self).unwrap_or(0);
        let n = Lang::ALL.len();
        Lang::ALL[if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        }]
    }
}

const TITLE: S = S(
    "Стенд віртуальної машини",
    "QEMU test stand",
    "Banco de pruebas QEMU",
);
const SUBTITLE: S = S(
    "Нічого не записується, доки не натиснете Enter.",
    "Nothing is written until you press Enter.",
    "No se escribe nada hasta que pulses Intro.",
);
const ON_ENTER: S = S(" Після Enter ", " On Enter ", " Al pulsar Intro ");
const NOTHING_TO_DO: S = S(
    "нічого — стенд уже такий, як треба",
    "nothing — the stand is already what you want",
    "nada — el banco ya es como lo quieres",
);
const SIZE_TITLE: S = S("Розмір диска", "Drive size", "Tamaño del disco");
const SIZE_PROMPT: S = S(
    "Скільки місця дати цьому диску?",
    "How much space should this drive have?",
    "¿Cuánto espacio debe tener este disco?",
);
const SIZE_KEYS: S = S(
    "цифри розмір · ←/→ GiB/MiB · Enter підтвердити · Esc скасувати",
    "digits size · ←/→ GiB/MiB · Enter confirm · Esc cancel",
    "números tamaño · ←/→ GiB/MiB · Intro confirmar · Esc cancelar",
);
const SIZE_EMPTY: S = S(
    "! введіть число більше за нуль",
    "! type a number greater than zero",
    "! escribe un número mayor que cero",
);
const ADD_DRIVE: S = S("+ Додати диск", "+ Add a drive", "+ Añadir disco");
const ADD_ENTER: S = S("Enter", "Enter", "Intro");
const ST_ADDED: S = S(
    "диск додано — ←/→ тип, цифри розмір",
    "drive added — ←/→ medium, digits for size",
    "disco añadido — ←/→ medio, números para el tamaño",
);
const ADD_HEAD: S = S(
    "Створити ще один віртуальний диск.",
    "Create another virtual drive.",
    "Crear otro disco virtual.",
);
const ADD_SUB: S = S(
    "Enter — додати. Потім ←/→ задає тип (SSD/HDD/NVMe), а цифри — розмір у ГіБ.",
    "Enter adds one. Then ←/→ sets the medium (SSD/HDD/NVMe) and digits the size in GiB.",
    "Intro añade uno. Luego ←/→ fija el medio (SSD/HDD/NVMe) y los números el tamaño en GiB.",
);
const APPLY: S = S("[Enter] Застосувати", "[Enter] Apply", "[Intro] Aplicar");

const ROW_LANG: S = S("мова", "language", "idioma");
const LANG_HEAD: S = S(
    "Мова цього помічника.",
    "The language of this helper.",
    "El idioma de este asistente.",
);
const LANG_SUB: S = S(
    "Ті самі три мови, що й в інсталяторі. Вибір памʼятається у vm/stand.conf.",
    "The same three the installer speaks. The choice is remembered in vm/stand.conf.",
    "Los mismos tres del instalador. La elección se guarda en vm/stand.conf.",
);
const ROW_MEM: S = S("памʼять", "memory", "memoria");
const ROW_CPUS: S = S("ядра", "cores", "núcleos");
const ROW_SOUND: S = S("звук", "sound", "sonido");
const ROW_USBKEY: S = S("USB-ключ", "USB key", "llave USB");
const HOST_HAS: S = S("у хоста", "host has", "el anfitrión tiene");
const AUTO: S = S("авто", "auto", "auto");
const OFF: S = S("вимк", "off", "no");
const SOUND_AUTO: S = S(
    "той сервер звуку, що працює",
    "whichever sound server is running",
    "el servidor de sonido que esté activo",
);
const SILENT: S = S("тиша", "silent", "en silencio");
const SOUND_FORCED: S = S(
    "саме цей бекенд, незалежно від того, що на хості",
    "this backend, whatever the host is running",
    "este backend, sea cual sea el del anfitrión",
);
const ATTACHED: S = S("підключено", "attached", "conectada");
const NOT_ATTACHED: S = S("не підключено", "not attached", "no conectada");

const MEM_HEAD: S = S(
    "Скільки оперативної памʼяті дістанеться гостю.",
    "How much RAM the guest gets.",
    "Cuánta RAM recibe el invitado.",
);
const MEM_SUB: S = S(
    "2–4 ГіБ — саме та машина, заради якої писалися zswap і earlyoom. Постав тут стільки, щоб побачити, як вони працюють.",
    "2–4 GiB is the machine the memory-tuning options were written for — set it here to see them do something.",
    "2–4 GiB es la máquina para la que se escribieron zswap y earlyoom: ponlo aquí para verlos actuar.",
);
const CPU_HEAD: S = S(
    "Скільки ядер дістанеться гостю.",
    "How many cores the guest gets.",
    "Cuántos núcleos recibe el invitado.",
);
const CPU_SUB: S = S(
    "Типово половина хоста, щоб машина лишалася придатною для роботи під час тесту.",
    "Half the host by default, so the machine stays usable while a test runs.",
    "La mitad del anfitrión por defecto, para que la máquina siga usable.",
);
const SND_HEAD: S = S(
    "Звук у гостя.",
    "Sound in the guest.",
    "Sonido en el invitado.",
);
const SND_SUB: S = S(
    "«авто» бере той сервер, що працює на хості. Решта — назвати бекенд прямо: PipeWire, PulseAudio, ALSA або тиша.",
    "auto picks whichever server the host runs. The rest name a backend outright: PipeWire, PulseAudio, ALSA, or silence.",
    "«auto» toma el servidor que use el anfitrión. El resto nombran el backend: PipeWire, PulseAudio, ALSA o silencio.",
);
const USB_HEAD: S = S(
    "Знімний стік — для перевірки розблокування LUKS ключем.",
    "A removable stick, for testing the LUKS USB-key feature.",
    "Una memoria extraíble, para probar el desbloqueo LUKS con llave.",
);
const USB_SUB: S = S(
    "У гостя це справжній диск, тож без нього список дисків коротший.",
    "It is a real drive in the guest, so leaving it off keeps the disk list short.",
    "En el invitado es un disco real, así que sin ella la lista queda más corta.",
);
const DRIVE_NEW: S = S(
    "Розмір — цифрами або +/-. Тонкий: до першого запису займає близько 200 КБ.",
    "Size: type it, or +/-. Thin: it costs about 200 KB until the guest writes to it.",
    "Tamaño: escríbelo, o +/-. Ligero: ocupa unos 200 KB hasta que el invitado escriba.",
);
const DRIVE_RENAMED: S = S(
    "Образ лише перейменується — усе встановлене на ньому лишається.",
    "The image is only renamed — whatever is installed on it survives.",
    "La imagen solo se renombra — lo instalado en ella sobrevive.",
);
const DRIVE_KEPT: S = S(
    "←/→ міняє носій; сам диск лишається на місці.",
    "Change the medium with ←/→; the drive itself is kept.",
    "Cambia el medio con ←/→; el disco se conserva.",
);

const KEYS_DRIVE: S = S(
    "←/→ носій · Enter розмір · a додати · d видалити · Esc скасувати",
    "←/→ medium · Enter size · a add · d delete · Esc cancel",
    "←/→ medio · Intro tamaño · a añadir · d borrar · Esc cancelar",
);
const KEYS_TOGGLE: S = S(
    "←/→ перемкнути · Пробіл перемкнути · a додати диск · Enter застосувати · Esc скасувати",
    "←/→ toggle · Space toggle · a add drive · Enter apply · Esc cancel",
    "←/→ alterna · Espacio alterna · a añadir disco · Intro aplicar · Esc cancelar",
);
const KEYS_ADD: S = S(
    "Enter додати диск · ↑/↓ рух · Esc скасувати",
    "Enter add a drive · ↑/↓ move · Esc cancel",
    "Intro añadir disco · ↑/↓ mover · Esc cancelar",
);
const KEYS_VALUE: S = S(
    "←/→ змінити · +/- змінити · a додати диск · Enter застосувати · Esc скасувати",
    "←/→ change · +/- change · a add drive · Enter apply · Esc cancel",
    "←/→ cambiar · +/- cambiar · a añadir disco · Intro aplicar · Esc cancelar",
);

const ST_MARKED: S = S(
    "позначено на видалення — d ще раз, щоб лишити",
    "marked for deletion — d again to keep it",
    "marcado para borrar — pulsa d otra vez para conservarlo",
);
const ST_KEPT: S = S("лишено", "kept", "conservado");
const ST_DOOMED: S = S(
    "! цей диск позначено на видалення — натисніть d, щоб лишити",
    "! this drive is marked for deletion — press d to keep it",
    "! este disco está marcado para borrar — pulsa d para conservarlo",
);
const ST_NO_RESIZE: S = S(
    "! наявний образ тут не змінює розмір — видаліть і створіть новий",
    "! an existing image is not resized here — delete and add one",
    "! una imagen existente no se redimensiona aquí — bórrala y crea otra",
);
const ST_CEILING: S = S(
    "! це все, що є в хоста — лишіть йому щось",
    "! that is all the host has — leave it something",
    "! eso es todo lo que tiene el anfitrión — déjale algo",
);
const WILL_DELETE: S = S("буде ВИДАЛЕНО", "will be DELETED", "se BORRARÁ");
const NEW_TAG: S = S("новий", "new", "nuevo");
const KEPT_RENAMED: S = S(
    "лишається, перейменування",
    "kept, renamed",
    "se conserva, renombrado",
);
const ON_DISK: S = S("на диску", "on disk", "en disco");

// ── Palette ─────────────────────────────────────────────────────────────────
// Deliberately the same colours as installer/src/theme.rs, and indexed ANSI for
// the same reason: this is often read on a plain VT, where truecolour is
// approximated onto sixteen colours and mid-tone RGB collapses into
// indistinguishable greys. A separate crate cannot share the module, so the
// values are repeated — if the installer's palette ever changes, this is the
// other place it lives.
// TRUECOLOR HERE, indexed ANSI in the installer — and that is a real difference,
// not a slip. The installer's first target is the Linux VT, where 24-bit colour
// is approximated onto sixteen and mid-tones collapse into identical greys. This
// helper only ever runs in a terminal emulator on a working desktop, so it can
// have the palette the installer would like to have: the same hues, turned up.
//
// The two BACKGROUNDS are not decoration. Without them the helper draws straight
// onto whatever shows through the terminal — on a transparent Kitty that is the
// wallpaper, and grey text on a photograph is unreadable.
const BG: Color = Color::Rgb(11, 15, 20);
const PANEL: Color = Color::Rgb(17, 24, 35);
const PILL: Color = Color::Rgb(20, 30, 40); // the ‹ value › chips at rest
const PILL_ON: Color = Color::Rgb(22, 52, 58); // and under the cursor
const ACCENT: Color = Color::Rgb(94, 246, 255); // neon cyan — the Artix note
const ACCENT_DIM: Color = Color::Rgb(43, 184, 196);
const FG: Color = Color::Rgb(230, 242, 245);
const DIM: Color = Color::Rgb(143, 163, 172);
const MUTE: Color = Color::Rgb(85, 102, 110);
const WARN: Color = Color::Rgb(255, 122, 147);
// Deliberately NOT green, the same as the installer: the whole thing is keyed to
// the Artix cyan and green fights it. "Done" reads as a calmer cyan; anything
// being ADDED gets amber, so new and destructive are never the same colour.
const OK: Color = Color::Rgb(69, 224, 216);
const NEW: Color = Color::Rgb(255, 196, 107);

fn title_style() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}
fn selected() -> Style {
    Style::default()
        .fg(ACCENT)
        .bg(PILL_ON)
        .add_modifier(Modifier::BOLD)
}
/// The ‹ value › chip. Giving it a background of its own is what makes a row
/// read as a CONTROL rather than as a sentence — the eye finds the thing it can
/// change without being told where to look.
fn pill(focused: bool) -> Style {
    if focused {
        Style::default()
            .fg(ACCENT)
            .bg(PILL_ON)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(ACCENT_DIM).bg(PILL)
    }
}

// ── Model ───────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Ssd,
    Hdd,
    Nvme,
}

impl Kind {
    const ALL: [Kind; 3] = [Kind::Ssd, Kind::Hdd, Kind::Nvme];

    fn tag(self) -> &'static str {
        match self {
            Kind::Ssd => "ssd",
            Kind::Hdd => "hdd",
            Kind::Nvme => "nvme",
        }
    }

    fn parse(s: &str) -> Option<Kind> {
        match s {
            "ssd" => Some(Kind::Ssd),
            "hdd" => Some(Kind::Hdd),
            "nvme" => Some(Kind::Nvme),
            _ => None,
        }
    }

    /// What the guest will see. This is the whole reason the choice exists, so
    /// it is spelled out on screen rather than left to the three-letter tag —
    /// and in the interface language, like everything else here. It was the one
    /// line left in English, which is exactly the kind of seam that makes a
    /// tool feel like two tools.
    fn guest(self, lang: Lang) -> &'static str {
        match self {
            Kind::Ssd => S(
                "/dev/sdX · SATA · ROTA=0 — читається як SSD",
                "/dev/sdX · SATA · ROTA=0 — reads as an SSD",
                "/dev/sdX · SATA · ROTA=0 — se lee como SSD",
            )
            .get(lang),
            Kind::Hdd => S(
                "/dev/sdX · SATA · ROTA=1 — читається як HDD на 7200 об/хв",
                "/dev/sdX · SATA · ROTA=1 — reads as a 7200rpm HDD",
                "/dev/sdX · SATA · ROTA=1 — se lee como HDD de 7200 rpm",
            )
            .get(lang),
            Kind::Nvme => S(
                "/dev/nvmeXn1 · NVMe · повний health-log SMART",
                "/dev/nvmeXn1 · NVMe · full SMART health log",
                "/dev/nvmeXn1 · NVMe · registro SMART completo",
            )
            .get(lang),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Kind::Ssd => "SSD  (SATA)",
            Kind::Hdd => "HDD  (SATA)",
            Kind::Nvme => "NVMe (M.2)",
        }
    }

    fn step(self, forward: bool) -> Kind {
        let i = Kind::ALL.iter().position(|k| *k == self).unwrap_or(0);
        let n = Kind::ALL.len();
        Kind::ALL[if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        }]
    }
}

/// How a size is written for qemu-img and shown on screen: whole gibibytes when
/// it divides evenly, mebibytes otherwise. `40G`, `512M` — never `0.5G`.
fn size_arg(mib: u32) -> String {
    if mib.is_multiple_of(1024) {
        format!("{}G", mib / 1024)
    } else {
        format!("{mib}M")
    }
}

/// The same, for a person: with a space, and with the unit spelled the way the
/// installer spells it.
fn size_text(mib: u32) -> String {
    if mib.is_multiple_of(1024) {
        format!("{} GiB", mib / 1024)
    } else {
        format!("{mib} MiB")
    }
}

/// A drive already in the folder.
#[derive(Clone)]
struct Existing {
    file: String,
    kind: Kind,
    bytes: u64,
}

/// One drive slot, whether it exists yet or not.
#[derive(Clone)]
struct Slot {
    n: u32,
    kind: Kind,
    /// Kept in MiB, not GiB: a size can be smaller than a gigabyte, and a
    /// unit that cannot say 512 MiB is a unit that forces a wrong answer.
    size_mib: u32,
    /// `None` for a drive that does not exist yet.
    existing: Option<Existing>,
    /// Marked for removal. Reversible until Enter — like the installer's own
    /// partition editor, where `d` marks and `d` again takes it back.
    doomed: bool,
}

impl Slot {
    fn is_new(&self) -> bool {
        self.existing.is_none()
    }
    /// Would this slot's medium change? Existing drives carry their kind in the
    /// file name, so changing it is a rename — the data is untouched, which is
    /// how the same installed system can be re-tested as an SSD and then as an
    /// HDD.
    fn renamed(&self) -> bool {
        self.existing.as_ref().is_some_and(|e| e.kind != self.kind) && !self.doomed
    }
    fn file_name(&self) -> String {
        format!("disk{}-{}.qcow2", self.n, self.kind.tag())
    }
}

/// What the emulated machine is, as opposed to what is plugged into it.
///
/// Kept in `vm/stand.conf` next to the drives so it survives between runs — a
/// setting you have to remember to pass on the command line is a setting that
/// is wrong every time you forget.
/// Which sound backend the guest gets.
///
/// It was a yes/no, which is the right question for somebody who does not know
/// and the wrong one for somebody who does: on a machine running PulseAudio you
/// may well want to test against ALSA, and "auto" cannot express that. So: auto
/// first, for the people the default is for, then the three QEMU actually takes,
/// then silence.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Audio {
    Auto,
    Pipewire,
    Pulse,
    Alsa,
    Off,
}

impl Audio {
    const ALL: [Audio; 5] = [
        Audio::Auto,
        Audio::Pipewire,
        Audio::Pulse,
        Audio::Alsa,
        Audio::Off,
    ];

    /// What goes in stand.conf, and — for the three real ones — the name QEMU
    /// knows the backend by. PulseAudio's is `pa`, not `pulseaudio`.
    fn tag(self) -> &'static str {
        match self {
            Audio::Auto => "auto",
            Audio::Pipewire => "pipewire",
            Audio::Pulse => "pa",
            Audio::Alsa => "alsa",
            Audio::Off => "off",
        }
    }

    fn parse(v: &str) -> Audio {
        match v {
            "pipewire" => Audio::Pipewire,
            "pa" | "pulse" | "pulseaudio" => Audio::Pulse,
            "alsa" => Audio::Alsa,
            "off" | "none" => Audio::Off,
            _ => Audio::Auto,
        }
    }

    fn step(self, forward: bool) -> Audio {
        let i = Audio::ALL.iter().position(|a| *a == self).unwrap_or(0);
        let n = Audio::ALL.len();
        Audio::ALL[if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        }]
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Machine {
    mem_gib: u32,
    cpus: u32,
    audio: Audio,
    /// The interface language. It lives with the machine settings because it is
    /// the same kind of thing: a choice made once that should still be there
    /// tomorrow.
    lang: Lang,
}

/// One line of the screen.
///
/// The rows used to be "slots, then the USB stick", indexed by arithmetic. With
/// the machine settings above them that arithmetic would have to be right in
/// five places, so what a row IS became a type instead.
#[derive(Clone, Copy, PartialEq)]
enum Row {
    Lang,
    Mem,
    Cpus,
    Audio,
    Drive(usize),
    /// The "+ add a drive" control.
    ///
    /// `a` always worked, but a keystroke nobody is shown is a keystroke nobody
    /// finds: the empty stand said "press a to add one" in grey, which reads as
    /// a caption rather than as something you can do. A row you can land on and
    /// press Enter is the same affordance the installer's partition editor uses.
    Add,
    Usb,
}

struct App {
    dir: String,
    slots: Vec<Slot>,
    /// Whether the USB stick should be attached when we are done.
    usb: bool,
    usb_before: bool,
    machine: Machine,
    machine_before: Machine,
    /// What the HOST has, so the ceilings on screen are real ones.
    host_mem_gib: u32,
    host_cpus: u32,
    cursor: usize,
    status: String,
    /// Which slot the size digits are currently going into. +/- nudges in
    /// steps, but a size is a number people know — 250, 12, 1000 — and making
    /// them press + twenty-five times to say so is not a size picker.
    /// The size box: `None` when closed, otherwise the slot it belongs to.
    sizing: Option<usize>,
    /// What has been typed into it, and in which unit.
    size_input: String,
    size_mib_unit: bool,
    /// True while the box still shows the number it OFFERED. The first digit
    /// then replaces it instead of extending it — otherwise typing 40 into a
    /// box showing 50 gives 5040, which is nobody's intention.
    size_fresh: bool,
}

impl App {
    /// Shorthand, so a translated string reads no worse than a literal did.
    fn t(&self, s: &S) -> &'static str {
        s.get(self.machine.lang)
    }

    fn row_list(&self) -> Vec<Row> {
        let mut v = vec![Row::Lang, Row::Mem, Row::Cpus, Row::Audio];
        for i in 0..self.slots.len() {
            v.push(Row::Drive(i));
        }
        v.push(Row::Add);
        v.push(Row::Usb);
        v
    }
    fn rows(&self) -> usize {
        self.row_list().len()
    }
    fn row(&self) -> Row {
        let list = self.row_list();
        list[self.cursor.min(list.len() - 1)]
    }
    /// The slot the cursor is on, if it is on one at all.
    fn slot_idx(&self) -> Option<usize> {
        match self.row() {
            Row::Drive(i) => Some(i),
            _ => None,
        }
    }

    /// Lowest unused slot number, so adding after deleting reuses the gap
    /// instead of climbing forever.
    fn free_slot(&self) -> u32 {
        let mut n = 1;
        while self.slots.iter().any(|s| s.n == n) {
            n += 1;
        }
        n
    }

    /// Everything that will actually happen, in the order the caller must do it.
    ///
    /// Deletes come FIRST: a rename can target a name a doomed drive still
    /// occupies (disk1-ssd → disk1-nvme while an old disk1-nvme is on its way
    /// out), and doing it the other way round would collide.
    fn plan(&self) -> Vec<String> {
        let mut out = Vec::new();
        for s in &self.slots {
            if let Some(e) = &s.existing {
                if s.doomed {
                    out.push(format!("delete {}", e.file));
                }
            }
        }
        for s in &self.slots {
            if s.doomed {
                continue;
            }
            match &s.existing {
                Some(e) if e.kind != s.kind => {
                    out.push(format!("rename {} {}", e.file, s.file_name()));
                }
                Some(_) => {}
                // Size still unanswered: the box is open on it. Showing
                // `create 2 hdd 0G` in the plan while somebody types is
                // offering to make a drive of nothing.
                None if s.size_mib == 0 => {}
                None => out.push(format!(
                    "create {} {} {}",
                    s.n,
                    s.kind.tag(),
                    size_arg(s.size_mib)
                )),
            }
        }
        if self.usb != self.usb_before {
            out.push(format!("usbkey {}", if self.usb { "on" } else { "off" }));
        }
        // Machine settings are written to stand.conf by the caller. Only what
        // CHANGED is emitted, so opening the picker and pressing Enter rewrites
        // nothing.
        if self.machine.mem_gib != self.machine_before.mem_gib {
            out.push(format!("set MEM_GIB {}", self.machine.mem_gib));
        }
        if self.machine.cpus != self.machine_before.cpus {
            out.push(format!("set CPUS {}", self.machine.cpus));
        }
        if self.machine.lang != self.machine_before.lang {
            out.push(format!("set UI_LANG {}", self.machine.lang.tag()));
        }
        if self.machine.audio != self.machine_before.audio {
            out.push(format!("set AUDIO {}", self.machine.audio.tag()));
        }
        out
    }
}

// ── Reading the folder ──────────────────────────────────────────────────────

fn human(bytes: u64) -> String {
    // KiB/MiB/GiB, like the installer — `16G` was ambiguous about which
    // thousand it meant, and this project has always been explicit.
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if v >= 10.0 || u == 0 {
        format!("{:.0} {}", v, UNITS[u])
    } else {
        format!("{:.1} {}", v, UNITS[u])
    }
}

fn scan(dir: &str) -> (Vec<Slot>, bool) {
    let mut slots: Vec<Slot> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            // disk<N>-<kind>.qcow2 — the medium is IN THE NAME, so there is no
            // metadata file that can fall out of step with what is really there.
            let Some(stem) = name.strip_suffix(".qcow2") else {
                continue;
            };
            let Some(rest) = stem.strip_prefix("disk") else {
                continue;
            };
            let Some((num, tag)) = rest.split_once('-') else {
                continue;
            };
            let (Ok(n), Some(kind)) = (num.parse::<u32>(), Kind::parse(tag)) else {
                continue;
            };
            let bytes = e.metadata().map(|m| m.len()).unwrap_or(0);
            slots.push(Slot {
                n,
                kind,
                // No size until the box says so: Esc then means "I changed my mind"
                // and takes the half-made drive with it.
                size_mib: 0,
                existing: Some(Existing {
                    file: name,
                    kind,
                    bytes,
                }),
                doomed: false,
            });
        }
    }
    slots.sort_by_key(|s| s.n);
    let usb = std::path::Path::new(dir).join("usbkey.img").is_file();
    (slots, usb)
}

/// What the host itself has: the ceiling every setting below is measured
/// against, so the numbers on screen mean something on THIS machine.
fn host_capacity() -> (u32, u32) {
    let mem_gib = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("MemTotal:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
        })
        .map(|kb| (kb / 1024 / 1024) as u32)
        .unwrap_or(8);
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(2);
    (mem_gib.max(1), cpus.max(1))
}

/// The saved machine settings, or what the script would have picked anyway.
///
/// The defaults are HALF the host, capped — the same rule the shell uses when
/// there is no file, so opening the picker on a fresh stand shows exactly what
/// would have happened without it rather than proposing something different.
fn read_machine(dir: &str, host_mem: u32, host_cpus: u32) -> Machine {
    let mut m = Machine {
        // 4 GiB, not half the machine. Half was a ceiling dressed up as a
        // default: on a 32 GiB desktop it handed the guest twelve, which no
        // install needs and which makes the host swap for nothing. An Artix
        // install is comfortable in four — and four is inside the band the
        // memory-tuning options were written for, so the common case also
        // happens to be the interesting one to test.
        mem_gib: (host_mem / 2).clamp(2, 4),
        cpus: (host_cpus / 2).clamp(2, 6),
        audio: Audio::Auto,
        // Ukrainian until told otherwise. The switcher is the first row on the
        // screen and names the alternatives, so nobody has to know a flag to
        // get out of a language they cannot read.
        lang: Lang::Uk,
    };
    let Ok(text) = std::fs::read_to_string(std::path::Path::new(dir).join("stand.conf")) else {
        return m;
    };
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim().trim_matches('"');
        match k.trim() {
            "MEM_GIB" => {
                if let Ok(n) = v.parse() {
                    m.mem_gib = n;
                }
            }
            "CPUS" => {
                if let Ok(n) = v.parse() {
                    m.cpus = n;
                }
            }
            "AUDIO" => m.audio = Audio::parse(v),
            // Not `LANG`: that is an environment variable everywhere else, and
            // a file that quietly redefines it is a trap for whoever reads the
            // stand.conf next.
            "UI_LANG" => {
                m.lang = match v {
                    "en" => Lang::En,
                    "es" => Lang::Es,
                    _ => Lang::Uk,
                }
            }
            _ => {}
        }
    }
    m
}

// ── Drawing ─────────────────────────────────────────────────────────────────

fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    // The whole background FIRST, before any layout: everything after this is
    // drawn onto a surface of our own rather than onto the terminal's.
    f.render_widget(Block::default().style(Style::default().bg(BG)), area);
    // The installer's own chrome: one rounded panel with the title on its top
    // edge, the keys on a line of their own underneath, and the action button
    // in the bottom-right corner. Someone who has just come from the installer
    // should not have to work out that this is the same project.
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(6), Constraint::Length(2)])
        .split(area);
    let (panel, footer) = (split[0], split[1]);

    let outer = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT_DIM))
        .style(Style::default().bg(PANEL))
        .title(Span::styled(format!(" {} ", app.t(&TITLE)), title_style()))
        .title_bottom(Span::styled(" ARTIX ", Style::default().fg(ACCENT_DIM)));
    let inner = outer.inner(panel);
    f.render_widget(outer, panel);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),                     // where + the promise
            Constraint::Length(app.rows() as u16 + 1), // the stand
            Constraint::Length(3),                     // what this row is
            Constraint::Min(3),                        // the plan
            Constraint::Length(3),                     // the action button
        ])
        .split(inner);

    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                format!(" {}", app.dir),
                Style::default().fg(DIM),
            )),
            Line::from(Span::styled(
                format!(" {}", app.t(&SUBTITLE)),
                Style::default().fg(MUTE),
            )),
        ]),
        rows[0],
    );

    draw_stand(f, rows[1], app);
    draw_detail(f, rows[2], app);
    draw_plan(f, rows[3], app);
    draw_action(f, rows[4], app);

    let keys = match app.row() {
        Row::Drive(_) => app.t(&KEYS_DRIVE),
        Row::Add => app.t(&KEYS_ADD),
        Row::Usb | Row::Audio | Row::Lang => app.t(&KEYS_TOGGLE),
        _ => app.t(&KEYS_VALUE),
    };
    draw_size_modal(f, panel, app);

    f.render_widget(
        Paragraph::new(vec![
            Line::from(key_spans(keys)),
            Line::from(Span::styled(
                format!(" {}", app.status),
                Style::default().fg(if app.status.starts_with('!') {
                    WARN
                } else {
                    OK
                }),
            )),
        ])
        // Wrapped, not clipped: at a narrow width the line lost its last pair
        // entirely — and the pair it loses is Esc, the way out.
        .wrap(ratatui::widgets::Wrap { trim: true }),
        footer,
    );
}

/// Key hints the way the installer draws them: the KEY in the accent colour,
/// what it does beside it in a quieter one, a divider between pairs.
///
/// A whole line in one grey is a line nobody reads — which is exactly what this
/// was, and it was the complaint.
fn key_spans(hint: &str) -> Vec<Span<'static>> {
    // The divider goes before EVERY pair, including the first. Without a bar on
    // the left the opening ←/→ hung off the edge with nothing holding it, while
    // every other pair sat between two — and the spacing was three blanks a
    // side, which spread eight pairs over two lines for no gain.
    let mut spans: Vec<Span> = Vec::new();
    for seg in hint.split('·').map(str::trim) {
        if seg.is_empty() {
            continue;
        }
        spans.push(Span::styled(" │ ", Style::default().fg(MUTE)));
        let bold = Style::default().fg(ACCENT).add_modifier(Modifier::BOLD);
        match seg.split_once(char::is_whitespace) {
            Some((key, rest)) => {
                spans.push(Span::styled(key.to_string(), bold));
                spans.push(Span::styled(
                    format!(" {}", rest.trim()),
                    Style::default().fg(DIM),
                ));
            }
            None => spans.push(Span::styled(seg.to_string(), bold)),
        }
    }
    spans
}

/// The bottom-right action, exactly where the installer keeps its Next button.
fn draw_action(f: &mut Frame, area: Rect, app: &App) {
    let label = app.t(&APPLY);
    let w = (label.chars().count() as u16 + 4).min(area.width);
    let x = area.x + area.width.saturating_sub(w);
    let rect = Rect::new(x, area.y, w, area.height.min(3));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT));
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(label, title_style())))
            .block(block)
            .alignment(ratatui::layout::Alignment::Center),
        rect,
    );
}

/// The backend's name as a person reads it. "auto" and "off" are words in the
/// interface language; the three real backends are product names and stay put.
fn audio_label(app: &App, a: Audio) -> String {
    match a {
        Audio::Auto => app.t(&AUTO).to_string(),
        Audio::Pipewire => "PipeWire".into(),
        Audio::Pulse => "PulseAudio".into(),
        Audio::Alsa => "ALSA".into(),
        Audio::Off => app.t(&OFF).to_string(),
    }
}

fn audio_note(app: &App, a: Audio) -> &'static str {
    match a {
        Audio::Auto => app.t(&SOUND_AUTO),
        Audio::Off => app.t(&SILENT),
        _ => app.t(&SOUND_FORCED),
    }
}

fn draw_stand(f: &mut Frame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = Vec::new();

    // One row-drawing shape for everything, so the machine settings and the
    // drives line up in the same three columns: name, value, consequence.
    let mut row = |focused: bool, name: &str, value: String, note: String| {
        let marker = if focused { "▍" } else { " " };
        let line = Line::from(vec![
            Span::styled(format!(" {marker} "), Style::default().fg(ACCENT)),
            Span::styled(
                format!("{name:<18}"),
                if focused {
                    selected()
                } else {
                    Style::default().fg(FG)
                },
            ),
            Span::styled(
                " ‹ ",
                Style::default().fg(if focused { ACCENT } else { MUTE }),
            ),
            Span::styled(format!(" {value:<11} "), pill(focused)),
            Span::styled(
                " › ",
                Style::default().fg(if focused { ACCENT } else { MUTE }),
            ),
            Span::styled(format!("  {note}"), Style::default().fg(MUTE)),
        ]);
        lines.push(line);
    };

    let cur = app.row();
    let m = app.machine;
    // The alternatives are listed, not hidden behind a keypress: someone who
    // opened this in a language they do not read needs to see the way out
    // without being able to read the hint that would explain it.
    let others: Vec<&str> = Lang::ALL
        .iter()
        .filter(|l| **l != m.lang)
        .map(|l| l.name())
        .collect();
    row(
        cur == Row::Lang,
        app.t(&ROW_LANG),
        m.lang.name().to_string(),
        others.join(" · "),
    );
    row(
        cur == Row::Mem,
        app.t(&ROW_MEM),
        format!("{} GiB", m.mem_gib),
        format!("{} {} GiB", app.t(&HOST_HAS), app.host_mem_gib),
    );
    row(
        cur == Row::Cpus,
        app.t(&ROW_CPUS),
        format!("{}", m.cpus),
        format!("{} {}", app.t(&HOST_HAS), app.host_cpus),
    );
    row(
        cur == Row::Audio,
        app.t(&ROW_SOUND),
        audio_label(app, m.audio),
        audio_note(app, m.audio).to_string(),
    );

    for (i, sl) in app.slots.iter().enumerate() {
        let focused = cur == Row::Drive(i);
        let (state, state_style) = if sl.doomed {
            (app.t(&WILL_DELETE).to_string(), Style::default().fg(WARN))
        } else if sl.is_new() {
            (
                format!("{} · {}", app.t(&NEW_TAG), size_text(sl.size_mib)),
                // Amber for a drive being ADDED, cyan for one merely kept:
                // "about to be created" and "already fine" must not look alike.
                Style::default().fg(NEW),
            )
        } else if sl.renamed() {
            let from = sl.existing.as_ref().map(|e| e.kind.tag()).unwrap_or("");
            (
                format!("{from} → {} ({})", sl.kind.tag(), app.t(&KEPT_RENAMED)),
                Style::default().fg(OK),
            )
        } else {
            let used = sl
                .existing
                .as_ref()
                .map(|e| human(e.bytes))
                .unwrap_or_default();
            (
                format!("{used} {}", app.t(&ON_DISK)),
                Style::default().fg(MUTE),
            )
        };
        // A doomed drive keeps its own struck-through name rather than going
        // through the shared row, so the deletion is visible at a glance.
        let marker = if focused { "▍" } else { " " };
        let name_style = if sl.doomed {
            Style::default()
                .fg(MUTE)
                .add_modifier(Modifier::CROSSED_OUT)
        } else if focused {
            selected()
        } else {
            Style::default().fg(FG)
        };
        let line = Line::from(vec![
            Span::styled(format!(" {marker} "), Style::default().fg(ACCENT)),
            Span::styled(format!("{:<18}", sl.file_name()), name_style),
            Span::styled(
                " ‹ ",
                Style::default().fg(if focused { ACCENT } else { MUTE }),
            ),
            Span::styled(format!(" {:<11} ", sl.kind.label()), pill(focused)),
            Span::styled(
                " › ",
                Style::default().fg(if focused { ACCENT } else { MUTE }),
            ),
            Span::styled(format!("  {state}"), state_style),
        ]);
        lines.push(line);
    }

    let focused = cur == Row::Add;
    let marker = if focused { "▍" } else { " " };
    lines.push(Line::from(vec![
        Span::styled(format!(" {marker} "), Style::default().fg(ACCENT)),
        Span::styled(
            format!("{:<18}", app.t(&ADD_DRIVE)),
            if focused {
                selected()
            } else {
                Style::default().fg(NEW)
            },
        ),
        Span::styled(format!(" {:<15} ", app.t(&ADD_ENTER)), pill(focused)),
    ]));

    let focused = cur == Row::Usb;
    let marker = if focused { "▍" } else { " " };
    let (word, style) = if app.usb {
        (app.t(&ATTACHED), Style::default().fg(OK))
    } else {
        (app.t(&NOT_ATTACHED), Style::default().fg(MUTE))
    };
    let usb_line = Line::from(vec![
        Span::styled(format!(" {marker} "), Style::default().fg(ACCENT)),
        Span::styled(
            format!("{:<18}", "usbkey.img"),
            if focused {
                selected()
            } else {
                Style::default().fg(FG)
            },
        ),
        Span::styled(
            " ‹ ",
            Style::default().fg(if focused { ACCENT } else { MUTE }),
        ),
        Span::styled(format!(" {:<11} ", app.t(&ROW_USBKEY)), pill(focused)),
        Span::styled(
            " › ",
            Style::default().fg(if focused { ACCENT } else { MUTE }),
        ),
        Span::styled(format!("  {word}"), style),
    ]);
    lines.push(usb_line);

    f.render_widget(Paragraph::new(lines), area);
}

/// What the focused row MEANS. The value alone is not an explanation, and
/// choosing between them is the entire point of the screen.
fn draw_detail(f: &mut Frame, area: Rect, app: &App) {
    let (head, sub) = match app.row() {
        Row::Lang => (app.t(&LANG_HEAD), app.t(&LANG_SUB)),
        Row::Mem => (app.t(&MEM_HEAD), app.t(&MEM_SUB)),
        Row::Cpus => (app.t(&CPU_HEAD), app.t(&CPU_SUB)),
        Row::Audio => (app.t(&SND_HEAD), app.t(&SND_SUB)),
        Row::Add => (app.t(&ADD_HEAD), app.t(&ADD_SUB)),
        Row::Usb => (app.t(&USB_HEAD), app.t(&USB_SUB)),
        Row::Drive(i) => {
            let Some(sl) = app.slots.get(i) else {
                return;
            };
            (
                sl.kind.guest(app.machine.lang),
                if sl.is_new() {
                    app.t(&DRIVE_NEW)
                } else if sl.renamed() {
                    app.t(&DRIVE_RENAMED)
                } else {
                    app.t(&DRIVE_KEPT)
                },
            )
        }
    };
    // The indent is in the RECT and not in the text: Wrap{trim} strips leading
    // whitespace from every line it produces, so a leading space in the string
    // survives on the first line and vanishes on the wrapped ones.
    let inset = Rect::new(
        area.x + 1,
        area.y,
        area.width.saturating_sub(2),
        area.height,
    );
    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(head, Style::default().fg(FG))),
            Line::from(Span::styled(sub, Style::default().fg(MUTE))),
        ])
        .wrap(ratatui::widgets::Wrap { trim: true }),
        inset,
    );
}

/// Show the plan before it runs. Same reason the installer shows one: the moment
/// to catch "that is not the drive I meant" is before anything is written.
fn draw_plan(f: &mut Frame, area: Rect, app: &App) {
    let plan = app.plan();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(MUTE))
        .title(Span::styled(app.t(&ON_ENTER), Style::default().fg(DIM)));
    let lines: Vec<Line> = if plan.is_empty() {
        vec![Line::from(Span::styled(
            format!(" {}", app.t(&NOTHING_TO_DO)),
            Style::default().fg(MUTE),
        ))]
    } else {
        plan.iter()
            .map(|p| {
                let style = if p.starts_with("delete") {
                    Style::default().fg(WARN)
                } else if p.starts_with("create") {
                    Style::default().fg(NEW)
                } else {
                    Style::default().fg(OK)
                };
                Line::from(Span::styled(format!(" {p}"), style))
            })
            .collect()
    };
    f.render_widget(Paragraph::new(lines).block(block), area);
}

// ── Keys ────────────────────────────────────────────────────────────────────

/// The size box.
///
/// Asked for in a box rather than typed into the row, because that is what the
/// partition editor does and this is the same question. ←/→ switches GiB and
/// MiB WITHOUT converting the number: 500 then MiB means 500 MiB, not half a
/// gibibyte — the box holds a size somebody is typing, not a measurement being
/// re-expressed.
fn draw_size_modal(f: &mut Frame, area: Rect, app: &App) {
    let Some(i) = app.sizing else { return };
    let kind = app.slots.get(i).map(|s| s.kind).unwrap_or(Kind::Ssd);

    let w = 72.min(area.width.saturating_sub(4)).max(24);
    let h = 9.min(area.height.saturating_sub(2)).max(6);
    let rect = Rect::new(
        area.x + area.width.saturating_sub(w) / 2,
        area.y + area.height.saturating_sub(h) / 2,
        w,
        h,
    );
    f.render_widget(Clear, rect);

    let unit = if app.size_mib_unit { "MiB" } else { "GiB" };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ACCENT))
        .style(Style::default().bg(PANEL))
        .title(Span::styled(
            format!(" {} — {} ", app.t(&SIZE_TITLE), kind.label().trim()),
            title_style(),
        ));
    let lines = vec![
        Line::from(Span::styled(app.t(&SIZE_PROMPT), Style::default().fg(FG))),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                format!(" {} ", app.size_input),
                Style::default()
                    .fg(ACCENT)
                    .bg(PILL_ON)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ‹ ", Style::default().fg(ACCENT_DIM)),
            Span::styled(
                unit,
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ›", Style::default().fg(ACCENT_DIM)),
        ]),
        Line::from(""),
        Line::from(key_spans(app.t(&SIZE_KEYS))),
    ];
    // The block and the text are rendered separately so the text can have a
    // margin: Wrap{trim} strips leading whitespace from every line it makes, so
    // a space inside the string survives on the first line and vanishes on the
    // wrapped ones. The indent has to be in the RECT.
    let inner = block.inner(rect).inner(ratatui::layout::Margin {
        horizontal: 1,
        vertical: 0,
    });
    f.render_widget(block, rect);
    f.render_widget(
        Paragraph::new(lines)
            // Wrapped, so the last pair — Esc, the way out — cannot fall off
            // the edge of a box that is narrower than its own hint.
            .wrap(ratatui::widgets::Wrap { trim: true }),
        inner,
    );
}

/// Open it for a slot, prefilled with what the slot already says.
fn open_size(app: &mut App, i: usize) {
    let mib = app.slots[i].size_mib;
    if mib == 0 {
        // A fresh drive: offer a sensible number rather than an empty box, but
        // do not write it anywhere until Enter.
        app.size_mib_unit = false;
        app.size_input = "50".into();
        app.size_fresh = true;
    } else {
        app.size_mib_unit = !mib.is_multiple_of(1024);
        app.size_input = if app.size_mib_unit {
            mib.to_string()
        } else {
            (mib / 1024).to_string()
        };
        app.size_fresh = true;
    }
    app.sizing = Some(i);
}

/// Returns true when the key was the box's.
fn size_modal_key(app: &mut App, code: KeyCode) -> bool {
    let Some(i) = app.sizing else { return false };
    match code {
        KeyCode::Esc => {
            // A slot that never got a size is a slot that was never wanted.
            if app.slots[i].size_mib == 0 {
                app.slots.remove(i);
                app.cursor = app.cursor.min(app.rows() - 1);
            }
            app.sizing = None;
        }
        KeyCode::Enter => {
            let n: u32 = app.size_input.parse().unwrap_or(0);
            if n == 0 {
                app.status = app.t(&SIZE_EMPTY).into();
                return true;
            }
            let mib = if app.size_mib_unit { n } else { n * 1024 };
            app.slots[i].size_mib = mib.clamp(64, 8 * 1024 * 1024);
            app.sizing = None;
        }
        KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
            app.size_mib_unit = !app.size_mib_unit;
        }
        KeyCode::Char(c) if c.is_ascii_digit() => {
            if app.size_fresh {
                app.size_input.clear();
                app.size_fresh = false;
            }
            if app.size_input.len() < 7 {
                app.size_input.push(c);
            }
        }
        KeyCode::Backspace => {
            // Backspace on the offer clears it whole: it was never typed, so
            // rubbing out one digit of it makes a number nobody chose.
            if app.size_fresh {
                app.size_input.clear();
                app.size_fresh = false;
            } else {
                app.size_input.pop();
            }
        }
        _ => {}
    }
    true
}

/// Add a drive and land the cursor on it, so the keys that follow edit the thing
/// that was just made rather than whatever was underneath.
fn add_drive(app: &mut App) {
    let n = app.free_slot();
    let kind = match app.slots.len() {
        0 => Kind::Ssd,
        1 => Kind::Hdd,
        _ => Kind::Nvme,
    };
    app.slots.push(Slot {
        n,
        kind,
        // No size until the box says so: Esc then means "I changed my mind"
        // and takes the half-made drive with it.
        size_mib: 0,
        existing: None,
        doomed: false,
    });
    app.slots.sort_by_key(|s| s.n);
    if let Some(i) = app.slots.iter().position(|s| s.n == n) {
        // Found by WHAT the row is: counting the rows above meant adding one
        // broke the jump, silently.
        if let Some(pos) = app.row_list().iter().position(|r| *r == Row::Drive(i)) {
            app.cursor = pos;
        }
        // And ask the size at once. Creating a drive and then hunting for where
        // to say how big it is was the complaint.
        open_size(app, i);
    }
}

/// Returns `Some(true)` to apply, `Some(false)` to cancel, `None` to carry on.
fn on_key(app: &mut App, code: KeyCode) -> Option<bool> {
    app.status.clear();
    // A box in front owns the keyboard — otherwise `d` deletes a drive behind it
    // while you are typing a number into it.
    if size_modal_key(app, code) {
        return None;
    }
    // ←/→ and +/- are the same gesture on every row that holds a value, so one
    // does not have to remember which control a row happens to use.
    let nudge = match code {
        KeyCode::Right | KeyCode::Char('+') | KeyCode::Char('=') => Some(true),
        KeyCode::Left | KeyCode::Char('-') | KeyCode::Char('_') => Some(false),
        _ => None,
    };

    match code {
        KeyCode::Esc | KeyCode::Char('q') => return Some(false),
        KeyCode::Enter if matches!(app.row(), Row::Drive(_)) => {
            if let Some(i) = app.slot_idx() {
                if app.slots[i].is_new() {
                    open_size(app, i);
                } else {
                    app.status = app.t(&ST_NO_RESIZE).into();
                }
            }
            return None;
        }
        KeyCode::Enter if app.row() == Row::Add => {
            add_drive(app);
            app.status = app.t(&ST_ADDED).into();
            return None;
        }
        KeyCode::Enter => return Some(true),
        KeyCode::Up | KeyCode::Char('k') => {
            app.cursor = app.cursor.saturating_sub(1);
            return None;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.cursor = (app.cursor + 1).min(app.rows() - 1);
            return None;
        }
        KeyCode::Char('a') | KeyCode::Char('A') => {
            add_drive(app);
            return None;
        }
        KeyCode::Char('d') | KeyCode::Char('D') => {
            if let Some(i) = app.slot_idx() {
                if app.slots[i].is_new() {
                    // Never created, so there is nothing to confirm.
                    app.slots.remove(i);
                    app.cursor = app.cursor.min(app.rows() - 1);
                } else {
                    app.slots[i].doomed = !app.slots[i].doomed;
                    app.status = if app.slots[i].doomed {
                        app.t(&ST_MARKED).into()
                    } else {
                        app.t(&ST_KEPT).into()
                    };
                }
            }
            return None;
        }
        _ => {}
    }

    let Some(up) = nudge else {
        // Space toggles whatever the row is, where a toggle makes sense.
        if code == KeyCode::Char(' ') {
            match app.row() {
                Row::Usb => app.usb = !app.usb,
                Row::Audio => app.machine.audio = app.machine.audio.step(true),
                Row::Lang => app.machine.lang = app.machine.lang.step(true),
                _ => {}
            }
        }
        return None;
    };

    match app.row() {
        // ←/→ on the add row adds one too: every other row here changes with
        // them, and a row that ignores them reads as broken.
        Row::Add => add_drive(app),
        Row::Lang => app.machine.lang = app.machine.lang.step(up),
        // 1 GiB steps below 8, then 2 — a machine is chosen precisely at the low
        // end (where the memory-tuning options live) and roughly at the high.
        Row::Mem => {
            // The step depends on WHERE THE MOVE LANDS, not on where it starts.
            // Keyed to the starting value it was asymmetric across the boundary:
            // 8 stepped down by two to 6, and 6 stepped back up by one to 7, so
            // ← then → did not return you to where you were.
            let v = app.machine.mem_gib;
            let next = if up {
                if v < 8 {
                    v + 1
                } else {
                    v + 2
                }
            } else if v <= 8 {
                v.saturating_sub(1)
            } else {
                v - 2
            };
            app.machine.mem_gib = next.clamp(1, app.host_mem_gib.max(1));
            if app.machine.mem_gib >= app.host_mem_gib {
                app.status = app.t(&ST_CEILING).into();
            }
        }
        Row::Cpus => {
            app.machine.cpus = if up {
                (app.machine.cpus + 1).min(app.host_cpus.max(1))
            } else {
                app.machine.cpus.saturating_sub(1).max(1)
            };
        }
        Row::Audio => app.machine.audio = app.machine.audio.step(up),
        Row::Usb => app.usb = !app.usb,
        Row::Drive(i) => {
            let sl = &mut app.slots[i];
            match code {
                // The medium.
                KeyCode::Left | KeyCode::Right => {
                    if sl.doomed {
                        app.status = app.t(&ST_DOOMED).into();
                    } else {
                        sl.kind = sl.kind.step(up);
                    }
                }
                // Size is not stepped here at all. Stepping it in tens meant
                // 10 GiB fell straight to 1 — the floor, wearing the costume of
                // a step — and typing digits into a row was worse. Enter opens
                // a box and asks, the way the partition editor does.
                _ => {}
            }
        }
    }
    None
}

// ── Wiring ──────────────────────────────────────────────────────────────────

fn run(app: &mut App) -> io::Result<bool> {
    enable_raw_mode()?;
    let mut err = io::stderr();
    execute!(err, EnterAlternateScreen)?;
    let mut term = Terminal::new(ratatui::backend::CrosstermBackend::new(err))?;

    let outcome = loop {
        term.draw(|f| draw(f, app))?;
        if let Event::Key(k) = event::read()? {
            if k.kind != KeyEventKind::Press {
                continue;
            }
            if let Some(apply) = on_key(app, k.code) {
                break apply;
            }
        }
    };

    disable_raw_mode()?;
    execute!(term.backend_mut(), LeaveAlternateScreen)?;
    term.show_cursor()?;
    Ok(outcome)
}

fn main() {
    let dir = match std::env::args().nth(1) {
        Some(d) => d,
        None => {
            eprintln!("usage: vm-setup <vm directory>");
            std::process::exit(2);
        }
    };
    let (slots, usb) = scan(&dir);
    let (host_mem_gib, host_cpus) = host_capacity();
    let machine = read_machine(&dir, host_mem_gib, host_cpus);
    let mut app = App {
        dir,
        slots,
        usb,
        usb_before: usb,
        machine,
        machine_before: machine,
        host_mem_gib,
        host_cpus,
        cursor: 0,
        status: String::new(),
        sizing: None,
        size_input: String::new(),
        size_mib_unit: false,
        size_fresh: false,
    };

    match run(&mut app) {
        Ok(true) => {
            for line in app.plan() {
                println!("{line}");
            }
        }
        Ok(false) => std::process::exit(1),
        Err(e) => {
            // A terminal we cannot drive is not a reason to lose the stand: say
            // so and exit as "cancelled", which the caller reads as "change
            // nothing" and falls back to its own questions.
            let _ = disable_raw_mode();
            eprintln!("vm-setup: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(n: u32, kind: Kind, existing: Option<&str>) -> Slot {
        Slot {
            n,
            kind,
            // No size until the box says so: Esc then means "I changed my mind"
            // and takes the half-made drive with it.
            size_mib: 0,
            existing: existing.map(|f| Existing {
                file: f.to_string(),
                kind: Kind::parse(f.rsplit('-').next().unwrap().trim_end_matches(".qcow2"))
                    .unwrap(),
                bytes: 0,
            }),
            doomed: false,
        }
    }

    /// A stand on a host with known capacity, so the ceilings in the tests are
    /// fixed rather than whatever the machine running them happens to have.
    fn app(slots: Vec<Slot>, usb: bool) -> App {
        let machine = Machine {
            mem_gib: 8,
            cpus: 4,
            audio: Audio::Auto,
            lang: Lang::Uk,
        };
        App {
            dir: "/tmp/vm".into(),
            slots,
            usb,
            usb_before: usb,
            machine,
            machine_before: machine,
            host_mem_gib: 16,
            host_cpus: 8,
            cursor: 0,
            status: String::new(),
            sizing: None,
            size_input: String::new(),
            size_mib_unit: false,
            size_fresh: false,
        }
    }

    /// Put the cursor on a row by what it IS, not by counting.
    fn focus(a: &mut App, want: Row) {
        a.cursor = a
            .row_list()
            .iter()
            .position(|r| *r == want)
            .expect("no such row");
    }

    /// Machine settings are planned only when they CHANGE, like everything
    /// else here — opening the picker and pressing Enter must rewrite nothing.
    #[test]
    fn the_machine_is_planned_only_when_it_changes() {
        let mut a = app(vec![], false);
        assert!(a.plan().is_empty());

        focus(&mut a, Row::Mem);
        on_key(&mut a, KeyCode::Left);
        assert_eq!(a.plan(), vec!["set MEM_GIB 7"]);
        // ← then → must land back where it started. It did not: the step size
        // was read off the value you were leaving, so 8 went down by two and
        // came back up by one.
        on_key(&mut a, KeyCode::Right);
        assert!(a.plan().is_empty(), "putting it back still planned a write");

        focus(&mut a, Row::Cpus);
        on_key(&mut a, KeyCode::Right);
        focus(&mut a, Row::Audio);
        on_key(&mut a, KeyCode::Char(' '));
        assert_eq!(a.plan(), vec!["set CPUS 5", "set AUDIO pipewire"]);
    }

    /// The guest cannot be given more than the host has. Handing it every core
    /// and every byte does not make the test faster; it makes the machine
    /// running it unusable, and on memory it invites the OOM killer.
    #[test]
    fn the_guest_cannot_outgrow_its_host() {
        let mut a = app(vec![], false);

        focus(&mut a, Row::Mem);
        for _ in 0..50 {
            on_key(&mut a, KeyCode::Right);
        }
        assert_eq!(a.machine.mem_gib, a.host_mem_gib);
        assert!(a.status.starts_with('!'), "the ceiling was silent");

        focus(&mut a, Row::Cpus);
        for _ in 0..50 {
            on_key(&mut a, KeyCode::Right);
        }
        assert_eq!(a.machine.cpus, a.host_cpus);
    }

    /// And it cannot be given nothing. A zero-core, zero-memory guest is not a
    /// configuration, it is a QEMU that refuses to start.
    #[test]
    fn the_guest_keeps_a_floor() {
        let mut a = app(vec![], false);
        focus(&mut a, Row::Mem);
        for _ in 0..50 {
            on_key(&mut a, KeyCode::Left);
        }
        assert_eq!(a.machine.mem_gib, 1);
        focus(&mut a, Row::Cpus);
        for _ in 0..50 {
            on_key(&mut a, KeyCode::Left);
        }
        assert_eq!(a.machine.cpus, 1);
    }

    /// Memory steps in 1 GiB below 8 GiB. The low end is where the choice
    /// actually matters — 2 GiB and 3 GiB are different machines as far as the
    /// installer's memory-tuning options are concerned — and stepping in twos
    /// would skip half of it.
    #[test]
    fn memory_is_fine_grained_where_it_matters() {
        let mut a = app(vec![], false);
        focus(&mut a, Row::Mem);
        a.machine.mem_gib = 4;
        on_key(&mut a, KeyCode::Right);
        assert_eq!(a.machine.mem_gib, 5, "1 GiB steps below 8");
        a.machine.mem_gib = 10;
        on_key(&mut a, KeyCode::Right);
        assert_eq!(a.machine.mem_gib, 12, "2 GiB steps above");
    }

    /// A saved stand.conf is what comes back, and a missing one falls back to
    /// half the host — the same rule the shell uses, so the picker shows what
    /// would have happened anyway rather than proposing something else.
    #[test]
    fn settings_survive_being_written_and_read_back() {
        let dir = std::env::temp_dir().join(format!("vm-setup-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("stand.conf");

        // 4 GiB even on a 16 GiB host: the default is a sensible size for an
        // install, not a share of whatever the machine happens to have.
        let fresh = read_machine(dir.to_str().unwrap(), 16, 8);
        assert_eq!((fresh.mem_gib, fresh.cpus), (4, 4));
        // And on a small host it is half of it rather than more than there is.
        let small = read_machine(dir.to_str().unwrap(), 4, 2);
        assert_eq!(small.mem_gib, 2);
        assert_eq!(fresh.audio, Audio::Auto);

        std::fs::write(&path, "MEM_GIB=3\nCPUS=2\nAUDIO=off\n").expect("write");
        let saved = read_machine(dir.to_str().unwrap(), 16, 8);
        assert_eq!((saved.mem_gib, saved.cpus), (3, 2));
        assert_eq!(saved.audio, Audio::Off);

        // Junk in the file must not throw away the settings around it.
        std::fs::write(&path, "MEM_GIB=nonsense\nCPUS=2\n").expect("write");
        let partial = read_machine(dir.to_str().unwrap(), 16, 8);
        assert_eq!(partial.cpus, 2);
        assert_eq!(partial.mem_gib, 4, "a bad line took a good one with it");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A size is a number people already know. Typing 250 must mean 250, not
    /// twenty-five presses of +, and the first digit must REPLACE the default
    /// Digits on a drive that already exists change nothing and say why —
    /// resizing a qcow2 under a guest that has already partitioned it is not a
    /// A fresh stand offers NOTHING. Someone short of space who wants to test
    /// one scenario on one drive should not have to delete two they never asked
    /// for — and drives are the one thing here that costs real gigabytes.
    #[test]
    fn a_fresh_stand_creates_nothing_by_itself() {
        let a = app(vec![], false);
        assert!(a.slots.is_empty());
        assert!(!a.usb, "a stick nobody asked for");
        assert!(a.plan().is_empty(), "a fresh stand planned something");
    }

    /// The switcher names every language IN ITSELF and cycles both ways.
    /// "Ukrainian" is no use to somebody who only reads Ukrainian, and a
    /// switcher you can only walk forwards is one you can overshoot.
    #[test]
    fn the_language_switcher_names_and_cycles() {
        assert_eq!(Lang::Uk.name(), "Українська");
        assert_eq!(Lang::En.name(), "English");
        assert_eq!(Lang::Es.name(), "Español");

        let mut a = app(vec![], false);
        assert_eq!(a.machine.lang, Lang::Uk, "the default is not Ukrainian");
        focus(&mut a, Row::Lang);

        on_key(&mut a, KeyCode::Right);
        assert_eq!(a.machine.lang, Lang::En);
        assert_eq!(a.plan(), vec!["set UI_LANG en"]);
        on_key(&mut a, KeyCode::Right);
        assert_eq!(a.machine.lang, Lang::Es);
        on_key(&mut a, KeyCode::Right);
        assert_eq!(a.machine.lang, Lang::Uk, "it does not wrap round");
        assert!(a.plan().is_empty(), "back where it started, still planning");

        on_key(&mut a, KeyCode::Left);
        assert_eq!(a.machine.lang, Lang::Es, "left does not go back");
    }

    /// Switching the language switches the whole interface, not a label or two.
    #[test]
    fn every_string_really_changes_with_the_language() {
        let mut a = app(vec![], false);
        for pair in [
            (&TITLE, "TITLE"),
            (&SUBTITLE, "SUBTITLE"),
            (&MEM_HEAD, "MEM_HEAD"),
            (&KEYS_VALUE, "KEYS_VALUE"),
            (&APPLY, "APPLY"),
        ] {
            let (msg, name) = pair;
            a.machine.lang = Lang::Uk;
            let uk = a.t(msg);
            a.machine.lang = Lang::En;
            let en = a.t(msg);
            a.machine.lang = Lang::Es;
            let es = a.t(msg);
            assert!(
                !uk.is_empty() && !en.is_empty() && !es.is_empty(),
                "{name}: empty"
            );
            assert_ne!(uk, en, "{name}: Ukrainian and English are the same string");
        }
    }

    /// Sound is a CHOICE, not a switch: auto for whoever does not want to
    /// think about it, and the three backends QEMU actually takes for whoever
    /// does. PulseAudio's QEMU name is `pa`, which is the one that matters.
    #[test]
    fn the_sound_row_offers_every_backend() {
        let mut a = app(vec![], false);
        focus(&mut a, Row::Audio);
        assert_eq!(a.machine.audio, Audio::Auto, "auto is not the default");

        let mut seen = Vec::new();
        for _ in 0..Audio::ALL.len() {
            seen.push(a.machine.audio.tag());
            on_key(&mut a, KeyCode::Right);
        }
        assert_eq!(seen, vec!["auto", "pipewire", "pa", "alsa", "off"]);
        assert_eq!(a.machine.audio, Audio::Auto, "the list does not come round");

        // And what a person reads is a product name, not a tag.
        assert_eq!(audio_label(&a, Audio::Pulse), "PulseAudio");
        assert_eq!(audio_label(&a, Audio::Alsa), "ALSA");
    }

    /// The add-drive row is a CONTROL, and Enter on it must add a drive rather
    /// than apply the plan and leave. A keystroke nobody is shown is a
    /// keystroke nobody finds, which is what the grey "press a" caption was.
    #[test]
    fn enter_on_the_add_row_adds_a_drive_instead_of_applying() {
        let mut a = app(vec![], false);
        focus(&mut a, Row::Add);
        assert_eq!(
            on_key(&mut a, KeyCode::Enter),
            None,
            "Enter on the add row ended the dialog"
        );
        assert_eq!(a.slots.len(), 1);
        assert!(
            matches!(a.row(), Row::Drive(0)),
            "the cursor did not land on the new drive"
        );
        assert!(a.sizing.is_some(), "it did not go on to ask the size");

        // On any other row Enter still means apply — once the box is out of the
        // way, because a box in front owns the keyboard.
        on_key(&mut a, KeyCode::Enter); // accept the offered size
        focus(&mut a, Row::Usb);
        assert_eq!(on_key(&mut a, KeyCode::Enter), Some(true));
    }

    /// A size is ASKED FOR, in a box, the way the partition editor asks. It was
    /// typed straight into the row and stepped with +/-, and both were wrong:
    /// stepping in tens dropped 10 GiB to 1 (the floor wearing a step's
    /// costume), and a row you can type numbers into does not look like one.
    #[test]
    fn the_size_is_asked_for_in_a_box() {
        let mut a = app(vec![], false);
        focus(&mut a, Row::Add);
        on_key(&mut a, KeyCode::Enter);
        assert!(a.sizing.is_some(), "adding a drive did not ask its size");
        assert_eq!(a.size_input, "50", "the box opened empty");

        // Type over the offer, confirm, and it lands as GiB.
        a.size_input.clear();
        for d in ['4', '0'] {
            on_key(&mut a, KeyCode::Char(d));
        }
        on_key(&mut a, KeyCode::Enter);
        assert!(a.sizing.is_none(), "the box did not close");
        assert_eq!(a.plan(), vec!["create 1 ssd 40G"]);
    }

    /// The unit switches WITHOUT converting the number: 500 then MiB means 500
    /// MiB, because the box holds a size somebody is typing, not a measurement
    /// being re-expressed. Same rule as the installer's own size box.
    #[test]
    fn switching_the_unit_does_not_convert_the_number() {
        let mut a = app(vec![], false);
        focus(&mut a, Row::Add);
        on_key(&mut a, KeyCode::Enter);
        a.size_input = "512".into();
        on_key(&mut a, KeyCode::Right); // GiB -> MiB
        on_key(&mut a, KeyCode::Enter);
        assert_eq!(a.plan(), vec!["create 1 ssd 512M"]);
    }

    /// Esc in the box takes the half-made drive with it. Otherwise pressing
    /// Enter on "add" and changing your mind leaves a drive nobody asked for.
    #[test]
    fn escaping_the_box_undoes_the_drive() {
        let mut a = app(vec![], false);
        focus(&mut a, Row::Add);
        on_key(&mut a, KeyCode::Enter);
        assert_eq!(a.slots.len(), 1);
        on_key(&mut a, KeyCode::Esc);
        assert!(a.slots.is_empty(), "the abandoned drive stayed");
        assert!(a.plan().is_empty());
    }

    /// An image that already exists is not resized: growing a qcow2 leaves the
    /// guest's partition table behind and shrinking one destroys data. Say so
    /// rather than opening a box that cannot deliver.
    #[test]
    fn an_existing_image_is_not_resized_and_says_why() {
        let mut a = app(vec![slot(1, Kind::Ssd, Some("disk1-ssd.qcow2"))], false);
        focus(&mut a, Row::Drive(0));
        on_key(&mut a, KeyCode::Enter);
        assert!(
            a.sizing.is_none(),
            "it opened the size box for a real image"
        );
        assert!(a.status.starts_with('!'));
        assert!(a.plan().is_empty());
    }

    /// The number the box offers is an OFFER: the first digit REPLACES it.
    /// Typing 40 into a box showing 50 gave 5040, which is nobody's intention —
    /// and Backspace on an offer clears the lot, because rubbing out one digit
    /// of a number you never typed leaves one you never chose either.
    #[test]
    fn the_offered_size_is_replaced_by_what_you_type() {
        let mut a = app(vec![], false);
        focus(&mut a, Row::Add);
        on_key(&mut a, KeyCode::Enter);
        assert_eq!(a.size_input, "50");

        on_key(&mut a, KeyCode::Char('4'));
        assert_eq!(a.size_input, "4", "the offer was extended, not replaced");
        on_key(&mut a, KeyCode::Char('0'));
        assert_eq!(a.size_input, "40", "the second digit replaced too");
        on_key(&mut a, KeyCode::Enter);
        assert_eq!(a.plan(), vec!["create 1 ssd 40G"]);

        // Backspace on a fresh offer empties it outright.
        focus(&mut a, Row::Add);
        on_key(&mut a, KeyCode::Enter);
        on_key(&mut a, KeyCode::Backspace);
        assert_eq!(a.size_input, "");
    }

    /// Every hint pair sits between two dividers, the first one included. The
    /// opening ←/→ used to hang off the left edge with nothing holding it.
    #[test]
    fn every_hint_pair_is_fenced_on_both_sides() {
        let spans = key_spans("←/→ носій · a додати");
        assert_eq!(
            spans.first().map(|s| s.content.as_ref()),
            Some(" │ "),
            "the first pair has no divider before it"
        );
        assert_eq!(
            spans.iter().filter(|s| s.content == " │ ").count(),
            2,
            "one divider per pair"
        );
    }

    /// It DRAWS, at every size a person might have.
    ///
    /// The rig is used on the developer's terminal and on a bare VT, and
    /// ratatui panics rather than clips when a layout asks for more rows than
    /// exist. A picker that crashes the moment the window is small is worse than
    /// no picker, because the fallback questions never get a chance to run.
    #[test]
    fn it_draws_at_every_size_and_on_every_row() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut a = app(
            vec![
                slot(1, Kind::Ssd, Some("disk1-ssd.qcow2")),
                slot(2, Kind::Hdd, Some("disk2-hdd.qcow2")),
                slot(3, Kind::Nvme, None),
            ],
            true,
        );
        a.slots[1].doomed = true;
        a.slots[0].kind = Kind::Nvme; // a pending rename, so that path draws too

        for (w, h) in [(80, 24), (59, 15), (120, 40), (40, 10)] {
            for row in 0..a.rows() {
                a.cursor = row;
                let mut term = Terminal::new(TestBackend::new(w, h)).expect("backend");
                term.draw(|f| draw(f, &a))
                    .unwrap_or_else(|e| panic!("{w}x{h}, row {row}: {e}"));
            }
        }

        // And with nothing at all, which is the state it opens in on a fresh
        // checkout — the case the empty-folder prompt exists for.
        let empty = app(vec![], false);
        let mut term = Terminal::new(TestBackend::new(80, 24)).expect("backend");
        term.draw(|f| draw(f, &empty)).expect("empty stand");
    }

    /// Nothing chosen, nothing planned. A tool that always "does something" is
    /// one nobody dares open on a stand they care about.
    #[test]
    fn an_untouched_stand_plans_nothing() {
        let a = app(vec![slot(1, Kind::Ssd, Some("disk1-ssd.qcow2"))], true);
        assert!(a.plan().is_empty());
    }

    /// Changing the medium of an EXISTING drive is a rename, never a re-create:
    /// the whole point is re-testing the same installed system as another kind
    /// of hardware.
    #[test]
    fn changing_the_medium_keeps_the_image() {
        let mut a = app(vec![slot(1, Kind::Ssd, Some("disk1-ssd.qcow2"))], false);
        a.slots[0].kind = Kind::Nvme;
        assert_eq!(a.plan(), vec!["rename disk1-ssd.qcow2 disk1-nvme.qcow2"]);
    }

    /// Deletes are planned before renames, because a rename can target the name
    /// a doomed drive is still holding.
    #[test]
    fn deletes_come_before_renames_so_names_cannot_collide() {
        let mut a = app(
            vec![
                slot(1, Kind::Ssd, Some("disk1-ssd.qcow2")),
                slot(2, Kind::Nvme, Some("disk2-nvme.qcow2")),
            ],
            false,
        );
        a.slots[1].doomed = true;
        a.slots[0].kind = Kind::Nvme;
        let plan = a.plan();
        assert_eq!(plan[0], "delete disk2-nvme.qcow2");
        assert_eq!(plan[1], "rename disk1-ssd.qcow2 disk1-nvme.qcow2");
    }

    /// A drive marked for deletion can be taken back. It may hold an installed
    /// system, so one keypress must never be final.
    #[test]
    fn deletion_is_reversible_until_enter() {
        let mut a = app(vec![slot(1, Kind::Hdd, Some("disk1-hdd.qcow2"))], false);
        focus(&mut a, Row::Drive(0));
        on_key(&mut a, KeyCode::Char('d'));
        assert_eq!(a.plan(), vec!["delete disk1-hdd.qcow2"]);
        on_key(&mut a, KeyCode::Char('d'));
        assert!(a.plan().is_empty(), "d did not take the deletion back");
    }

    /// A slot that was never created just disappears — there is nothing to
    /// confirm and nothing to plan.
    #[test]
    fn dropping_a_new_slot_plans_nothing() {
        let mut a = app(vec![], false);
        on_key(&mut a, KeyCode::Char('a'));
        on_key(&mut a, KeyCode::Enter); // accept the size the box offers
        assert_eq!(a.plan().len(), 1);
        on_key(&mut a, KeyCode::Char('d'));
        assert!(a.plan().is_empty());
        assert!(a.slots.is_empty());
    }

    /// Adding after deleting reuses the freed number rather than climbing, so a
    /// stand of two drives is disk1 and disk2 however much it was rearranged.
    #[test]
    fn a_new_drive_takes_the_lowest_free_number() {
        let mut a = app(
            vec![
                slot(1, Kind::Ssd, Some("disk1-ssd.qcow2")),
                slot(3, Kind::Nvme, Some("disk3-nvme.qcow2")),
            ],
            false,
        );
        on_key(&mut a, KeyCode::Char('a'));
        on_key(&mut a, KeyCode::Enter); // answer the size box
        assert!(a.plan().iter().any(|p| p.starts_with("create 2 ")));
    }

    /// The USB key is a choice like any other, and only a CHANGE is planned.
    #[test]
    fn the_usb_key_plans_only_when_it_changes() {
        let mut a = app(vec![], false);
        focus(&mut a, Row::Usb);
        on_key(&mut a, KeyCode::Char(' '));
        assert_eq!(a.plan(), vec!["usbkey on"]);
        on_key(&mut a, KeyCode::Char(' '));
        assert!(a.plan().is_empty());
    }
}
