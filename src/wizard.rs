use anyhow::{Context, Result};
use colored::Colorize;
use dialoguer::Input;
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::ssh_config;

// ─── SIGWINCH flag ──────────────────────────────────────────

static RESIZED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_winch(_: libc::c_int) {
    RESIZED.store(true, Ordering::SeqCst);
}

/// Get terminal size directly via ioctl on a given fd.
fn get_size(fd: i32) -> (usize, usize) {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(fd, libc::TIOCGWINSZ as libc::c_ulong, &mut ws) == 0
            && ws.ws_row > 0
            && ws.ws_col > 0
        {
            (ws.ws_row as usize, ws.ws_col as usize)
        } else {
            (24, 80)
        }
    }
}

// ─── Raw key reading (bypasses console crate entirely) ──────

#[derive(Debug, PartialEq)]
enum Key {
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Enter,
    Tab,
    BackTab,
    Backspace,
    Escape,
    Char(char),
    Unknown,
}

/// Read a single byte from a non-blocking `fd`, retrying only on EINTR.
/// Returns WouldBlock if no data is available (spurious poll wakeup).
fn read_byte(fd: i32) -> std::io::Result<u8> {
    let mut buf = [0u8; 1];
    loop {
        let ret = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, 1) };
        if ret == 1 {
            return Ok(buf[0]);
        }
        if ret < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue; // retry on signal interrupt only
            }
            return Err(err); // WouldBlock and others propagate up
        }
        return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "EOF"));
    }
}

/// Try to read a byte within `timeout_ms`; returns None on timeout or no data.
/// Uses non-blocking read so a spurious poll(POLLIN) can't block forever.
fn read_byte_timeout(fd: i32, timeout_ms: i32) -> Option<u8> {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let ret = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
    if ret <= 0 {
        return None;
    }
    // Non-blocking read — returns EAGAIN if poll lied about data
    let mut buf = [0u8; 1];
    let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, 1) };
    if n == 1 {
        Some(buf[0])
    } else {
        None
    }
}

/// Read a complete key from the raw tty fd.
fn read_key(fd: i32) -> std::io::Result<Key> {
    let b = read_byte(fd)?;
    Ok(match b {
        b'\r' | b'\n' => Key::Enter,
        b'\t' => Key::Tab,
        0x7f | 0x08 => Key::Backspace,
        0x1b => {
            // Escape or start of escape sequence — peek with short timeout
            match read_byte_timeout(fd, 50) {
                None => Key::Escape,
                Some(b'[') => match read_byte_timeout(fd, 50) {
                    Some(b'A') => Key::ArrowUp,
                    Some(b'B') => Key::ArrowDown,
                    Some(b'C') => Key::ArrowRight,
                    Some(b'D') => Key::ArrowLeft,
                    Some(b'Z') => Key::BackTab,
                    // Consume any remaining bytes of unknown sequences (e.g. \x1b[1;5C)
                    Some(b) if b.is_ascii_digit() => {
                        // CSI sequences like \x1b[3~ — read until final byte
                        let mut last = b;
                        while last < 0x40 || last > 0x7e {
                            match read_byte_timeout(fd, 50) {
                                Some(next) => last = next,
                                None => break,
                            }
                        }
                        Key::Unknown
                    }
                    _ => Key::Unknown,
                },
                Some(b'O') => match read_byte_timeout(fd, 50) {
                    Some(b'A') => Key::ArrowUp,
                    Some(b'B') => Key::ArrowDown,
                    Some(b'C') => Key::ArrowRight,
                    Some(b'D') => Key::ArrowLeft,
                    _ => Key::Unknown,
                },
                Some(_) => Key::Unknown, // Alt+key, ignore
            }
        }
        0x01..=0x1a => Key::Unknown, // other ctrl chars
        b if b >= b' ' && b <= b'~' => Key::Char(b as char),
        _ => Key::Unknown,
    })
}

// ─── Form types ──────────────────────────────────────────────

#[derive(Clone, PartialEq)]
enum OptionKind {
    Choice(String),
    Manual,
    Skip,
}

struct FormOption {
    label: String,
    kind: OptionKind,
}

struct TextField {
    label: String,
    buffer: String,
    digits_only: bool,
}

enum TabContent {
    Selection {
        options: Vec<FormOption>,
        selected: Option<usize>,
        manual_value: Option<String>,
    },
    TextInput {
        fields: Vec<TextField>,
        active_field: usize,
    },
}

struct FormSection {
    label: String,
    required: bool,
    content: TabContent,
}

impl FormSection {
    fn new_selection(label: &str, required: bool) -> Self {
        Self {
            label: label.to_string(),
            required,
            content: TabContent::Selection {
                options: Vec::new(),
                selected: None,
                manual_value: None,
            },
        }
    }

    fn new_text(label: &str, required: bool, fields: Vec<TextField>) -> Self {
        Self {
            label: label.to_string(),
            required,
            content: TabContent::TextInput {
                fields,
                active_field: 0,
            },
        }
    }

    fn choice(mut self, label: &str, value: &str) -> Self {
        if let TabContent::Selection { ref mut options, .. } = self.content {
            options.push(FormOption {
                label: label.to_string(),
                kind: OptionKind::Choice(value.to_string()),
            });
        }
        self
    }

