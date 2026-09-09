mod enigma;

use std::fs;
use std::io::Write;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use enigma::{Machine, Rotor, Trace, REFLECTORS, ROTORS};
use ratatui::{
    crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

const LAMPS: [&str; 3] = ["QWERTZUIO", "ASDFGHJK", "PYXCVBNML"];
const DIM: Color = Color::Rgb(90, 92, 100);
const FRAME: Color = Color::Rgb(70, 72, 80);
const WIRE: Color = Color::Rgb(120, 170, 255);
const HOT: Color = Color::Rgb(255, 196, 60);
const LIT: Color = Color::Rgb(120, 255, 140);
const INK: Color = Color::Rgb(180, 184, 200);

#[derive(Clone)]
struct Config {
    reflector: usize,   // index into REFLECTORS
    rotors: [usize; 3], // indices into ROTORS, [left, middle, right]
    rings: [u8; 3],     // 0-25
    positions: [u8; 3], // 0-25 start positions
    plugs: Vec<(u8, u8)>,
}

impl Config {
    fn default() -> Self {
        Config {
            reflector: 0,
            rotors: [0, 1, 2], // I, II, III
            rings: [0, 0, 0],
            positions: [0, 0, 0],
            plugs: vec![(0, 12), (5, 19)], // A-M, F-T as a starter
        }
    }

    fn build(&self) -> Machine {
        let rotors = [
            Rotor::new(ROTORS[self.rotors[0]], self.positions[0], self.rings[0]),
            Rotor::new(ROTORS[self.rotors[1]], self.positions[1], self.rings[1]),
            Rotor::new(ROTORS[self.rotors[2]], self.positions[2], self.rings[2]),
        ];
        let mut m = Machine::new(rotors, REFLECTORS[self.reflector].1);
        m.set_plugs(&self.plugs);
        m
    }

    fn toggle_plug(&mut self, a: u8, b: u8) {
        let pair = (a.min(b), a.max(b));
        if let Some(i) = self.plugs.iter().position(|&p| p == pair) {
            self.plugs.remove(i);
            return;
        }
        // remove any existing pairs that use either letter, then add
        self.plugs.retain(|&(x, y)| x != a && y != a && x != b && y != b);
        if self.plugs.len() < 10 {
            self.plugs.push(pair);
        }
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Screen {
    Run,
    Setup,
    Help,
}

struct Animation {
    trace: Trace,
    hop: usize,
    hop_timer: Instant,
}

struct App {
    config: Config,
    machine: Machine,
    plaintext: String,
    ciphertext: String,
    anim: Option<Animation>,
    disp: [f32; 3],   // eased display position of each drum (continuous)
    target: [f32; 3], // where each drum is heading (bumps +1 per step)
    spin: [f32; 3],   // "just stepped" glow, decays to 0
    screen: Screen,
    // setup state
    sel: usize,        // selected field 0..=10
    pending: Option<u8>, // first letter of a plug pair being entered
    help_scroll: u16,
    status: Option<(String, Instant)>, // transient footer message
    quit: bool,
}

const N_FIELDS: usize = 11; // reflector, 3 rotors, 3 rings, 3 positions, plugboard

impl App {
    fn new() -> Self {
        let config = Config::default();
        let machine = config.build();
        let p = config.positions;
        App {
            config,
            machine,
            plaintext: String::new(),
            ciphertext: String::new(),
            anim: None,
            disp: [p[0] as f32, p[1] as f32, p[2] as f32],
            target: [p[0] as f32, p[1] as f32, p[2] as f32],
            spin: [0.0; 3],
            screen: Screen::Run,
            sel: 0,
            pending: None,
            help_scroll: 0,
            status: None,
            quit: false,
        }
    }

    /// Rebuild the machine from config and reset the drums to start positions.
    fn apply(&mut self) {
        self.machine = self.config.build();
        let p = self.config.positions;
        for i in 0..3 {
            self.disp[i] = p[i] as f32;
            self.target[i] = p[i] as f32;
            self.spin[i] = 0.0;
        }
        self.plaintext.clear();
        self.ciphertext.clear();
        self.anim = None;
        self.pending = None;
    }

    fn press(&mut self, letter: u8) {
        let trace = self.machine.encode(letter);
        for i in 0..3 {
            if trace.stepped[i] {
                self.target[i] += 1.0; // one click forward
                self.spin[i] = 1.0;
            }
        }
        self.plaintext.push((b'A' + letter) as char);
        self.ciphertext.push((b'A' + trace.lamp) as char);
        self.anim = Some(Animation { trace, hop: 0, hop_timer: Instant::now() });
    }

    fn tick(&mut self) {
        if let Some((_, when)) = &self.status {
            if when.elapsed() >= Duration::from_secs(4) {
                self.status = None;
            }
        }
        for i in 0..3 {
            self.disp[i] += (self.target[i] - self.disp[i]) * 0.35; // ease
            self.spin[i] *= 0.80;
            if self.spin[i] < 0.02 {
                self.spin[i] = 0.0;
            }
        }
        if let Some(a) = &mut self.anim {
            if a.hop_timer.elapsed() >= Duration::from_millis(55) {
                a.hop_timer = Instant::now();
                if a.hop < a.trace.hops.len() {
                    a.hop += 1;
                }
            }
        }
    }

    fn live_lamp(&self) -> Option<u8> {
        let a = self.anim.as_ref()?;
        if a.hop >= a.trace.hops.len() {
            Some(a.trace.lamp)
        } else {
            None
        }
    }

    fn flash(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), Instant::now()));
    }

    /// A full, human-readable transcript of the session: the settings used, the
    /// keystrokes typed, and the ciphertext produced. This is both the record
    /// and the way to verify the machine against any reference Enigma.
    fn session_report(&self) -> String {
        let c = &self.config;
        let ch = |v: u8| (b'A' + v) as char;
        let group = |s: &str| -> String {
            s.as_bytes()
                .chunks(5)
                .map(|c| std::str::from_utf8(c).unwrap())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let plugs = if c.plugs.is_empty() {
            "(none)".to_string()
        } else {
            c.plugs.iter().map(|&(a, b)| format!("{}{}", ch(a), ch(b))).collect::<Vec<_>>().join(" ")
        };
        let (y, mo, d, h, mi, s) = now_utc();
        let end = self.machine.rotors.iter().map(|r| r.window()).collect::<String>();

        format!(
            "Enigmascope session — {y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02} UTC\n\
             ============================================\n\
             Reflector   : {}\n\
             Rotors      : {} {} {}   (left middle right)\n\
             Ring setting: {} {} {}\n\
             Start pos   : {} {} {}\n\
             Plugboard   : {}\n\
             End pos     : {}\n\
             \n\
             Input  (plaintext): {}\n\
             Output (cipher)   : {}\n\
             \n\
             To verify: set any Enigma emulator to the settings above and type the\n\
             input; the output should match exactly.\n",
            REFLECTORS[c.reflector].0,
            ROTORS[c.rotors[0]].name, ROTORS[c.rotors[1]].name, ROTORS[c.rotors[2]].name,
            ch(c.rings[0]), ch(c.rings[1]), ch(c.rings[2]),
            ch(c.positions[0]), ch(c.positions[1]), ch(c.positions[2]),
            plugs,
            end,
            group(&self.plaintext),
            group(&self.ciphertext),
        )
    }

    /// Write the session report to a timestamped file in the current directory.
    fn save_session(&self) -> std::io::Result<String> {
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let name = format!("enigmascope-{secs}.txt");
        let mut f = fs::File::create(&name)?;
        f.write_all(self.session_report().as_bytes())?;
        Ok(name)
    }
}

/// Civil date/time (UTC) from the system clock, no external crates.
/// Uses Howard Hinnant's days-to-civil algorithm.
fn now_utc() -> (i64, u32, u32, u32, u32, u32) {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32, h as u32, mi as u32, s as u32)
}

