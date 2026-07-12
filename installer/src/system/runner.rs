//! Command runner. Every privileged action (partitioning, mkfs, pacstrap,
//! chroot config) goes through here so we get three things for free:
//!   1. live stdout+stderr streamed line-by-line into the TUI log,
//!   2. a single place to enforce error handling / abort-on-failure,
//!   3. testability — the rest of the code builds `Cmd`s, doesn't shell out ad hoc.
//!
//! The design choice (per project decision): we do NOT reimplement parted/mkfs
//! in Rust. We orchestrate the battle-tested Artix/Arch utilities and capture
//! their output. Rust owns state and error flow; the utilities do the work.

use crossbeam_channel::{unbounded, Receiver, Sender};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::thread;

/// A line emitted by a running command, tagged by stream.
#[derive(Debug, Clone)]
pub enum LogLine {
    Out(String),
    Err(String),
    /// pacman (running under a PTY in interactive mode) is waiting for input —
    /// e.g. a provider selection ("Enter a number"). The string is the prompt
    /// text so the TUI can show it. The user's reply is written back via the
    /// PtyWriter handle held by the install screen.
    Prompt(String),
    /// Process finished. `Ok(())` on exit code 0, else the failing command.
    Done(Result<(), String>),
}

/// A handle the TUI keeps for an interactive (PTY) step, so it can send the
/// user's typed answer back to pacman. Cloneable and thread-safe.
#[derive(Clone)]
pub struct PtyWriter {
    inner: std::sync::Arc<std::sync::Mutex<Box<dyn std::io::Write + Send>>>,
}

impl PtyWriter {
    /// Send a line (the typed answer) to the child, appending a newline.
    pub fn send_line(&self, text: &str) {
        if let Ok(mut w) = self.inner.lock() {
            let _ = w.write_all(text.as_bytes());
            let _ = w.write_all(b"\n");
            let _ = w.flush();
        }
    }
}

/// Spawn a planned step. When `interactive` is true the child inherits the real
/// terminal so the user can answer prompts directly (interactive-mode
/// basestrap). Otherwise output is streamed into the TUI log with stdin
/// detached (the default).
pub fn spawn_with_mode(program: &str, args: &[&str], interactive: bool) -> Receiver<LogLine> {
    // Non-interactive path only. Interactive steps go through spawn_pty (which
    // also returns a writer handle), called directly by the install screen.
    let _ = interactive;
    spawn_streamed(program, args)
}