    fn manual(mut self) -> Self {
        if let TabContent::Selection { ref mut options, .. } = self.content {
            options.push(FormOption {
                label: "Enter manually...".to_string(),
                kind: OptionKind::Manual,
            });
        }
        self
    }

    fn skip(mut self) -> Self {
        if let TabContent::Selection { ref mut options, .. } = self.content {
            options.push(FormOption {
                label: "None".to_string(),
                kind: OptionKind::Skip,
            });
        }
        self
    }

    fn with_default(mut self, idx: usize) -> Self {
        if let TabContent::Selection {
            ref mut selected,
            ref options,
            ..
        } = self.content
        {
            if idx < options.len() {
                *selected = Some(idx);
            }
        }
        self
    }

    fn option_count(&self) -> usize {
        match &self.content {
            TabContent::Selection { options, .. } => options.len(),
            TabContent::TextInput { fields, .. } => fields.len(),
        }
    }

    fn value(&self) -> Option<String> {
        match &self.content {
            TabContent::Selection {
                options,
                selected,
                manual_value,
            } => {
                if let Some(mv) = manual_value {
                    return Some(mv.clone());
                }
                selected.and_then(|idx| match &options[idx].kind {
                    OptionKind::Choice(v) => Some(v.clone()),
                    OptionKind::Manual => manual_value.clone(),
                    OptionKind::Skip => None,
                })
            }
            TabContent::TextInput { fields, .. } => {
                if fields.len() == 1 {
                    let val = fields[0].buffer.trim().to_string();
                    if val.is_empty() {
                        None
                    } else {
                        Some(val)
                    }
                } else {
                    let all_filled = fields.iter().all(|f| !f.buffer.trim().is_empty());
                    if all_filled {
                        Some(
                            fields
                                .iter()
                                .map(|f| f.buffer.trim().to_string())
                                .collect::<Vec<_>>()
                                .join(":"),
                        )
                    } else {
                        None
                    }
                }
            }
        }
    }

    fn text_field_value(&self, idx: usize) -> Option<String> {
        if let TabContent::TextInput { fields, .. } = &self.content {
            let val = fields.get(idx)?.buffer.trim().to_string();
            if val.is_empty() {
                None
            } else {
                Some(val)
            }
        } else {
            None
        }
    }
}

// ─── Form state + navigation ─────────────────────────────────

struct FormState {
    sections: Vec<FormSection>,
    tab: usize,
    item: usize,
    on_confirm: bool,
    existing_names: Vec<String>,
    used_ports: Vec<u16>,
    error: Option<String>,
}

impl FormState {
    fn new(
        sections: Vec<FormSection>,
        existing_names: Vec<String>,
        used_ports: Vec<u16>,
    ) -> Self {
        Self {
            sections,
            tab: 0,
            item: 0,
            on_confirm: false,
            existing_names,
            used_ports,
            error: None,
        }
    }

    fn tab_left(&mut self) {
        if self.tab > 0 {
            self.on_confirm = false;
            self.error = None;
            self.tab -= 1;
            self.item = self.restore_cursor();
        }
    }

    fn tab_right(&mut self) {
        if self.tab + 1 < self.sections.len() {
            self.on_confirm = false;
            self.error = None;
            self.tab += 1;
            self.item = self.restore_cursor();
        }
    }

    fn restore_cursor(&self) -> usize {
        match &self.sections[self.tab].content {
            TabContent::Selection { selected, .. } => selected.unwrap_or(0),
            TabContent::TextInput { active_field, .. } => *active_field,
        }
    }

    fn up(&mut self) {
        if self.on_confirm {
            self.on_confirm = false;
            let count = self.sections[self.tab].option_count();
            self.item = count.saturating_sub(1);
            if let TabContent::TextInput {
                ref mut active_field,
                ..
            } = self.sections[self.tab].content
            {
                *active_field = self.item;
            }
        } else if self.item > 0 {
            self.item -= 1;
            if let TabContent::TextInput {
                ref mut active_field,
                ..
            } = self.sections[self.tab].content
            {
                *active_field = self.item;
            }
        }
    }

    fn down(&mut self) {
        if self.on_confirm {
            return;
        }
        let count = self.sections[self.tab].option_count();
        if self.item + 1 < count {
            self.item += 1;
            if let TabContent::TextInput {
                ref mut active_field,
                ..
            } = self.sections[self.tab].content
            {
                *active_field = self.item;
            }
        } else {
            self.on_confirm = true;
        }
    }

    fn advance_tab(&mut self) {
        self.on_confirm = false;
        self.error = None;
        if self.tab + 1 < self.sections.len() {
            self.tab += 1;
            self.item = self.restore_cursor();
        } else {
            self.on_confirm = true;
        }
    }

    fn select_current(&mut self) {
        if let TabContent::Selection {
            ref mut selected,
            ref mut manual_value,
            ref options,
        } = self.sections[self.tab].content
        {
            match &options[self.item].kind {
                OptionKind::Choice(_) => {
                    *selected = Some(self.item);
                    *manual_value = None;
                }
                OptionKind::Skip => {
                    *selected = None;
                    *manual_value = None;
                }
                OptionKind::Manual => {}
            }
        }
    }

