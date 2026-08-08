//! Drive-wear (TBW) inspector, reachable from the mode chooser (fourth entry).
//!
//! Shows how much has been written to each drive over its life — the number an
//! SSD's endurance is rated in (TBW, terabytes written) — plus the wear percent
//! and power-on hours. Useful before an install: a drive near its endurance is
//! a poor place to put a new system, and a zero-fill on a tired SSD is exactly
//! the mistake the wipe picker already guards against.
//!
//! One tool covers every medium: `smartctl -j` reads SMART as JSON from SATA
//! SSDs (seen as sdX), NVMe drives, and spinning HDDs alike. TBW comes from the
//! ATA attribute `Total_LBAs_Written` (241) × the logical block size for SATA,
//! and from `Data Units Written` × 512000 for NVMe; an HDD that reports neither
//! shows "n/a" (spinning disks have no endurance budget to track). Parsing is
//! pure and unit-tested; only the scan shells out — in a worker thread, so a
//! slow-spinning HDD never freezes the UI.

use crate::app::{App, Screen};
use crate::i18n::t;
use crate::theme;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use serde::Deserialize;

/// Why a drive produced no wear data. Kept as a KIND rather than a message so
/// the text comes from i18n — and so the three causes read differently, because
/// they need different things from the user: run as root, use a drive that has
/// SMART at all, or install smartmontools.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WearError {
    /// smartctl could not open the device: not running as root.
    NeedsRoot,
    /// The device has no SMART to read. Every VIRTUAL disk lands here —
    /// virtio (`/dev/vdX`) exposes no SMART, so a QEMU guest shows this for
    /// every drive unless it is given SATA/SCSI or NVMe disks instead.
    NoSmart,
    /// `smartctl` is not installed (smartmontools missing from the ISO).
    NoTool,
    /// Anything else — smartctl's own words, shown verbatim as a diagnostic.
    Other(String),
}

/// One drive's wear summary, parsed from `smartctl -j -A -i`.
#[derive(Clone, Debug, Default)]
pub struct DriveWear {
    pub dev: String,
    pub model: String,
    /// "NVMe SSD", "SSD" or "HDD".
    pub kind: String,
    /// Total bytes written over the drive's life, when the drive reports it.
    pub tbw_bytes: Option<u64>,
    /// Endurance used, in percent (SSD only; HDDs have none).
    pub wear_percent: Option<i64>,
    pub power_on_hours: Option<u64>,
    /// Set when smartctl could not read this drive.
    pub error: Option<WearError>,
}

// ── serde shapes for the subset of `smartctl -j` we read ────────────────────
#[derive(Deserialize, Default)]
struct SmartJson {
    #[serde(default)]
    smartctl: Option<SmartctlMeta>,
    #[serde(default)]
    logical_block_size: Option<u64>,
    #[serde(default)]
    model_name: Option<String>,
    #[serde(default)]
    power_on_time: Option<PowerOnTime>,
    #[serde(default)]
    endurance_used: Option<EnduranceUsed>,
    #[serde(default)]
    ata_smart_attributes: Option<AtaAttrs>,
    #[serde(default)]
    nvme_smart_health_information_log: Option<NvmeLog>,
}
/// smartctl's own report on the run: `exit_status` and any error messages. This
/// is the ONLY place the real reason for an empty reading lives — the rest of
/// the document is simply absent when the read failed.
#[derive(Deserialize, Default)]
struct SmartctlMeta {
    #[serde(default)]
    messages: Vec<SmartctlMessage>,
}
#[derive(Deserialize, Default)]
struct SmartctlMessage {
    #[serde(default)]
    string: String,
}

#[derive(Deserialize, Default)]
struct PowerOnTime {
    #[serde(default)]
    hours: Option<u64>,
}
#[derive(Deserialize, Default)]
struct EnduranceUsed {
    #[serde(default)]
    current_percent: Option<i64>,
}
#[derive(Deserialize, Default)]
struct AtaAttrs {
    #[serde(default)]
    table: Vec<AtaAttr>,
}
#[derive(Deserialize, Default)]
struct AtaAttr {
    id: u32,
    #[serde(default)]
    raw: Option<AtaRaw>,
}
#[derive(Deserialize, Default)]
struct AtaRaw {
    #[serde(default)]
    value: Option<u64>,
}
#[derive(Deserialize, Default)]
struct NvmeLog {
    #[serde(default)]
    data_units_written: Option<u64>,
    #[serde(default)]
    percentage_used: Option<i64>,
    #[serde(default)]
    power_on_hours: Option<u64>,
}