/// Spawn a command under a pseudo-terminal so it behaves as if attached to a
/// real terminal (pacman then shows provider prompts and waits for input). The
/// output is streamed into the channel as Out lines; when the child appears to
/// be waiting for input we emit a Prompt line. The returned PtyWriter lets the
/// TUI send the user's typed answer back. Used for interactive install mode.
pub fn spawn_pty(program: &str, args: &[&str]) -> (Receiver<LogLine>, PtyWriter) {
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};
    let (tx, rx): (Sender<LogLine>, Receiver<LogLine>) = unbounded();

    let pty_system = native_pty_system();
    let pair = match pty_system.openpty(PtySize {
        rows: 40,
        cols: 120,
        pixel_width: 0,
        pixel_height: 0,
    }) {
        Ok(p) => p,
        Err(e) => {
            let _ = tx.send(LogLine::Done(Err(format!("openpty: {e}"))));
            // Return a no-op writer.
            let sink: Box<dyn std::io::Write + Send> = Box::new(std::io::sink());
            return (
                rx,
                PtyWriter {
                    inner: std::sync::Arc::new(std::sync::Mutex::new(sink)),
                },
            );
        }
    };

    let mut cmd = CommandBuilder::new(program);
    cmd.args(args);
    // portable-pty's CommandBuilder starts with an EMPTY environment, so the
    // child can't even find `sh` (no PATH). Inherit the installer's environment
    // explicitly. We also set a sane TERM and a guaranteed PATH fallback.
    for (k, v) in std::env::vars() {
        cmd.env(k, v);
    }
    if std::env::var_os("PATH").is_none() {
        cmd.env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/bin:/usr/sbin:/sbin:/bin",
        );
    }
    cmd.env("TERM", "xterm-256color");
    // Make pacman's prompts predictable / unlocalised so our heuristics match.
    cmd.env("LC_ALL", "C");

    let mut child = match pair.slave.spawn_command(cmd) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(LogLine::Done(Err(format!("spawn: {e}"))));
            let sink: Box<dyn std::io::Write + Send> = Box::new(std::io::sink());
            return (
                rx,
                PtyWriter {
                    inner: std::sync::Arc::new(std::sync::Mutex::new(sink)),
                },
            );
        }
    };
    // Drop the slave handle in the parent so EOF propagates when the child exits.
    drop(pair.slave);

    let writer = match pair.master.take_writer() {
        Ok(w) => w,
        Err(e) => {
            let _ = tx.send(LogLine::Done(Err(format!("pty writer: {e}"))));
            let sink: Box<dyn std::io::Write + Send> = Box::new(std::io::sink());
            return (
                rx,
                PtyWriter {
                    inner: std::sync::Arc::new(std::sync::Mutex::new(sink)),
                },
            );
        }
    };
    let pty_writer = PtyWriter {
        inner: std::sync::Arc::new(std::sync::Mutex::new(writer)),
    };

    let mut reader = match pair.master.try_clone_reader() {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(LogLine::Done(Err(format!("pty reader: {e}"))));
            return (rx, pty_writer);
        }
    };

    // Keep the master alive for the duration by moving it into the thread.
    let master = pair.master;
    let writer_for_auto = pty_writer.clone();
    let prog_name = program.to_string();

    // Shared "stalled tail" state for the watchdog: the reader updates the
    // current unterminated tail + timestamp; the watchdog emits a Prompt if the
    // tail sits unchanged long enough (the child is blocked waiting for input).
    // This is language-independent: it doesn't matter what the prompt SAYS —
    // if output stalled on a non-empty line, someone is waiting for an answer.
    use std::sync::{Arc, Mutex};
    use std::time::Instant;
    struct TailState {
        text: String,
        since: Instant,
        prompted: bool,
        block: Vec<String>,
        in_block: bool,
    }
    let tail_state = Arc::new(Mutex::new(TailState {
        text: String::new(),
        since: Instant::now(),
        prompted: false,
        block: Vec::new(),
        in_block: false,
    }));

    // Watchdog thread: if the tail is non-empty, unchanged for >900ms, and not
    // yet handled, decide: Y/n → auto-yes; same-package block → auto "1";
    // otherwise → Prompt for the user.
    {
        let tail_state = Arc::clone(&tail_state);
        let tx = tx.clone();
        let writer = pty_writer.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(120));
            let mut st = match tail_state.lock() {
                Ok(g) => g,
                Err(_) => break,
            };
            if st.prompted {
                continue;
            }
            // Two waiting shapes:
            //  (a) non-empty tail sitting unchanged (classic "prompt: _")
            //  (b) EMPTY tail right after a numbered options menu — some tools
            //      (paru) print the question + newline and read on a fresh
            //      line, so the tail is empty but the child is still waiting.
            let empty_tail = st.text.trim().is_empty();
            if empty_tail && !st.in_block {
                continue;
            }
            // Latency matters here: every provider question costs at least
            // one full stall window, and a typical install asks several, so
            // the windows add up to user-visible sluggishness. Tails that
            // UNAMBIGUOUSLY look like prompts ("Enter a number", "[Y/n]",
            // "(default=N)", or ending in ':') get a short 250ms window;
            // only shapeless stalls keep the conservative 900ms fallback
            // (which exists for localized/unknown tools).
            let tail_clean = strip_ansi(&st.text);
            let lowt = tail_clean.to_lowercase();
            let looks_like_prompt = lowt.contains("enter a number")
                || lowt.contains("[y/n]")
                || lowt.contains("default=")
                || tail_clean.trim_end().ends_with(':');
            let window = if looks_like_prompt { 250 } else { 900 };
            if st.since.elapsed() < std::time::Duration::from_millis(window) {
                continue;
            }
            // The child has been silent on a non-empty tail: it's waiting.
            if lowt.contains("[y/n]") || lowt.contains("proceed") {
                let _ = tx.send(LogLine::Out("(auto) Proceed → Y".to_string()));
                writer.send_line("y");
                st.prompted = true;
                continue;
            }
            if st.in_block && provider_block_same_package(&st.block) {
                let _ = tx.send(LogLine::Out(
                    "(auto) same package across repos → Artix (1)".to_string(),
                ));
                writer.send_line("1");
            } else {
                // With an empty tail (question printed + newline, e.g. paru),
                // fall back to the last menu line so the UI prompt has context.
                let prompt_text = if tail_clean.trim().is_empty() {
                    st.block.last().cloned().unwrap_or_else(|| "?".to_string())
                } else {
                    tail_clean
                };
                let _ = tx.send(LogLine::Prompt(prompt_text));
            }
            st.prompted = true;
            st.in_block = false;
            // Drop the consumed menu so stale rows can never leak into the
            // next block even if block-start detection ever misfires.
            st.block.clear();
        });
    }

    let tail_for_reader = Arc::clone(&tail_state);
    std::thread::spawn(move || {
        use std::io::Read;
        let _keep_master = master; // hold until child exits
        let mut buf = [0u8; 4096];
        let mut line = String::new();
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break, // EOF: child exited
                Ok(n) => {
                    let chunk = String::from_utf8_lossy(&buf[..n]);
                    for ch in chunk.chars() {
                        if ch == '\n' || ch == '\r' {
                            if !line.trim().is_empty() {
                                let clean = strip_ansi(&line);
                                // Structural provider-block tracking, language-
                                // independent: a line that CONTAINS numbered
                                // options ("1) name") extends/starts a block; a
                                // header line ending in ':' right before options
                                // also counts (pacman's ":: Repository X:" /
                                // paru's localised ":: <repo> AUR:"). Any other
                                // line ends the block.
                                if let Ok(mut st) = tail_for_reader.lock() {
                                    if line_has_numbered_options(&clean) {
                                        if !st.in_block {
                                            st.in_block = true;
                                            st.block.clear();
                                        }
                                        st.block.push(clean.clone());
                                    } else if st.in_block
                                        && (clean.trim_end().ends_with(':')
                                            || clean.trim_start().starts_with("::"))
                                    {
                                        // Lines INSIDE the menu that aren't
                                        // numbered options: repo-group headers.
                                        // pacman prints them as ":: Repository
                                        // world" (NO trailing colon!), paru as
                                        // ":: <repo> AUR:". Either way a "::"
                                        // line between option rows must NOT end
                                        // the block, or a multi-repo menu gets
                                        // split and same-package detection
                                        // breaks (e.g. world/extra both shipping
                                        // the same name).
                                        st.block.push(clean.clone());
                                    } else if st.in_block {
                                        st.in_block = false;
                                    }
                                }
                                emit_pty_line(&tx, &writer_for_auto, &line);
                            }
                            line.clear();
                            // Newline arrived → the previous tail is consumed.
                            if let Ok(mut st) = tail_for_reader.lock() {
                                st.text.clear();
                                st.since = Instant::now();
                                st.prompted = false;
                            }
                        } else {
                            line.push(ch);
                        }
                    }
                    // Update the shared tail (the unterminated line, if any).
                    if let Ok(mut st) = tail_for_reader.lock() {
                        if st.text != line {
                            st.text = line.clone();
                            st.since = Instant::now();
                            st.prompted = false;
                        }
                    }
                }
                Err(_) => break,
            }
        }
        let result = match child.wait() {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(format!(
                "{} exited with code {}",
                prog_name,
                status.exit_code()
            )),
            Err(e) => Err(format!("wait: {e}")),
        };
        let _ = tx.send(LogLine::Done(result));
    });

    (rx, pty_writer)
}