    fn is_manual(&self) -> bool {
        if self.on_confirm {
            return false;
        }
        if let TabContent::Selection { ref options, .. } = self.sections[self.tab].content {
            options[self.item].kind == OptionKind::Manual
        } else {
            false
        }
    }

    fn set_manual(&mut self, val: String) {
        if let TabContent::Selection {
            ref mut selected,
            ref mut manual_value,
            ..
        } = self.sections[self.tab].content
        {
            *manual_value = Some(val);
            *selected = Some(self.item);
        }
    }

    fn is_text_input(&self) -> bool {
        matches!(
            self.sections[self.tab].content,
            TabContent::TextInput { .. }
        )
    }

    fn handle_char(&mut self, c: char) {
        if let TabContent::TextInput {
            ref mut fields,
            active_field,
        } = self.sections[self.tab].content
        {
            let field = &mut fields[active_field];
            if field.digits_only {
                if c.is_ascii_digit() {
                    field.buffer.push(c);
                    self.error = None;
                }
            } else {
                field.buffer.push(c);
                self.error = None;
            }
        }
    }

    fn handle_backspace(&mut self) {
        if let TabContent::TextInput {
            ref mut fields,
            active_field,
        } = self.sections[self.tab].content
        {
            fields[active_field].buffer.pop();
            self.error = None;
        }
    }

    fn validate_current_text_tab(&mut self) -> bool {
        let err = if self.tab == 0 {
            self.validate_name()
        } else if self.tab == self.sections.len() - 1 {
            self.validate_ports()
        } else {
            None
        };
        if let Some(e) = err {
            self.error = Some(e);
            false
        } else {
            self.error = None;
            true
        }
    }

    fn validate_name(&self) -> Option<String> {
        if let TabContent::TextInput { ref fields, .. } = self.sections[0].content {
            let val = fields[0].buffer.trim();
            if val.is_empty() {
                return Some("cannot be empty".into());
            }
            if val.contains(char::is_whitespace) {
                return Some("cannot contain spaces".into());
            }
            if val.contains('*') || val.contains('?') {
                return Some("cannot contain wildcards".into());
            }
            if self.existing_names.iter().any(|n| n == val) {
                return Some(format!("'{}' already exists", val));
            }
        }
        None
    }

    fn validate_ports(&self) -> Option<String> {
        let last = self.sections.len() - 1;
        if let TabContent::TextInput { ref fields, .. } = self.sections[last].content {
            for field in fields {
                let val = field.buffer.trim();
                if val.is_empty() {
                    return Some(format!("{} cannot be empty", field.label));
                }
                match val.parse::<u16>() {
                    Ok(0) => return Some("port must be between 1 and 65535".into()),
                    Ok(_) => {}
                    Err(_) => return Some("must be a number between 1 and 65535".into()),
                }
            }
            if let Ok(lp) = fields[0].buffer.trim().parse::<u16>() {
                if self.used_ports.contains(&lp) {
                    return Some(format!(
                        "port {} is already used by another tunnel",
                        lp
                    ));
                }
            }
        }
        None
    }

    fn ready(&self) -> bool {
        self.sections
            .iter()
            .all(|s| !s.required || s.value().is_some())
    }
}

// ─── Rendering ───────────────────────────────────────────────

fn visible_range(total: usize, cursor: usize, max_height: usize) -> (usize, usize) {
    if total <= max_height {
        return (0, total);
    }
    // Reserve 2 lines for scroll indicators
    let window = max_height.saturating_sub(2);
    if window == 0 {
        return (0, 0);
    }
    let half = window / 2;
    let mut start = cursor.saturating_sub(half);
    if start + window > total {
        start = total - window;
    }
    (start, start + window)
}