/// How the medium is labelled, from the lsblk scan (never guessed from output).
fn drive_kind(rota: bool, tran: &str) -> String {
    if rota {
        "HDD".into()
    } else if tran == "nvme" {
        "NVMe SSD".into()
    } else {
        "SSD".into()
    }
}

/// Parse a `smartctl -j -A -i` document into a wear summary. Pure, so the three
/// medium shapes (SATA / NVMe / HDD) can be unit-tested against real fixtures.
pub fn parse_smart(json: &str, dev: &str, rota: bool, tran: &str) -> DriveWear {
    let kind = drive_kind(rota, tran);
    let parsed: SmartJson = serde_json::from_str(json).unwrap_or_default();
    let model = parsed.model_name.clone().unwrap_or_default();

    if let Some(nvme) = &parsed.nvme_smart_health_information_log {
        // NVMe: a "data unit" is 1000 × 512 bytes by the spec.
        return DriveWear {
            dev: dev.into(),
            model,
            kind,
            tbw_bytes: nvme.data_units_written.map(|d| d.saturating_mul(512_000)),
            wear_percent: nvme.percentage_used,
            power_on_hours: nvme
                .power_on_hours
                .or_else(|| parsed.power_on_time.as_ref().and_then(|p| p.hours)),
            error: None,
        };
    }

    // SATA (and any HDD that tracks it): attribute 241, in logical blocks.
    let lbs = parsed.logical_block_size.unwrap_or(512);
    let lbaw = parsed.ata_smart_attributes.as_ref().and_then(|a| {
        a.table
            .iter()
            .find(|x| x.id == 241)
            .and_then(|x| x.raw.as_ref())
            .and_then(|r| r.value)
    });
    let mut w = DriveWear {
        dev: dev.into(),
        model,
        kind,
        tbw_bytes: lbaw.map(|l| l.saturating_mul(lbs)),
        wear_percent: parsed
            .endurance_used
            .as_ref()
            .and_then(|e| e.current_percent),
        power_on_hours: parsed.power_on_time.as_ref().and_then(|p| p.hours),
        error: None,
    };
    // Nothing at all came back — say WHY, in smartctl's own words. "no data" on
    // its own sends the user hunting for a fault that may not exist: a virtio
    // disk simply has no SMART, and that is not a problem to fix.
    if w.tbw_bytes.is_none()
        && w.power_on_hours.is_none()
        && w.wear_percent.is_none()
        && w.model.is_empty()
    {
        w.error = Some(classify_smart_failure(parsed.smartctl.as_ref()));
    }
    w
}

/// Turn smartctl's error messages into a cause. Matching is on the stable parts
/// of its wording (verified against smartctl 7.5): a refused open is
/// "Permission denied"; a device with no SMART at all is "Unable to detect
/// device type" (what every virtio disk reports).
fn classify_smart_failure(meta: Option<&SmartctlMeta>) -> WearError {
    let msg = meta
        .and_then(|m| m.messages.first())
        .map(|m| m.string.clone())
        .unwrap_or_default();
    let low = msg.to_lowercase();
    if low.contains("permission denied") || low.contains("operation not permitted") {
        WearError::NeedsRoot
    } else if low.contains("unable to detect device type")
        || low.contains("not supported")
        || low.contains("unknown device type")
        || msg.is_empty()
    {
        WearError::NoSmart
    } else {
        // Keep smartctl's own line: an unforeseen cause is more useful verbatim
        // than flattened into a guess.
        WearError::Other(msg)
    }
}