/// Emit a finished line: filter noise, auto-answer Proceed prompts.
fn emit_pty_line(tx: &Sender<LogLine>, writer: &PtyWriter, raw: &str) {
    let clean = strip_ansi(raw);
    let t = clean.trim();
    if t.is_empty() {
        return;
    }
    let _ = tx.send(LogLine::Out(clean));
    let _ = writer; // proceed prompts are handled on the no-newline tail
}

/// Detect whether the current (newline-less) tail looks like a prompt awaiting
/// input. pacman prompts end with patterns like ": ", "]:", "(default=N):",
/// "[Y/n]" etc.
/// Given the lines of a pacman provider block (everything printed after
/// "There are N providers available for X:" up to the prompt), decide whether
/// every numbered option refers to the SAME package name. pacman prints options
/// like "   1) foo" or "1) foo  2) bar" (sometimes grouped under ":: Repository
/// <name>" headers). We extract the token right after each "N)" and compare. If
/// they're all identical, the choice is purely which repository to take it from
/// — and since Artix repos sit above Arch in pacman.conf, option 1 is Artix, so
/// we can safely auto-pick it. If the names differ, it's a real provider choice
/// (e.g. vulkan-driver) and must go to the user.
fn provider_block_same_package(block: &[String]) -> bool {
    // Collect (option_number, package_name) pairs from the menu lines.
    let mut entries: Vec<(u32, String)> = Vec::new();
    for raw in block {
        let line = strip_ansi(raw);
        // Echoed prompts ("Enter a number (default=1): 1") are not menu rows;
        // their "(default=1)" would otherwise parse as a bogus option 1.
        if line.to_lowercase().contains("default=") {
            continue;
        }
        let bytes: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < bytes.len() {
            // Same word-boundary rule as line_has_numbered_options: the option
            // number must start the line or follow whitespace.
            if bytes[i].is_ascii_digit() && (i == 0 || bytes[i - 1].is_whitespace()) {
                let mut j = i;
                let mut numv: u32 = 0;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    numv = numv.saturating_mul(10) + bytes[j].to_digit(10).unwrap_or(0);
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == ')' {
                    let mut k = j + 1;
                    while k < bytes.len() && bytes[k] == ' ' {
                        k += 1;
                    }
                    let mut name = String::new();
                    while k < bytes.len() && !bytes[k].is_whitespace() {
                        name.push(bytes[k]);
                        k += 1;
                    }
                    // Strip a "repo/" prefix if present, so "system/foo" and
                    // "extra/foo" compare as the same package "foo".
                    if let Some(pos) = name.rfind('/') {
                        name = name[pos + 1..].to_string();
                    }
                    if !name.is_empty() {
                        entries.push((numv, name));
                    }
                    i = k;
                    continue;
                }
            }
            i += 1;
        }
    }
    // STRICT completeness check: we must have captured the WHOLE menu — the
    // option numbers must be exactly 1..=K with no gaps. A partially captured
    // menu (watchdog racing the output, or a header splitting the block) must
    // never trigger the auto-pick: a wrong silent "1" is far worse than asking.
    if entries.len() < 2 {
        return false;
    }
    entries.sort_by_key(|(n, _)| *n);
    let complete = entries
        .iter()
        .enumerate()
        .all(|(idx, (n, _))| *n == (idx as u32) + 1);
    if !complete {
        return false;
    }
    // All names identical → same package across repos.
    entries.windows(2).all(|w| w[0].1 == w[1].1)
}