fn render(state: &FormState, fd: i32) -> Result<()> {
    let (rows, cols) = get_size(fd);

    // Minimum terminal size guard — chrome alone needs ~12 rows
    if rows < 14 || cols < 20 {
        let mut frame = String::from("\x1b[r");
        for row in 1..=rows {
            frame.push_str(&format!("\x1b[{};1H\x1b[2K", row));
        }
        frame.push_str("\x1b[1;1H  ");
        frame.push_str(&"Terminal too small — resize to continue".dimmed().to_string());
        tty_write(fd, &frame);
        return Ok(());
    }

    // Buffer all output lines, then flush with truncation + row limit
    // to guarantee no wrapping and no scrollback overflow.
    let mut out: Vec<String> = Vec::new();

    // ── Title ──
    out.push(format!("  {}", "New Tunnel".bold()));
    out.push(String::new());

    // ── Tab bar ──
    let mut tab_bar = String::from("  ");
    for (si, section) in state.sections.iter().enumerate() {
        if si > 0 {
            tab_bar.push_str("  ");
        }
        let is_active = !state.on_confirm && state.tab == si;
        let has_value = section.value().is_some();

        if is_active {
            tab_bar.push_str(&format!(
                "{}{}{}",
                "[ ".cyan().bold(),
                section.label.cyan().bold(),
                " ]".cyan().bold()
            ));
        } else if has_value {
            tab_bar.push_str(&format!("{} {}", "✓".green(), section.label.green()));
        } else if section.required {
            tab_bar.push_str(&format!("{} {}", "·".yellow(), section.label.yellow()));
        } else {
            tab_bar.push_str(&section.label.dimmed().to_string());
        }
    }
    out.push(tab_bar);

    // ── Separator ──
    let sep_width = cols.saturating_sub(4);
    out.push(format!("  {}", "─".repeat(sep_width).dimmed()));

    // ── Content area ──
    out.push(String::new());

    // Compute available height for content area
    // Fixed overhead: title(1) + blank(1) + tab_bar(1) + separator(1) +
    //   blank_before(1) + blank_after(1) + confirm(1) + blank(1) +
    //   dotted_sep(1) + summary(1) + blank(1) + hints(1) = 12
    let error_lines = if state.error.is_some() { 2 } else { 0 };
    let max_content = rows.saturating_sub(12 + error_lines);

    match &state.sections[state.tab].content {
        TabContent::Selection {
            options,
            selected,
            manual_value,
        } => {
            let total = options.len();
            let cursor = if state.on_confirm {
                total.saturating_sub(1)
            } else {
                state.item
            };
            let (start, end) = visible_range(total, cursor, max_content);

            if start > 0 {
                out.push(format!(
                    "    {}",
                    format!("↑ {} more", start).dimmed()
                ));
            }

            for oi in start..end {
                let opt = &options[oi];
                let at_cursor = !state.on_confirm && state.item == oi;
                let is_selected = *selected == Some(oi);

                let prefix = if is_selected {
                    format!("    {} ", "✓".green())
                } else if at_cursor {
                    format!("    {} ", "›".cyan())
                } else {
                    "      ".to_string()
                };

                let label = if is_selected && opt.kind == OptionKind::Manual {
                    match manual_value {
                        Some(v) => format!("{}: {}", opt.label, v),
                        None => opt.label.clone(),
                    }
                } else {
                    opt.label.clone()
                };

                let styled = if at_cursor && is_selected {
                    label.green().bold().to_string()
                } else if at_cursor {
                    label.bold().to_string()
                } else if is_selected {
                    label.green().to_string()
                } else if matches!(opt.kind, OptionKind::Manual | OptionKind::Skip) {
                    label.dimmed().to_string()
                } else {
                    label
                };

                out.push(format!("{}{}", prefix, styled));
            }

            if end < total {
                out.push(format!(
                    "    {}",
                    format!("↓ {} more", total - end).dimmed()
                ));
            }
        }
        TabContent::TextInput {
            fields,
            active_field,
        } => {
            let max_label = fields.iter().map(|f| f.label.len()).max().unwrap_or(0);
            for (fi, field) in fields.iter().enumerate() {
                let is_active = !state.on_confirm && fi == *active_field;
                let prefix = if is_active {
                    format!("    {} ", "›".cyan())
                } else {
                    "      ".to_string()
                };
                let pad = " ".repeat(max_label.saturating_sub(field.label.len()) + 1);

                if is_active {
                    out.push(format!(
                        "{}{}:{}{}{}",
                        prefix,
                        field.label.cyan().bold(),
                        pad,
                        field.buffer,
                        "_".dimmed()
                    ));
                } else {
                    out.push(format!(
                        "{}{}:{}{}",
                        prefix,
                        field.label.dimmed(),
                        pad,
                        field.buffer.dimmed()
                    ));
                }
            }
        }
    }

    // ── Validation error ──
    if let Some(ref err) = state.error {
        out.push(String::new());
        out.push(format!("    {}", err.red()));
    }

    // ── Confirm button ──
    out.push(String::new());

    if state.on_confirm {
        if state.ready() {
            out.push(format!(
                "    {} {}",
                "›".cyan(),
                "[ Confirm ]".green().bold()
            ));
        } else {
            out.push(format!(
                "    {} {}",
                "›".cyan(),
                "[ Fill required tabs ]".yellow()
            ));
        }
    } else {
        out.push(format!("      {}", "[ Confirm ]".dimmed()));
    }

    // ── Dotted separator + Summary ──
    out.push(String::new());
    let dot_width = cols.saturating_sub(4);
    out.push(format!("  {}", "┄".repeat(dot_width).dimmed()));
    out.push(format!("  {}", build_summary(state)));

    // ── Hints ──
    out.push(String::new());
    out.push(format!(
        "  {}",
        "←→ tab  ↑↓ choose  ⏎ select  esc cancel".dimmed()
    ));

    // ── Flush: explicit cursor positioning per row ──
    // Each row gets \x1b[row;1H (go to row) + \x1b[2K (clear line) + content.
    // This is immune to scroll region corruption and cursor state issues
    // that occur when the terminal is resized in the alt screen.
    let mut frame = String::from("\x1b[r"); // reset scroll region to full screen
    for row in 1..=rows {
        frame.push_str(&format!("\x1b[{};1H\x1b[2K", row));
        if let Some(line) = out.get(row - 1) {
            let truncated = console::truncate_str(line, cols, "");
            frame.push_str(&truncated);
            frame.push_str("\x1b[0m"); // reset attrs so erase doesn't inherit color
        }
    }
    tty_write(fd, &frame);

    Ok(())
}