fn main() -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let mut app = App::new();
    let mut last = Instant::now();

    while !app.quit {
        terminal.draw(|f| ui(f, &app))?;

        if event::poll(Duration::from_millis(16))? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press {
                    handle_key(&mut app, k.code, k.modifiers);
                }
            }
        }

        if last.elapsed() >= Duration::from_millis(16) {
            app.tick();
            last = Instant::now();
        }
    }

    ratatui::restore();

    // Dump the session to normal terminal scrollback (so you can select-copy it)
    // and drop a timestamped log file next to the binary.
    if !app.plaintext.is_empty() {
        println!("\n{}", app.session_report());
        match app.save_session() {
            Ok(name) => println!("Saved session log to ./{name}"),
            Err(e) => eprintln!("Could not save session log: {e}"),
        }
    }
    Ok(())
}

fn handle_key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    if code == KeyCode::Char('c') && mods.contains(KeyModifiers::CONTROL) {
        app.quit = true;
        return;
    }
    match app.screen {
        // In Run, EVERY letter must encipher — commands live on non-letter keys
        // (Tab, ?, Esc) and Ctrl-combos, so nothing collides with your message.
        Screen::Run => match code {
            KeyCode::Esc => app.quit = true,
            KeyCode::Tab => app.screen = Screen::Setup,
            KeyCode::Char('?') => app.screen = Screen::Help,
            KeyCode::Char(c) if mods.contains(KeyModifiers::CONTROL) => {
                match c.to_ascii_lowercase() {
                    's' => match app.save_session() {
                        Ok(name) => app.flash(format!("saved session to ./{name}")),
                        Err(e) => app.flash(format!("save failed: {e}")),
                    },
                    'r' => {
                        app.apply();
                        app.flash("reset to start settings");
                    }
                    _ => {}
                }
            }
            KeyCode::Char(c) if c.is_ascii_alphabetic() => {
                app.press(c.to_ascii_uppercase() as u8 - b'A');
            }
            _ => {}
        },
        Screen::Help => match code {
            KeyCode::Esc | KeyCode::Char('?') => {
                app.screen = Screen::Run;
                app.help_scroll = 0;
            }
            KeyCode::Down => app.help_scroll = app.help_scroll.saturating_add(1),
            KeyCode::Up => app.help_scroll = app.help_scroll.saturating_sub(1),
            _ => {}
        },
        Screen::Setup => handle_setup_key(app, code),
    }
}