/// Bytes → a human TBW string. Decimal TB, the unit endurance is rated in.
fn fmt_tbw(bytes: u64) -> String {
    const TB: f64 = 1e12;
    const GB: f64 = 1e9;
    let b = bytes as f64;
    if b >= TB {
        format!("{:.1} TB", b / TB)
    } else {
        format!("{:.0} GB", b / GB)
    }
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    // Lazy scan: kicked off the first time the screen is drawn (and after 'r').
    // The work runs in a thread; results arrive via `tick`.
    if !app.tbwtest_scanned && app.tbwtest_rx.is_none() {
        run(app);
        app.tbwtest_scanned = true;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // hint
            Constraint::Min(0),    // table
            Constraint::Length(1), // action line
        ])
        .spacing(1)
        .split(area);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            t(app.lang, "tbw.hint"),
            theme::dim(),
        )))
        .wrap(ratatui::widgets::Wrap { trim: true }),
        rows[0],
    );

    let mut body: Vec<Line> = Vec::new();
    if app.tbwtest_running && app.tbwtest_rows.is_empty() {
        body.push(Line::from(Span::styled(
            t(app.lang, "tbw.scanning"),
            theme::accent(),
        )));
    } else if app.tbwtest_rows.is_empty() {
        body.push(Line::from(Span::styled(
            t(app.lang, "tbw.none"),
            theme::dim(),
        )));
    } else {
        // Header row.
        body.push(Line::from(Span::styled(
            format!(
                "  {:<13}{:<10}{:<20}{:>12}{:>8}{:>10}",
                t(app.lang, "tbw.col_dev"),
                t(app.lang, "tbw.col_kind"),
                t(app.lang, "tbw.col_model"),
                t(app.lang, "tbw.col_tbw"),
                t(app.lang, "tbw.col_wear"),
                t(app.lang, "tbw.col_hours"),
            ),
            theme::heading(),
        )));
        for d in &app.tbwtest_rows {
            if let Some(err) = &d.error {
                let msg = match err {
                    WearError::NeedsRoot => t(app.lang, "tbw.err_root"),
                    WearError::NoSmart => t(app.lang, "tbw.err_nosmart"),
                    WearError::NoTool => t(app.lang, "tbw.err_notool"),
                    WearError::Other(s) => s.clone(),
                };
                body.push(Line::from(vec![
                    Span::styled(format!("  {:<13}{:<10}", d.dev, d.kind), theme::dim()),
                    Span::styled(msg, theme::warn()),
                ]));
                continue;
            }
            let tbw = d
                .tbw_bytes
                .map(fmt_tbw)
                .unwrap_or_else(|| t(app.lang, "tbw.na"));
            let wear = d
                .wear_percent
                .map(|w| format!("{w}%"))
                .unwrap_or_else(|| t(app.lang, "tbw.na"));
            let hours = d
                .power_on_hours
                .map(|h| format!("{h}"))
                .unwrap_or_else(|| t(app.lang, "tbw.na"));
            let model: String = d.model.chars().take(19).collect();
            // A worn SSD (endurance ≥ 80 %) is called out in the warn colour.
            let style = match d.wear_percent {
                Some(w) if w >= 80 => theme::warn(),
                _ => theme::normal(),
            };
            body.push(Line::from(Span::styled(
                format!(
                    "  {:<13}{:<10}{:<20}{:>12}{:>8}{:>10}",
                    d.dev, d.kind, model, tbw, wear, hours
                ),
                style,
            )));
        }
    }

    f.render_widget(
        Paragraph::new(body)
            .block(Block::default().borders(Borders::ALL))
            .wrap(ratatui::widgets::Wrap { trim: false }),
        rows[1],
    );

    let action = if app.tbwtest_running {
        t(app.lang, "tbw.running")
    } else {
        t(app.lang, "tbw.done")
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(action, theme::dim()))),
        rows[2],
    );

    app.can_advance = false;
}

pub fn footer_hint(app: &App) -> String {
    t(app.lang, "tbw.footer")
}

pub fn handle_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Enter | KeyCode::Char('r') | KeyCode::Char('R') => {
            if app.tbwtest_running {
                return; // a scan is already in flight
            }
            app.tbwtest_scanned = false; // re-scan on the next draw
        }
        KeyCode::Esc => app.goto(Screen::Mode),
        _ => {}
    }
}