fn build_summary(state: &FormState) -> String {
    let m = || "???".yellow().to_string();

    let name = state.sections[0].value().unwrap_or_else(&m);
    let group = state.sections[1].value();
    let host = state.sections[2].value().unwrap_or_else(&m);
    let user = state.sections[3].value().unwrap_or_else(&m);
    let identity = state.sections[4].value();
    let proxy_jump = state.sections[5].value();

    let arrow = "→".dimmed().to_string();
    let dot = "·".dimmed().to_string();

    let mut parts = vec![name];
    if let Some(g) = group {
        parts.push(format!("[{}]", g).dimmed().to_string());
    }
    parts.extend([arrow.clone(), host, dot.clone(), user]);
    if let Some(id) = identity {
        parts.push(dot.clone());
        parts.push(id);
    }
    if let Some(pj) = proxy_jump {
        parts.push(dot.clone());
        parts.push(pj);
    }
    parts.push(dot);

    let last = state.sections.len() - 1;
    let section_count = state.sections.len();

    // Sections 0-5 are always the same (Name, Group, Host, User, Identity, ProxyJump).
    // Section 6+ varies by forward type:
    //   Local/Remote: section 6 = Forward/Target (selection), section 7 = Ports (2 fields)
    //   Dynamic: section 6 = Port (1 field)
    if section_count == 7 {
        // Dynamic: only a single port field
        let listen_port = state.sections[last]
            .text_field_value(0)
            .unwrap_or_else(&m);
        parts.push(format!("D:{}", listen_port));
    } else {
        // Local or Remote: Forward/Target + Ports
        let forward = state.sections[6]
            .value()
            .unwrap_or_else(|| "localhost".to_string());
        let port1 = state.sections[last]
            .text_field_value(0)
            .unwrap_or_else(&m);
        let port2 = state.sections[last]
            .text_field_value(1)
            .unwrap_or_else(&m);
        parts.push(format!("{}:{}", forward, port1));
        parts.push(arrow);
        parts.push(port2);
    }

    parts.join(" ")
}

// ─── Form loop ───────────────────────────────────────────────

/// Set the tty file descriptor to raw mode; returns the original termios.
unsafe fn set_raw_mode(fd: i32) -> libc::termios {
    unsafe {
        let mut orig: libc::termios = std::mem::zeroed();
        libc::tcgetattr(fd, &mut orig);
        let mut raw = orig;
        libc::cfmakeraw(&mut raw);
        // Keep output post-processing so \n still maps to \r\n
        raw.c_oflag |= libc::OPOST;
        libc::tcsetattr(fd, libc::TCSANOW, &raw);
        orig
    }
}

/// Restore original termios on a file descriptor.
unsafe fn restore_mode(fd: i32, orig: &libc::termios) {
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, orig) };
}