fn handle_setup_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Esc => {
            app.screen = Screen::Run;
            app.pending = None;
        }
        KeyCode::Enter => {
            app.apply();
            app.screen = Screen::Run;
        }
        KeyCode::Up => app.sel = (app.sel + N_FIELDS - 1) % N_FIELDS,
        KeyCode::Down => app.sel = (app.sel + 1) % N_FIELDS,
        KeyCode::Left => adjust_field(app, -1),
        KeyCode::Right => adjust_field(app, 1),
        KeyCode::Delete if app.sel == 10 => {
            app.config.plugs.clear();
            app.pending = None;
        }
        KeyCode::Backspace if app.sel == 10 => {
            if app.pending.take().is_none() {
                app.config.plugs.pop();
            }
        }
        KeyCode::Char(c) if app.sel == 10 && c.is_ascii_alphabetic() => {
            let l = c.to_ascii_uppercase() as u8 - b'A';
            match app.pending {
                None => app.pending = Some(l),
                Some(p) if p == l => app.pending = None,
                Some(p) => {
                    app.config.toggle_plug(p, l);
                    app.pending = None;
                }
            }
        }
        _ => {}
    }
}

fn adjust_field(app: &mut App, dir: i32) {
    let c = &mut app.config;
    match app.sel {
        0 => c.reflector = ((c.reflector as i32 + dir).rem_euclid(REFLECTORS.len() as i32)) as usize,
        1..=3 => {
            let slot = app.sel - 1;
            // cycle to next rotor not already used by the other two slots
            let others: Vec<usize> = (0..3).filter(|&s| s != slot).map(|s| c.rotors[s]).collect();
            let mut v = c.rotors[slot] as i32;
            loop {
                v = (v + dir).rem_euclid(ROTORS.len() as i32);
                if !others.contains(&(v as usize)) {
                    break;
                }
            }
            c.rotors[slot] = v as usize;
        }
        4..=6 => {
            let i = app.sel - 4;
            c.rings[i] = ((c.rings[i] as i32 + dir).rem_euclid(26)) as u8;
        }
        7..=9 => {
            let i = app.sel - 7;
            c.positions[i] = ((c.positions[i] as i32 + dir).rem_euclid(26)) as u8;
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn ui(f: &mut Frame, app: &App) {
    match app.screen {
        Screen::Run => run_screen(f, app),
        Screen::Setup => {
            run_screen(f, app);
            setup_overlay(f, app);
        }
        Screen::Help => help_screen(f, app),
    }
}

fn run_screen(f: &mut Frame, app: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // settings line
            Constraint::Length(9), // rotor drums
            Constraint::Length(4), // signal path
            Constraint::Length(5), // lampboard
            Constraint::Min(3),    // tape
            Constraint::Length(1), // footer
        ])
        .split(f.area());

    settings_line(f, rows[0], app);
    rotor_drums(f, rows[1], app);
    signal_path(f, rows[2], app);
    lampboard(f, rows[3], app);
    text_log(f, rows[4], app);
    match &app.status {
        Some((msg, _)) => footer_styled(f, rows[5], msg, LIT),
        None => footer_styled(
            f,
            rows[5],
            "A-Z encipher   Tab setup   ? help   Ctrl+S save   Ctrl+R reset   Esc quit",
            DIM,
        ),
    }
}

fn footer_styled(f: &mut Frame, area: Rect, txt: &str, color: Color) {
    f.render_widget(
        Paragraph::new(Span::styled(format!("  {txt}"), Style::default().fg(color))),
        area,
    );
}

fn settings_line(f: &mut Frame, area: Rect, app: &App) {
    let r = &app.machine.rotors;
    let txt = format!(
        "  Rotors {}-{}-{}    Reflector {}    Ring {}{}{}    Window {}{}{}    Plugs {}",
        r[0].spec.name, r[1].spec.name, r[2].spec.name,
        REFLECTORS[app.config.reflector].0,
        (b'A' + r[0].ring) as char, (b'A' + r[1].ring) as char, (b'A' + r[2].ring) as char,
        r[0].window(), r[1].window(), r[2].window(),
        app.config.plugs.len(),
    );
    f.render_widget(Paragraph::new(Span::styled(txt, Style::default().fg(INK))), area);
}

/// Three drums with a smoothly gliding brightness band = the rotation illusion.
fn rotor_drums(f: &mut Frame, area: Rect, app: &App) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(1, 3); 3])
        .split(area);

    for i in 0..3 {
        let rotor = &app.machine.rotors[i];
        let disp = app.disp[i];
        let stepping = app.spin[i] > 0.05;
        let anchor = disp.round() as i32;
        let frac = disp - anchor as f32; // -0.5..0.5

        let mut lines: Vec<Line> = Vec::new();
        // rows from o=+2 (top) down to o=-2 (bottom); o=0 is the window
        for o in (-2i32..=2).rev() {
            let letter = (b'A' + ((anchor + o).rem_euclid(26)) as u8) as char;
            let dist = (o as f32 - frac).abs(); // how centered this row is
            let color = if dist < 0.35 {
                if stepping { HOT } else { Color::White }
            } else if dist < 0.9 {
                Color::Rgb(150, 152, 160)
            } else {
                DIM
            };
            let style = if dist < 0.35 {
                Style::default().fg(color).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(color)
            };
            let s = if o == 0 {
                format!("> {letter} <")
            } else {
                format!("  {letter}  ")
            };
            lines.push(Line::from(Span::styled(s, style)).alignment(Alignment::Center));
        }

        let border = if stepping { HOT } else { FRAME };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border))
            .title(format!(" {} ", rotor.spec.name))
            .title_alignment(Alignment::Center);
        f.render_widget(Paragraph::new(lines).block(block), cols[i]);
    }
}