/// Read every whole disk's SMART data in a worker thread. Same thread+channel
/// shape as the Wi-Fi test — the UI never blocks on smartctl (a sleeping HDD can
/// take seconds to answer).
fn run(app: &mut App) {
    if app.tbwtest_rx.is_some() {
        return;
    }
    app.tbwtest_running = true;
    app.tbwtest_rows.clear();

    let disks: Vec<(String, bool, String)> = crate::screens::disk::disks()
        .iter()
        .map(|d| (d.path.clone(), d.rota, d.tran.clone()))
        .collect();

    let (tx, rx) = crossbeam_channel::bounded(1);
    app.tbwtest_rx = Some(rx);

    std::thread::spawn(move || {
        let mut out = Vec::new();
        for (dev, rota, tran) in disks {
            match std::process::Command::new("smartctl")
                .args(["-j", "-A", "-i", &dev])
                .output()
            {
                // `parse_smart` classifies an empty reading itself, from
                // smartctl's own messages.
                Ok(o) => out.push(parse_smart(
                    &String::from_utf8_lossy(&o.stdout),
                    &dev,
                    rota,
                    &tran,
                )),
                Err(e) => out.push(DriveWear {
                    dev,
                    kind: drive_kind(rota, &tran),
                    error: Some(if e.kind() == std::io::ErrorKind::NotFound {
                        WearError::NoTool
                    } else {
                        WearError::Other(e.to_string())
                    }),
                    ..Default::default()
                }),
            }
        }
        let _ = tx.send(out);
    });
}