fn run_form(mut state: FormState) -> Result<Option<FormState>> {
    // Open /dev/tty — single fd for poll, read, write, and ioctl
    let tty = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .context("failed to open /dev/tty")?;
    let tty_fd = tty.as_raw_fd();

    // Set non-blocking so reads never hang on spurious poll(POLLIN)
    unsafe {
        let flags = libc::fcntl(tty_fd, libc::F_GETFL);
        libc::fcntl(tty_fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }

    // Set raw mode so we get individual keypresses
    let orig_termios = unsafe { set_raw_mode(tty_fd) };

    // Install SIGWINCH handler (no SA_RESTART so poll() is interrupted)
    RESIZED.store(false, Ordering::SeqCst);
    let old_sa = unsafe {
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = handle_winch as *const () as usize;
        sa.sa_flags = 0;
        let mut old: libc::sigaction = std::mem::zeroed();
        libc::sigaction(libc::SIGWINCH, &sa, &mut old);
        old
    };

    // Hide cursor + enter alternate screen buffer
    tty_write(tty_fd, "\x1b[?25l\x1b[?1049h");

    let confirmed = (|| -> Result<bool> {
        loop {
            render(&state, tty_fd)?;
            let mut last_render = std::time::Instant::now();

            let key = loop {
                let mut pfd = libc::pollfd {
                    fd: tty_fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                let ret = unsafe { libc::poll(&mut pfd, 1, 100) };

                // Timeout, EINTR, or spurious POLLIN — re-render if throttle allows
                if ret <= 0 {
                    if last_render.elapsed().as_millis() >= 50 {
                        render(&state, tty_fd)?;
                        last_render = std::time::Instant::now();
                    }
                    continue;
                }

                // POLLIN — try to read a key
                match read_key(tty_fd) {
                    Ok(k) => break k,
                    Err(_) => {
                        if last_render.elapsed().as_millis() >= 50 {
                            render(&state, tty_fd)?;
                            last_render = std::time::Instant::now();
                        }
                        continue;
                    }
                }
            };

            // Text input character handling
            if state.is_text_input() && !state.on_confirm {
                match key {
                    Key::Char(c) => {
                        state.handle_char(c);
                        continue;
                    }
                    Key::Backspace => {
                        state.handle_backspace();
                        continue;
                    }
                    _ => {}
                }
            }

            match key {
                Key::ArrowLeft => state.tab_left(),
                Key::ArrowRight => state.tab_right(),
                Key::ArrowUp | Key::BackTab => state.up(),
                Key::ArrowDown => state.down(),
                Key::Tab => {
                    if state.is_text_input() && !state.on_confirm {
                        if state.validate_current_text_tab() {
                            state.advance_tab();
                        }
                    } else {
                        state.down();
                    }
                }
                Key::Enter | Key::Char(' ') => {
                    if state.on_confirm {
                        if state.ready() {
                            return Ok(true);
                        }
                    } else if state.is_text_input() {
                        let count = state.sections[state.tab].option_count();
                        if count > 1 && state.item + 1 < count {
                            state.down();
                        } else if state.validate_current_text_tab() {
                            state.advance_tab();
                        }
                    } else if state.is_manual() {
                        let ti = state.tab;
                        let prompt = format!("  Enter {}", state.sections[ti].label);
                        tty_write(tty_fd, "\x1b[H\x1b[2J\x1b[3J");

                        // Restore cooked mode + show cursor for dialoguer
                        unsafe { restore_mode(tty_fd, &orig_termios) };
                        tty_write(tty_fd, "\x1b[?25h");

                        let val: String =
                            Input::new().with_prompt(&prompt).interact_text()?;

                        // Re-enter raw mode + hide cursor
                        tty_write(tty_fd, "\x1b[?25l");
                        unsafe { set_raw_mode(tty_fd) };

                        let val = val.trim().to_string();
                        if !val.is_empty() {
                            state.set_manual(val);
                        }
                        state.advance_tab();
                    } else {
                        state.select_current();
                        state.advance_tab();
                    }
                }
                Key::Escape => return Ok(false),
                _ => {}
            }
        }
    })();

    // Leave alternate screen + show cursor + restore terminal mode
    tty_write(tty_fd, "\x1b[?1049l\x1b[?25h");
    unsafe {
        restore_mode(tty_fd, &orig_termios);
        libc::sigaction(libc::SIGWINCH, &old_sa, std::ptr::null_mut());
    }

    match confirmed? {
        true => Ok(Some(state)),
        false => Ok(None),
    }
}

// ─── SSH config gathering ────────────────────────────────────

struct HostEntry {
    alias: String,
    hostname: String,
}

struct SshChoices {
    hosts: Vec<HostEntry>,
    users: Vec<String>,
    identity_files: Vec<String>,
    proxy_jumps: Vec<String>,
    remote_hosts: Vec<String>,
}

impl SshChoices {
    fn empty() -> Self {
        Self {
            hosts: vec![],
            users: vec![],
            identity_files: vec![],
            proxy_jumps: vec![],
            remote_hosts: vec![],
        }
    }
}

fn gather_choices() -> SshChoices {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return SshChoices::empty(),
    };
    let ssh_dir = home.join(".ssh");
    let config_path = ssh_dir.join("config");

    let mut hosts = Vec::new();
    let mut users = BTreeSet::new();
    let mut identity_files = BTreeSet::new();
    let mut proxy_jumps = BTreeSet::new();
    let mut host_aliases = BTreeSet::new();
    let mut remote_hosts = BTreeSet::new();

    if let Ok(content) = fs::read_to_string(&config_path) {
        let mut cur_alias: Option<String> = None;
        let mut cur_hostname: Option<String> = None;

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = match parse_kv(line) {
                Some(p) => p,
                None => continue,
            };
            match key.to_lowercase().as_str() {
                "host" => {
                    if let Some(alias) = cur_alias.take() {
                        if let Some(hn) = cur_hostname.take() {
                            hosts.push(HostEntry {
                                alias: alias.clone(),
                                hostname: hn,
                            });
                        }
                        host_aliases.insert(alias);
                    }
                    let name = value.split_whitespace().next().unwrap_or("");
                    if !name.contains('*') && !name.contains('?') && !name.is_empty() {
                        cur_alias = Some(name.to_string());
                    }
                }
                "hostname" => {
                    if cur_alias.is_some() {
                        cur_hostname = Some(value.to_string());
                    }
                }
                "user" => {
                    users.insert(value.to_string());
                }
                "identityfile" => {
                    identity_files.insert(value.to_string());
                }
                "proxyjump" => {
                    proxy_jumps.insert(value.to_string());
                }
                "localforward" => {
                    let parts: Vec<&str> = value.split_whitespace().collect();
                    if parts.len() == 2 {
                        if let Some(c) = parts[1].rfind(':') {
                            let rh = &parts[1][..c];
                            if rh != "localhost" {
                                remote_hosts.insert(rh.to_string());
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(alias) = cur_alias {
            if let Some(hn) = cur_hostname {
                hosts.push(HostEntry {
                    alias: alias.clone(),
                    hostname: hn,
                });
            }
            host_aliases.insert(alias);
        }
    }

    if let Ok(entries) = fs::read_dir(&ssh_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.ends_with(".pub") {
                        let priv_name = &name[..name.len() - 4];
                        if ssh_dir.join(priv_name).is_file() {
                            identity_files.insert(format!("~/.ssh/{}", priv_name));
                        }
                    }
                }
            }
        }
    }



    SshChoices {
        hosts,
        users: users.into_iter().collect(),
        identity_files: identity_files.into_iter().collect(),
        proxy_jumps: proxy_jumps.into_iter().collect(),
        remote_hosts: remote_hosts.into_iter().collect(),
    }
}

/// Write all bytes to the given fd (retries on partial writes, EINTR, and WouldBlock).
fn tty_write(fd: i32, data: &str) {
    let bytes = data.as_bytes();
    let mut offset = 0;
    while offset < bytes.len() {
        let ret = unsafe {
            libc::write(
                fd,
                bytes[offset..].as_ptr() as *const libc::c_void,
                bytes[offset..].len(),
            )
        };
        if ret > 0 {
            offset += ret as usize;
        } else if ret < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            if err.kind() == std::io::ErrorKind::WouldBlock {
                // Non-blocking fd — wait for writable then retry
                let mut pfd = libc::pollfd {
                    fd,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                unsafe { libc::poll(&mut pfd, 1, 100) };
                continue;
            }
            break; // give up on other errors
        } else {
            break;
        }
    }
}

fn parse_kv(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if let Some(eq) = line.find('=') {
        let k = line[..eq].trim();
        let v = line[eq + 1..].trim();
        if !k.is_empty() && !v.is_empty() {
            return Some((k, v));
        }
    }
    let mut parts = line.splitn(2, char::is_whitespace);
    let k = parts.next()?;
    let v = parts.next()?.trim();
    if v.is_empty() {
        return None;
    }
    Some((k, v))
}

// ─── Main wizard ─────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum ForwardType {
    Local,
    Remote,
    Dynamic,
}

pub fn cmd_add() -> Result<()> {
    let tunnels = ssh_config::discover_tunnels().unwrap_or_default();
    let choices = gather_choices();

    let existing_names: Vec<String> = tunnels.iter().map(|t| t.name.clone()).collect();
    let used_ports: Vec<u16> = tunnels
        .iter()
        .flat_map(|t| {
            t.forwards
                .iter()
                .map(|f| f.local_port)
                .chain(t.dynamic_forwards.iter().map(|f| f.listen_port))
        })
        .collect();

    // ── Ask forward type ──
    let fwd_type = {
        let items = &["Local Forward", "Remote Forward", "Dynamic (SOCKS)"];
        let selection = dialoguer::Select::new()
            .with_prompt("Forward type")
            .items(items)
            .default(0)
            .interact()
            .context("failed to read selection")?;
        match selection {
            0 => ForwardType::Local,
            1 => ForwardType::Remote,
            2 => ForwardType::Dynamic,
            _ => ForwardType::Local,
        }
    };

    let mut sections = Vec::new();

    // ── Name tab (TextInput) ──
    sections.push(FormSection::new_text(
        "Name",
        true,
        vec![TextField {
            label: "Tunnel name".to_string(),
            buffer: String::new(),
            digits_only: false,
        }],
    ));

    // ── Group tab (TextInput, optional) ──
    sections.push(FormSection::new_text(
        "Group",
        false,
        vec![TextField {
            label: "Group tag".to_string(),
            buffer: String::new(),
            digits_only: false,
        }],
    ));

    // ── Host tab (exclude existing tunnels) ──
    let mut host_sec = FormSection::new_selection("Host", true);
    for h in &choices.hosts {
        if existing_names.contains(&h.alias) {
            continue;
        }
        if choices.proxy_jumps.contains(&h.alias) {
            continue;
        }
        host_sec = host_sec.choice(
            &format!("{} ({})", h.alias, h.hostname),
            &h.hostname,
        );
    }
    host_sec = host_sec.manual();
    sections.push(host_sec);

    // ── User tab ──
    let default_user = whoami::username();
    let mut user_sec = FormSection::new_selection("User", true);
    let mut has_current = false;
    for u in &choices.users {
        if *u == default_user {
            has_current = true;
        }
        user_sec = user_sec.choice(u, u);
    }
    if !has_current {
        if let TabContent::Selection {
            ref mut options, ..
        } = user_sec.content
        {
            options.insert(
                0,
                FormOption {
                    label: default_user.clone(),
                    kind: OptionKind::Choice(default_user),
                },
            );
        }
    }
    user_sec = user_sec.manual();
    sections.push(user_sec);

    // ── Identity tab ──
    let mut id_sec = FormSection::new_selection("Identity", false);
    for f in &choices.identity_files {
        id_sec = id_sec.choice(f, f);
    }
    id_sec = id_sec.manual().skip();
    sections.push(id_sec);

    // ── ProxyJump tab (all non-tunnel hosts — any host can be a jump target) ──
    let mut pj_sec = FormSection::new_selection("ProxyJump", false);
    for h in &choices.hosts {
        if existing_names.contains(&h.alias) {
            continue;
        }
        pj_sec = pj_sec.choice(&format!("{} ({})", h.alias, h.hostname), &h.alias);
    }
    pj_sec = pj_sec.manual().skip();
    sections.push(pj_sec);

    // ── Forward-type-specific tabs ──
    match fwd_type {
        ForwardType::Local => {
            let mut fwd_sec = FormSection::new_selection("Forward", true);
            fwd_sec = fwd_sec.choice("localhost", "localhost");
            for rh in &choices.remote_hosts {
                fwd_sec = fwd_sec.choice(rh, rh);
            }
            fwd_sec = fwd_sec.manual().with_default(0);
            sections.push(fwd_sec);

            sections.push(FormSection::new_text(
                "Ports",
                true,
                vec![
                    TextField {
                        label: "Local port".to_string(),
                        buffer: String::new(),
                        digits_only: true,
                    },
                    TextField {
                        label: "Remote port".to_string(),
                        buffer: String::new(),
                        digits_only: true,
                    },
                ],
            ));
        }
        ForwardType::Remote => {
            let mut fwd_sec = FormSection::new_selection("Target", true);
            fwd_sec = fwd_sec.choice("localhost", "localhost");
            for rh in &choices.remote_hosts {
                fwd_sec = fwd_sec.choice(rh, rh);
            }
            fwd_sec = fwd_sec.manual().with_default(0);
            sections.push(fwd_sec);

            sections.push(FormSection::new_text(
                "Ports",
                true,
                vec![
                    TextField {
                        label: "Remote bind port".to_string(),
                        buffer: String::new(),
                        digits_only: true,
                    },
                    TextField {
                        label: "Local target port".to_string(),
                        buffer: String::new(),
                        digits_only: true,
                    },
                ],
            ));
        }
        ForwardType::Dynamic => {
            sections.push(FormSection::new_text(
                "Port",
                true,
                vec![TextField {
                    label: "Listen port".to_string(),
                    buffer: String::new(),
                    digits_only: true,
                }],
            ));
        }
    }

    // ── Run the form ──
    let state = FormState::new(sections, existing_names, used_ports);
    let state = match run_form(state)? {
        Some(s) => s,
        None => {
            println!("  Aborted.");
            return Ok(());
        }
    };

    // ── Extract values ──
    let name = state.sections[0].value().context("name is required")?;
    let group = state.sections[1].value();
    let hostname = state.sections[2].value().context("hostname is required")?;
    let user = state.sections[3].value().context("user is required")?;
    let identity_file = state.sections[4].value();
    let proxy_jump = state.sections[5].value();

    // ── Build config block ──
    let mut block = format!(
        "\n\n# Tunnel: {name}\nHost {name}\n"
    );
    if let Some(ref g) = group {
        block.push_str(&format!("  # mole:group={g}\n"));
    }
    block.push_str(&format!("  HostName {hostname}\n  User {user}\n"));
    if let Some(ref id) = identity_file {
        block.push_str(&format!("  IdentityFile {id}\n"));
    }
    if let Some(ref pj) = proxy_jump {
        block.push_str(&format!("  ProxyJump {pj}\n"));
    }

    let last = state.sections.len() - 1;
    match fwd_type {
        ForwardType::Local => {
            let remote_host = state.sections[6]
                .value()
                .unwrap_or_else(|| "localhost".to_string());
            let local_port: u16 = state.sections[last]
                .text_field_value(0)
                .context("local port is required")?
                .parse()
                .context("invalid local port")?;
            let remote_port: u16 = state.sections[last]
                .text_field_value(1)
                .context("remote port is required")?
                .parse()
                .context("invalid remote port")?;
            block.push_str(&format!(
                "  LocalForward {local_port} {remote_host}:{remote_port}\n"
            ));
        }
        ForwardType::Remote => {
            let target_host = state.sections[6]
                .value()
                .unwrap_or_else(|| "localhost".to_string());
            let bind_port: u16 = state.sections[last]
                .text_field_value(0)
                .context("remote bind port is required")?
                .parse()
                .context("invalid remote bind port")?;
            let target_port: u16 = state.sections[last]
                .text_field_value(1)
                .context("local target port is required")?
                .parse()
                .context("invalid local target port")?;
            block.push_str(&format!(
                "  RemoteForward {bind_port} {target_host}:{target_port}\n"
            ));
        }
        ForwardType::Dynamic => {
            let listen_port: u16 = state.sections[last]
                .text_field_value(0)
                .context("listen port is required")?
                .parse()
                .context("invalid listen port")?;
            block.push_str(&format!("  DynamicForward {listen_port}\n"));
        }
    }
    block.push_str("  RequestTTY no\n  ExitOnForwardFailure yes\n");

    // ── Preview + Write ──
    println!("\n  Will add to ~/.ssh/config:\n");
    for line in block.lines() {
        println!("  {line}");
    }
    println!();

    let config_path = dirs::home_dir()
        .context("cannot determine home directory")?
        .join(".ssh")
        .join("config");

    let mut file = OpenOptions::new()
        .append(true)
        .open(&config_path)
        .with_context(|| format!("failed to open {}", config_path.display()))?;

    file.write_all(block.as_bytes())
        .with_context(|| format!("failed to write to {}", config_path.display()))?;

    println!(
        "  {} Tunnel '{}' added to ~/.ssh/config",
        "✓".green(),
        name
    );

    Ok(())
}