fn signal_path(f: &mut Frame, area: Rect, app: &App) {
    let labels = ["KB", "S", "R", "M", "L", "UKW", "L", "M", "R", "S", "LMP"];
    let (hops, active) = match &app.anim {
        Some(a) => (Some(&a.trace.hops), a.hop),
        None => (None, 0usize),
    };

    let mut top = vec![Span::raw("  ")];
    let mut mid = vec![Span::raw("  ")];
    for (i, label) in labels.iter().enumerate() {
        let lit = hops.is_some() && i <= active && i >= 1;
        let very = hops.is_some() && i == active;
        let letter = match (hops, i) {
            (Some(h), 0) => Some((b'A' + h[0].input) as char),
            (Some(h), n) if n >= 1 && n <= h.len() => Some((b'A' + h[n - 1].output) as char),
            _ => None,
        };
        let box_color = if very { HOT } else if lit { WIRE } else { DIM };
        let letter_color = if very { HOT } else if lit { Color::White } else { DIM };

        top.push(Span::styled(
            format!(" {} ", letter.unwrap_or(' ')),
            Style::default().fg(letter_color).add_modifier(if very { Modifier::BOLD } else { Modifier::empty() }),
        ));
        top.push(Span::raw(" "));
        mid.push(Span::styled(format!("[{:^3}]", label), Style::default().fg(box_color)));
        mid.push(Span::styled("-", Style::default().fg(if lit { WIRE } else { DIM })));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(FRAME))
        .title(" signal path  (keyboard > plugboard > rotors > reflector > back > lamp) ");
    f.render_widget(Paragraph::new(vec![Line::from(top), Line::from(mid)]).block(block), area);
}