/// True if a line contains numbered menu options like "1) name 2) name" — the
/// structural signature of a pacman/paru selection menu, independent of locale.
/// We require at least one "<digits>)" followed by a non-space token, AND the
/// digits must sit at the start of the line or right after whitespace.
///
/// The boundary requirement is what keeps the ECHOED PROMPT out: after an
/// answer is sent, the PTY echoes a line like "Enter a number (default=1): 1" —
/// and "(default=1)" contains "1)" too. Without the boundary check that echo
/// line starts a bogus block, the next real menu gets appended to it, the
/// duplicate option numbers fail the strict completeness check, and every
/// same-package menu after the first one wrongly falls through to the user.
/// (We also skip "default=" lines entirely, belt and suspenders — the child
/// runs with LC_ALL=C, so the wording is guaranteed.)
fn line_has_numbered_options(line: &str) -> bool {
    if line.to_lowercase().contains("default=") {
        return false;
    }
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() && (i == 0 || chars[i - 1].is_whitespace()) {
            let mut j = i;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            if j < chars.len() && chars[j] == ')' {
                let mut k = j + 1;
                while k < chars.len() && chars[k] == ' ' {
                    k += 1;
                }
                if k < chars.len() && !chars[k].is_whitespace() {
                    return true;
                }
            }
            i = j;
        }
        i += 1;
    }
    false
}

/// Strip ANSI escape sequences (CSI ... letter) so the TUI log stays clean.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // Skip until the final byte of the escape sequence.
            if chars.peek() == Some(&'[') {
                chars.next();
                while let Some(&nc) = chars.peek() {
                    chars.next();
                    if nc.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else if c != '\u{0}' {
            out.push(c);
        }
    }
    out
}

fn spawn_streamed(program: &str, args: &[&str]) -> Receiver<LogLine> {
    let (tx, rx): (Sender<LogLine>, Receiver<LogLine>) = unbounded();
    let program = program.to_string();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();

    thread::spawn(move || {
        let pretty = format!("$ {} {}", program, args.join(" "));
        let _ = tx.send(LogLine::Out(pretty));

        let child = Command::new(&program)
            .args(&args)
            // Detach stdin (no TTY): pacman's group selection prompt
            // ("Enter a selection (default=all):") and any other prompt then
            // auto-takes its default instead of blocking forever waiting for
            // input that the TUI can't provide.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();

        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(LogLine::Done(Err(format!(
                    "failed to start {program}: {e}"
                ))));
                return;
            }
        };

        // Drain stdout and stderr on separate threads to avoid pipe deadlock.
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let tx_out = tx.clone();
        let tx_err = tx.clone();

        let h_out = stdout.map(|s| {
            thread::spawn(move || {
                for line in BufReader::new(s).lines().map_while(Result::ok) {
                    let _ = tx_out.send(LogLine::Out(line));
                }
            })
        });
        let h_err = stderr.map(|s| {
            thread::spawn(move || {
                for line in BufReader::new(s).lines().map_while(Result::ok) {
                    let _ = tx_err.send(LogLine::Err(line));
                }
            })
        });

        if let Some(h) = h_out {
            let _ = h.join();
        }
        if let Some(h) = h_err {
            let _ = h.join();
        }

        let status = child.wait();
        let result = match status {
            Ok(s) if s.success() => Ok(()),
            Ok(s) => Err(format!(
                "{} exited with {}",
                program,
                s.code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".into())
            )),
            Err(e) => Err(format!("{program}: {e}")),
        };
        let _ = tx.send(LogLine::Done(result));
    });

    rx
}

/// Run a command to completion, collecting stdout. For read-only queries where
/// we need the output as a value (lsblk --json, pacman -Sl, nmcli -t).
pub fn capture(program: &str, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{program} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