/// Collect the worker's rows once they land; the spinner runs meanwhile because
/// the UI thread never waits.
pub fn tick(app: &mut App) {
    let Some(rx) = &app.tbwtest_rx else {
        return;
    };
    match rx.try_recv() {
        Ok(rows) => {
            app.tbwtest_rx = None;
            app.tbwtest_rows = rows;
            app.tbwtest_running = false;
        }
        Err(crossbeam_channel::TryRecvError::Empty) => {}
        Err(crossbeam_channel::TryRecvError::Disconnected) => {
            app.tbwtest_rx = None;
            app.tbwtest_running = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A SATA SSD: TBW is attribute 241 × the logical block size, wear from
    /// `endurance_used`, hours from `power_on_time`.
    #[test]
    fn a_sata_ssd_reports_tbw_from_lbas_written() {
        let json = r#"{
            "model_name": "Samsung SSD 860",
            "rotation_rate": 0,
            "logical_block_size": 512,
            "power_on_time": {"hours": 17808},
            "endurance_used": {"current_percent": 5},
            "ata_smart_attributes": {"table": [
                {"id": 9, "name": "Power_On_Hours", "raw": {"value": 17808}},
                {"id": 241, "name": "Total_LBAs_Written", "raw": {"value": 81848767104}}
            ]}
        }"#;
        let w = parse_smart(json, "/dev/sda", false, "sata");
        assert_eq!(w.kind, "SSD");
        assert_eq!(w.tbw_bytes, Some(81_848_767_104 * 512));
        assert_eq!(w.wear_percent, Some(5));
        assert_eq!(w.power_on_hours, Some(17808));
        assert_eq!(fmt_tbw(w.tbw_bytes.unwrap()), "41.9 TB");
    }

    /// An NVMe drive: TBW is `data_units_written` × 512000, wear/hours from the
    /// NVMe health log.
    #[test]
    fn an_nvme_drive_reports_tbw_from_data_units_written() {
        let json = r#"{
            "model_name": "WD Black SN850",
            "nvme_smart_health_information_log": {
                "data_units_written": 100000000,
                "percentage_used": 12,
                "power_on_hours": 4200
            }
        }"#;
        let w = parse_smart(json, "/dev/nvme0", false, "nvme");
        assert_eq!(w.kind, "NVMe SSD");
        assert_eq!(w.tbw_bytes, Some(100_000_000 * 512_000));
        assert_eq!(w.wear_percent, Some(12));
        assert_eq!(w.power_on_hours, Some(4200));
        assert_eq!(fmt_tbw(w.tbw_bytes.unwrap()), "51.2 TB");
    }

    /// An HDD that tracks nothing but power-on hours: TBW is n/a (spinning disks
    /// have no endurance budget), but the row still shows its hours.
    #[test]
    fn an_hdd_without_write_tracking_shows_tbw_na() {
        let json = r#"{
            "model_name": "WD Blue HDD",
            "rotation_rate": 7200,
            "power_on_time": {"hours": 30000},
            "ata_smart_attributes": {"table": [
                {"id": 9, "name": "Power_On_Hours", "raw": {"value": 30000}}
            ]}
        }"#;
        let w = parse_smart(json, "/dev/sdb", true, "sata");
        assert_eq!(w.kind, "HDD");
        assert_eq!(w.tbw_bytes, None);
        assert_eq!(w.wear_percent, None);
        assert_eq!(w.power_on_hours, Some(30000));
    }

    /// Garbage or an empty document never panics — the row is simply blank.
    #[test]
    fn unreadable_smart_output_does_not_panic() {
        let w = parse_smart("not json at all", "/dev/sdc", false, "sata");
        assert_eq!(w.kind, "SSD");
        assert_eq!(w.tbw_bytes, None);
    }

    /// An empty reading names its CAUSE. The three JSON documents below are the
    /// real output of smartctl 7.5 — the reasons differ in what the user has to
    /// do, and "no data" alone sent them hunting for a fault that may not exist.
    #[test]
    fn an_empty_reading_reports_why() {
        // Not root.
        let denied = r#"{"smartctl":{"exit_status":2,"messages":[
            {"string":"Smartctl open device: /dev/sda failed: Permission denied","severity":"error"}]}}"#;
        assert_eq!(
            parse_smart(denied, "/dev/sda", false, "sata").error,
            Some(WearError::NeedsRoot)
        );

        // A virtual disk with no SMART at all — what EVERY virtio disk reports,
        // so a QEMU guest on /dev/vdX shows this rather than looking broken.
        let virt = r#"{"smartctl":{"exit_status":1,"messages":[
            {"string":"/dev/vda: Unable to detect device type","severity":"error"}]}}"#;
        assert_eq!(
            parse_smart(virt, "/dev/vda", false, "").error,
            Some(WearError::NoSmart)
        );

        // Anything unforeseen keeps smartctl's own words.
        let odd = r#"{"smartctl":{"exit_status":4,"messages":[
            {"string":"Smartctl open device: /dev/sdz failed: No such device","severity":"error"}]}}"#;
        match parse_smart(odd, "/dev/sdz", false, "sata").error {
            Some(WearError::Other(s)) => {
                assert!(s.contains("No such device"), "lost the reason: {s}")
            }
            other => panic!("an unforeseen failure was flattened into {other:?}"),
        }

        // A good reading carries no error.
        let ok = r#"{"model_name":"X","logical_block_size":512,
            "ata_smart_attributes":{"table":[{"id":241,"raw":{"value":10}}]}}"#;
        assert_eq!(parse_smart(ok, "/dev/sda", false, "sata").error, None);
    }

    /// The table renders in both languages, showing TB for a drive that reports
    /// it and the "n/a" for one that does not — without touching real hardware
    /// (`tbwtest_scanned` set, so `draw` never kicks off a smartctl scan).
    #[test]
    fn the_wear_table_renders() {
        use crate::i18n::Lang;
        for lang in [Lang::Uk, Lang::En, Lang::Es] {
            let mut app = App::new();
            app.lang = lang;
            app.screen = Screen::TbwTest;
            app.tbwtest_scanned = true;
            app.tbwtest_running = false;
            app.tbwtest_rows = vec![
                DriveWear {
                    dev: "/dev/sda".into(),
                    model: "Test SSD".into(),
                    kind: "SSD".into(),
                    tbw_bytes: Some(41_906_000_000_000),
                    wear_percent: Some(5),
                    power_on_hours: Some(17808),
                    error: None,
                },
                DriveWear {
                    dev: "/dev/nvme0".into(),
                    model: "Test NVMe".into(),
                    kind: "NVMe SSD".into(),
                    tbw_bytes: Some(51_200_000_000_000),
                    wear_percent: Some(90),
                    power_on_hours: Some(4200),
                    error: None,
                },
                DriveWear {
                    dev: "/dev/sdb".into(),
                    model: "Test HDD".into(),
                    kind: "HDD".into(),
                    tbw_bytes: None,
                    wear_percent: None,
                    power_on_hours: Some(30000),
                    error: None,
                },
            ];
            let mut term =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
            term.draw(|f| {
                let a = f.area();
                draw(f, &mut app, a);
            })
            .unwrap();
            let out = term.backend().to_string();
            assert!(
                out.contains("41.9 TB"),
                "SSD TBW not shown ({lang:?}):\n{out}"
            );
            assert!(out.contains("NVMe SSD"), "NVMe kind not shown ({lang:?})");
            assert!(
                out.contains(&t(lang, "tbw.na")),
                "the HDD's n/a TBW is not shown ({lang:?}):\n{out}"
            );
            // The scan was NOT triggered by drawing.
            assert!(
                app.tbwtest_rx.is_none(),
                "draw kicked off a real smartctl scan"
            );
        }
    }
}