fn lampboard(f: &mut Frame, area: Rect, app: &App) {
    let live = app.live_lamp();
    let mut lines: Vec<Line> = Vec::new();
    for (r, row) in LAMPS.iter().enumerate() {
        let mut spans = vec![Span::raw(" ".repeat(r * 2 + 2))];
        for ch in row.chars() {
            let on = Some(ch as u8 - b'A') == live;
            let style = if on {
                Style::default().fg(Color::Black).bg(LIT).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Rgb(110, 112, 120))
            };
            spans.push(Span::styled(format!(" {ch} "), style));
        }
        lines.push(Line::from(spans));
    }
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(FRAME)).title(" lampboard ");
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn text_log(f: &mut Frame, area: Rect, app: &App) {
    let group = |s: &str| -> String {
        s.as_bytes().chunks(5).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join(" ")
    };
    let lines = vec![
        Line::from(vec![
            Span::styled("  in   ", Style::default().fg(DIM)),
            Span::styled(group(&app.plaintext), Style::default().fg(INK)),
        ]),
        Line::from(vec![
            Span::styled("  out  ", Style::default().fg(DIM)),
            Span::styled(group(&app.ciphertext), Style::default().fg(LIT).add_modifier(Modifier::BOLD)),
        ]),
    ];
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(FRAME)).title(" tape ");
    f.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), area);
}

fn setup_overlay(f: &mut Frame, app: &App) {
    let area = centered(64, 20, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(HOT))
        .title(" setup  (up/down move, left/right change, Enter apply, Esc cancel) ");
    f.render_widget(block.clone(), area);
    let inner = block.inner(area);

    let c = &app.config;
    let sel = app.sel;
    let field = |idx: usize, label: &str, value: String| -> Line {
        let marker = if sel == idx { "> " } else { "  " };
        let lstyle = if sel == idx {
            Style::default().fg(HOT).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(INK)
        };
        Line::from(vec![
            Span::styled(format!("{marker}{label:<14}"), lstyle),
            Span::styled(value, Style::default().fg(Color::White)),
        ])
    };
    let rn = |i: usize| ROTORS[c.rotors[i]].name.to_string();
    let ch = |v: u8| ((b'A' + v) as char).to_string();

    let plug_str = {
        let mut s: String = c.plugs.iter().map(|&(a, b)| format!("{}{} ", (b'A'+a) as char, (b'A'+b) as char)).collect();
        if let Some(p) = app.pending {
            s.push_str(&format!("[{}_]", (b'A' + p) as char));
        }
        if s.is_empty() { "(none)".into() } else { s }
    };

    let lines = vec![
        field(0, "Reflector", REFLECTORS[c.reflector].0.to_string()),
        Line::raw(""),
        field(1, "Rotor left", rn(0)),
        field(2, "Rotor middle", rn(1)),
        field(3, "Rotor right", rn(2)),
        Line::raw(""),
        field(4, "Ring left", ch(c.rings[0])),
        field(5, "Ring middle", ch(c.rings[1])),
        field(6, "Ring right", ch(c.rings[2])),
        Line::raw(""),
        field(7, "Start left", ch(c.positions[0])),
        field(8, "Start middle", ch(c.positions[1])),
        field(9, "Start right", ch(c.positions[2])),
        Line::raw(""),
        field(10, "Plugboard", plug_str),
        Line::from(Span::styled(
            "                 type two letters to pair (again to remove); Bksp undo, Del clear",
            Style::default().fg(DIM),
        )),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

fn help_screen(f: &mut Frame, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(WIRE))
        .title(" how the Enigma works   (up/down scroll, Esc back) ");
    let inner = block.inner(f.area());
    f.render_widget(block, f.area());

    let h = |s: &str| Line::from(Span::styled(s.to_string(), Style::default().fg(HOT).add_modifier(Modifier::BOLD)));
    let p = |s: &str| Line::from(Span::styled(s.to_string(), Style::default().fg(INK)));

    let lines = vec![
        h("What it is"),
        p("A rotor cipher machine used by Germany in WWII. Each key press sends"),
        p("current through a plugboard, three spinning rotors, a reflector, back"),
        p("out through the rotors and plugboard, and lights one lamp."),
        Line::raw(""),
        h("The path of a single letter"),
        p("KB  -> you press a key."),
        p("S   -> plugboard swaps it (or not) for its paired letter."),
        p("R M L -> three rotors, each a scrambled wiring, right to left."),
        p("UKW -> reflector bounces it back into a different wire."),
        p("L M R -> back out through the rotors on new paths."),
        p("S   -> plugboard swaps once more, and a lamp lights."),
        Line::raw(""),
        h("Why it is reciprocal"),
        p("Because of the reflector, the wiring is symmetric: if A encodes to G"),
        p("at some setting, then G encodes to A at that same setting. Encrypt and"),
        p("decrypt are the same operation on identically set machines."),
        p("A side effect: no letter can ever encode to itself. That flaw helped"),
        p("Bletchley Park find cribs and break the cipher."),
        Line::raw(""),
        h("Stepping and the double step"),
        p("The right rotor advances every keypress. When it reaches its notch it"),
        p("kicks the middle rotor; when the middle reaches its notch it kicks the"),
        p("left AND advances itself again the next press. That double step is a"),
        p("real mechanical quirk, faithfully reproduced here."),
        Line::raw(""),
        h("What you can tune (press Tab)"),
        p("Reflector  - B or C, the fixed bounce-back wiring."),
        p("Rotor order- which of I-V sit left/middle/right; order changes everything."),
        p("Ring (Ringstellung) - rotates wiring relative to the letter ring."),
        p("Start (Grundstellung) - the letters showing at the start of a message."),
        p("Plugboard  - up to 10 letter swaps before and after the rotors."),
        Line::raw(""),
        p("Together these are the daily key. Two machines set identically will"),
        p("decode each other; change any one setting and the output diverges."),
        Line::raw(""),
        h("Keys and saving your work"),
        p("Every A-Z key enciphers - no letter is a command, so nothing collides"),
        p("with your message. Commands are Tab (setup), ? (help), Ctrl+S (save a"),
        p("session log now), Ctrl+R (reset), Esc (quit)."),
        p("On save or quit, a timestamped enigmascope-*.txt file is written with the"),
        p("settings, your input, and the cipher output - and the transcript is also"),
        p("printed to the terminal when you quit, so you can select and copy it."),
    ];

    f.render_widget(
        Paragraph::new(Text::from(lines)).scroll((app.help_scroll, 0)).wrap(Wrap { trim: false }),
        inner,
    );
}

fn centered(w: u16, h: u16, area: Rect) -> Rect {
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect { x, y, width: w.min(area.width), height: h.min(area.height) }
}

#[cfg(test)]
mod app_tests {
    use super::*;

    #[test]
    fn session_report_captures_io_and_settings() {
        let mut app = App::new(); // I-II-III, reflector B, AAA/AAA, plugs A-M & F-T
        for ch in "HELLO".bytes() {
            app.press(ch - b'A');
        }
        let report = app.session_report();
        assert!(report.contains("Rotors      : I II III"));
        assert!(report.contains("Reflector   : B"));
        assert!(report.contains("Input  (plaintext): HELLO"));
        // ciphertext must be 5 letters, none equal to its plaintext letter
        assert_eq!(app.ciphertext.len(), 5);
        for (p, c) in app.plaintext.bytes().zip(app.ciphertext.bytes()) {
            assert_ne!(p, c, "no letter may encipher to itself");
        }
    }

    #[test]
    fn reset_returns_to_start_and_clears_tape() {
        let mut app = App::new();
        for ch in "TESTING".bytes() {
            app.press(ch - b'A');
        }
        app.apply(); // Ctrl+R path
        assert!(app.plaintext.is_empty());
        assert_eq!(app.machine.rotors[2].window(), 'A'); // right rotor back to start
    }
}

