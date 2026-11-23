//! A terminal: a child process on a pty, alacritty's emulation of its screen, and the calls
//! an application draws it and types into it with.
//!
//! Three threads are involved. The application's thread calls the methods of [`Terminal`].
//! alacritty's event loop runs on a reader thread, which reads the pty, parses what comes
//! out into the grid and writes what goes in. A third thread hands events to the
//! application's callback, so the callback never runs while the terminal is locked, and
//! answers the child's queries in the order they were asked.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll as GridScroll};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{color, Config, Osc52, Term, MIN_COLUMNS, MIN_SCREEN_LINES};
use alacritty_terminal::vte::ansi::{NamedColor, Rgb as AnsiRgb};
use bakedit_core::{Key, KeyEvent, KeyKind, MouseEvent, Rgb};

use crate::config::{TermColors, TermConfig};
use crate::error::Result;
use crate::event::TermEvent;
use crate::input::{encode_alt_scroll, encode_focus, encode_mouse, encode_paste};
use crate::keys::encode_key;
use crate::osc::OscScanner;
use crate::pty::{self, lock, ChildHandle};
use crate::screen::{self, Cell, Cursor, Drag, ScreenLink, SelectionKind, TermModes};

/// The shortest time between two bells passed on to the application.
const BELL_INTERVAL: Duration = Duration::from_millis(100);
/// Clipboard requests the child may have outstanding before the oldest is dropped.
const MAX_CLIPBOARD_REQUESTS: usize = 16;

type ClipboardFormat = Arc<dyn Fn(&str) -> String + Send + Sync>;
type ColorFormat = Arc<dyn Fn(AnsiRgb) -> String + Send + Sync>;
type SizeFormat = Arc<dyn Fn(WindowSize) -> String + Send + Sync>;

/// How far to move the view into the scrollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroll {
    /// By this many lines: positive towards older output, negative back towards the
    /// present.
    Lines(i32),
    /// One screen towards older output.
    PageUp,
    /// One screen back towards the present.
    PageDown,
    /// To the oldest line kept.
    Top,
    /// Back to the live screen.
    Bottom,
}

/// The size of the terminal: cells for the grid, pixels for the programs that ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Geometry {
    cols: u16,
    rows: u16,
    cell_width: u16,
    cell_height: u16,
}

impl Geometry {
    fn new(cols: u16, rows: u16, cell_width: u16, cell_height: u16) -> Geometry {
        Geometry {
            cols: cols.max(MIN_COLUMNS as u16),
            rows: rows.max(MIN_SCREEN_LINES as u16),
            cell_width: cell_width.max(1),
            cell_height: cell_height.max(1),
        }
    }

    fn window_size(self) -> WindowSize {
        WindowSize {
            num_lines: self.rows,
            num_cols: self.cols,
            cell_width: self.cell_width,
            cell_height: self.cell_height,
        }
    }
}

impl Dimensions for Geometry {
    fn total_lines(&self) -> usize {
        usize::from(self.rows)
    }

    fn screen_lines(&self) -> usize {
        usize::from(self.rows)
    }

    fn columns(&self) -> usize {
        usize::from(self.cols)
    }
}

/// State shared between the application's thread, the reader and the dispatcher. Each lock
/// guards one small value and is never held while another is taken, except that `drag` is
/// only ever touched with the terminal itself locked.
struct Shared {
    /// Counts changes to what is on screen.
    generation: AtomicU64,
    /// Whether the child has not been seen to exit.
    running: AtomicBool,
    /// Set when the `Terminal` is dropped: no more events for the application.
    closed: AtomicBool,
    focused: AtomicBool,
    title: Mutex<Option<String>>,
    geometry: Mutex<Geometry>,
    colors: Mutex<TermColors>,
    exit_code: Mutex<Option<i32>>,
    clipboard_requests: Mutex<VecDeque<ClipboardFormat>>,
    drag: Mutex<Option<Drag>>,
}

/// What travels from the reader thread to the dispatcher.
enum Internal {
    /// The grid changed.
    Wakeup,
    /// Something to pass straight on.
    Event(TermEvent),
    Bell,
    /// An answer for the child, ready to write.
    Reply(String),
    /// The child asked for a colour; the answer depends on the terminal's state.
    Color(usize, ColorFormat),
    /// The child asked for the size of the text area in pixels.
    TextAreaSize(SizeFormat),
    /// The child exited and its last output has been parsed.
    Exited,
    /// The pty went away with the child still running.
    Lost,
}

/// alacritty's window onto the outside world. It is called on the reader thread with the
/// terminal locked, so all it does is note the cheap things and queue the rest.
#[derive(Clone)]
struct Listener {
    queue: Sender<Internal>,
    shared: Arc<Shared>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let message = match event {
            Event::Wakeup => {
                self.shared.generation.fetch_add(1, Ordering::Relaxed);
                Internal::Wakeup
            }
            Event::Title(title) => {
                let mut current = lock(&self.shared.title);
                if current.as_deref() == Some(title.as_str()) {
                    return;
                }
                *current = Some(title.clone());
                Internal::Event(TermEvent::Title(title))
            }
            Event::ResetTitle => {
                if lock(&self.shared.title).take().is_none() {
                    return;
                }
                Internal::Event(TermEvent::ResetTitle)
            }
            Event::Bell => Internal::Bell,
            Event::ClipboardStore(_, text) => Internal::Event(TermEvent::ClipboardStore(text)),
            Event::ClipboardLoad(_, format) => {
                let mut requests = lock(&self.shared.clipboard_requests);
                if requests.len() >= MAX_CLIPBOARD_REQUESTS {
                    requests.pop_front();
                }
                requests.push_back(format);
                Internal::Event(TermEvent::ClipboardLoad)
            }
            Event::ColorRequest(index, format) => Internal::Color(index, format),
            Event::PtyWrite(text) => Internal::Reply(text),
            Event::TextAreaSizeRequest(format) => Internal::TextAreaSize(format),
            Event::ChildExit(status) => {
                *lock(&self.shared.exit_code) = status.code();
                self.shared.running.store(false, Ordering::Relaxed);
                return;
            }
            Event::Exit => Internal::Exited,
            Event::CursorBlinkingChange | Event::MouseCursorDirty => return,
        };
        let _ = self.queue.send(message);
    }
}

/// Spaces out wakeups: the first goes out at once, further ones within `interval` of the
/// last are folded into a single one that goes out when the interval is over.
#[derive(Debug)]
struct Throttle {
    interval: Duration,
    last: Option<Instant>,
    due: Option<Instant>,
}

impl Throttle {
    fn new(interval: Duration) -> Throttle {
        Throttle {
            interval,
            last: None,
            due: None,
        }
    }

    /// Something changed at `now`. Returns whether to wake the application right away; if
    /// not, a wakeup is owed at [`Throttle::deadline`].
    fn request(&mut self, now: Instant) -> bool {
        if self.due.is_some() {
            return false;
        }
        match self.last {
            Some(last) if now < last + self.interval => {
                self.due = Some(last + self.interval);
                false
            }
            _ => {
                self.last = Some(now);
                true
            }
        }
    }

    /// When the owed wakeup has to go out.
    fn deadline(&self) -> Option<Instant> {
        self.due
    }

    /// Returns whether an owed wakeup has come due by `now`, and settles it if so.
    fn fire(&mut self, now: Instant) -> bool {
        match self.due {
            Some(due) if now >= due => {
                self.due = None;
                self.last = Some(now);
                true
            }
            _ => false,
        }
    }

    /// Returns whether a wakeup is owed, and settles it without waiting.
    fn flush(&mut self, now: Instant) -> bool {
        let owed = self.due.take().is_some();
        if owed {
            self.last = Some(now);
        }
        owed
    }
}

/// The dispatcher thread: turns what the reader queued into events for the application and
/// answers for the child.
struct Dispatcher {
    queue: Receiver<Internal>,
    on_event: Box<dyn Fn(TermEvent) + Send + Sync>,
    term: Weak<FairMutex<Term<Listener>>>,
    pty: EventLoopSender,
    shared: Arc<Shared>,
    wakeups: Throttle,
    last_bell: Option<Instant>,
    exit_reported: bool,
}

impl Dispatcher {
    fn run(mut self) {
        loop {
            let now = Instant::now();
            if self.wakeups.fire(now) {
                self.emit(TermEvent::Wakeup);
            }
            let message = match self.wakeups.deadline() {
                Some(deadline) => match self
                    .queue
                    .recv_timeout(deadline.saturating_duration_since(now))
                {
                    Ok(message) => message,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break,
                },
                None => match self.queue.recv() {
                    Ok(message) => message,
                    Err(_) => break,
                },
            };
            self.handle(message);
        }
        if self.wakeups.flush(Instant::now()) {
            self.emit(TermEvent::Wakeup);
        }
    }

    fn handle(&mut self, message: Internal) {
        match message {
            Internal::Wakeup => {
                if self.wakeups.request(Instant::now()) {
                    self.emit(TermEvent::Wakeup);
                }
            }
            Internal::Event(event) => self.emit(event),
            Internal::Bell => {
                let now = Instant::now();
                if self
                    .last_bell
                    .is_none_or(|last| now.duration_since(last) >= BELL_INTERVAL)
                {
                    self.last_bell = Some(now);
                    self.emit(TermEvent::Bell);
                }
            }
            Internal::Reply(text) => self.reply(text),
            Internal::Color(index, format) => {
                if let Some(color) = self.color(index) {
                    self.reply(format(color));
                }
            }
            Internal::TextAreaSize(format) => {
                let size = lock(&self.shared.geometry).window_size();
                self.reply(format(size));
            }
            Internal::Exited => self.report_exit(true),
            Internal::Lost => self.report_exit(false),
        }
    }

    fn emit(&self, event: TermEvent) {
        if !self.shared.closed.load(Ordering::Relaxed) {
            (self.on_event)(event);
        }
    }

    fn reply(&self, text: String) {
        if !text.is_empty() {
            let _ = self.pty.send(Msg::Input(Cow::Owned(text.into_bytes())));
        }
    }

    /// The colour the child asked about: its own redefinition if it made one, otherwise
    /// the application's.
    fn color(&self, index: usize) -> Option<AnsiRgb> {
        if index >= color::COUNT {
            return None;
        }
        let redefined = match self.term.upgrade() {
            Some(term) => term.lock().colors()[index],
            None => None,
        };
        Some(redefined.unwrap_or_else(|| {
            let color = named_color(&lock(&self.shared.colors), index);
            AnsiRgb {
                r: color.r,
                g: color.g,
                b: color.b,
            }
        }))
    }

    fn report_exit(&mut self, with_status: bool) {
        if self.exit_reported {
            return;
        }
        self.exit_reported = true;
        self.shared.running.store(false, Ordering::Relaxed);
        // The application hears about the last output before it hears about the exit.
        if self.wakeups.flush(Instant::now()) {
            self.emit(TermEvent::Wakeup);
        }
        let code = if with_status {
            *lock(&self.shared.exit_code)
        } else {
            None
        };
        self.emit(TermEvent::Exit(code));
    }
}

/// The application's value for one of alacritty's colour slots: the 256 palette entries,
/// then foreground, background, cursor, the eight dim colours, bright and dim foreground.
fn named_color(colors: &TermColors, index: usize) -> Rgb {
    let dim = |color: Rgb| {
        Rgb::new(
            (u16::from(color.r) * 2 / 3) as u8,
            (u16::from(color.g) * 2 / 3) as u8,
            (u16::from(color.b) * 2 / 3) as u8,
        )
    };
    const FOREGROUND: usize = NamedColor::Foreground as usize;
    const BACKGROUND: usize = NamedColor::Background as usize;
    const CURSOR: usize = NamedColor::Cursor as usize;
    const DIM_BLACK: usize = NamedColor::DimBlack as usize;
    const DIM_WHITE: usize = NamedColor::DimWhite as usize;
    const DIM_FOREGROUND: usize = NamedColor::DimForeground as usize;
    match index {
        0..=255 => colors.indexed(index as u8),
        BACKGROUND => colors.bg,
        CURSOR => colors.cursor,
        DIM_BLACK..=DIM_WHITE => dim(colors.ansi[index - DIM_BLACK]),
        DIM_FOREGROUND => dim(colors.fg),
        FOREGROUND => colors.fg,
        // Bright foreground, and anything alacritty adds later.
        _ => colors.fg,
    }
}

/// A terminal with a program running in it.
///
/// Every method takes `&self` and may be called from any thread. Dropping the terminal
/// hangs up on the child (`SIGHUP` to its process group) and stops the events.
pub struct Terminal {
    term: Arc<FairMutex<Term<Listener>>>,
    pty: EventLoopSender,
    shared: Arc<Shared>,
    child: Arc<ChildHandle>,
    alt_scroll_lines: u16,
}

impl Terminal {
    /// Start the program in `config` on a new pty and begin emulating its screen.
    ///
    /// The child's environment is this process's, with `TERM=xterm-256color`,
    /// `COLORTERM=truecolor`, `TERM_PROGRAM=bakedit` and `TERM_PROGRAM_VERSION` set, `PWD`
    /// set to the working directory when one is given, then `config.env` applied and
    /// `config.env_remove` removed.
    ///
    /// `on_event` is called on a thread of the terminal's own, one event at a time and in
    /// order. No lock is held while it runs, so it may call back into the terminal, but it
    /// holds up the answers to the child's queries for as long as it takes: do no more in
    /// it than passing the event on to the application's loop. Events can arrive before
    /// this function has returned.
    ///
    /// Fails when no pty can be had or the program cannot be run; see
    /// [`TermError::is_program_missing`](crate::TermError::is_program_missing) for the
    /// failure worth falling back to a shell for.
    pub fn spawn(
        config: TermConfig,
        on_event: Box<dyn Fn(TermEvent) + Send + Sync>,
    ) -> Result<Terminal> {
        let geometry = Geometry::new(
            config.cols,
            config.rows,
            config.cell_width,
            config.cell_height,
        );
        let (queue, receiver) = mpsc::channel();
        let shared = Arc::new(Shared {
            generation: AtomicU64::new(0),
            running: AtomicBool::new(true),
            closed: AtomicBool::new(false),
            focused: AtomicBool::new(true),
            title: Mutex::new(None),
            geometry: Mutex::new(geometry),
            colors: Mutex::new(config.colors),
            exit_code: Mutex::new(None),
            clipboard_requests: Mutex::new(VecDeque::new()),
            drag: Mutex::new(None),
        });
        let listener = Listener {
            queue: queue.clone(),
            shared: shared.clone(),
        };

        let options = Config {
            scrolling_history: config.scrollback,
            // The child may push kitty keyboard flags, and may ask for the clipboard: the
            // application decides whether it gets an answer.
            kitty_keyboard: true,
            osc52: Osc52::CopyPaste,
            ..Config::default()
        };
        let term = Arc::new(FairMutex::new(Term::new(
            options,
            &geometry,
            listener.clone(),
        )));

        let mut scanner = OscScanner::for_this_host();
        let tap_queue = queue.clone();
        let tap = Box::new(move |bytes: &[u8]| {
            scanner.feed(bytes, &mut |event| {
                let _ = tap_queue.send(Internal::Event(event));
            });
        });
        let on_abandon = Box::new(move || {
            let _ = queue.send(Internal::Lost);
        });
        let pty = pty::spawn(&config, geometry.window_size(), tap, on_abandon)?;
        let child = pty.child_handle();

        let event_loop = EventLoop::new(term.clone(), listener, pty, true, false)?;
        let sender = event_loop.channel();
        let dispatcher = Dispatcher {
            queue: receiver,
            on_event,
            term: Arc::downgrade(&term),
            pty: sender.clone(),
            shared: shared.clone(),
            wakeups: Throttle::new(config.wakeup_interval),
            last_bell: None,
            exit_reported: false,
        };
        std::thread::Builder::new()
            .name("bakedit-term-events".to_string())
            .spawn(move || dispatcher.run())?;
        // The reader thread is left to itself: it ends when the child does or when the
        // terminal is dropped, and takes the pty with it.
        drop(event_loop.spawn());

        Ok(Terminal {
            term,
            pty: sender,
            shared,
            child,
            alt_scroll_lines: config.alt_scroll_lines,
        })
    }

    // ---- the process ------------------------------------------------------------------

    /// The process id of the child.
    pub fn pid(&self) -> u32 {
        self.child.pid()
    }

    /// Whether the child has not exited yet.
    pub fn is_running(&self) -> bool {
        self.shared.running.load(Ordering::Relaxed)
    }

    /// Hang up on the child: `SIGHUP` to its process group, which is the child and what it
    /// started without job control. A shell passes the hangup on to its jobs. Returns
    /// whether a signal was sent; `false` once the child has exited.
    ///
    /// The exit shows up as [`TermEvent::Exit`] like any other, with no exit code. A child
    /// that ignores the hangup keeps running.
    pub fn kill(&self) -> bool {
        self.child.hang_up()
    }

    // ---- size and state ---------------------------------------------------------------

    /// The size of the terminal in cells: `(cols, rows)`.
    pub fn size(&self) -> (u16, u16) {
        let geometry = lock(&self.shared.geometry);
        (geometry.cols, geometry.rows)
    }

    /// Resize the grid and tell the child (`TIOCSWINSZ`, which sends it `SIGWINCH`). Lines
    /// are rewrapped; a size below 2 columns or 1 row is raised to that. Nothing happens
    /// when the size is the one the terminal already has.
    pub fn resize(&self, cols: u16, rows: u16) {
        let window = {
            let mut geometry = lock(&self.shared.geometry);
            let resized = Geometry::new(cols, rows, geometry.cell_width, geometry.cell_height);
            if resized == *geometry {
                return;
            }
            *geometry = resized;
            resized
        };
        // The grid first: the child redraws as soon as it hears of the new size.
        self.term.lock().resize(window);
        *lock(&self.shared.drag) = None;
        let _ = self.pty.send(Msg::Resize(window.window_size()));
        self.touch();
    }

    /// Set the size of one cell in pixels, which is what the pixel fields of the window
    /// size and the answer to `CSI 14 t` are computed from. The child is told like for a
    /// resize.
    pub fn set_cell_size(&self, width: u16, height: u16) {
        let window = {
            let mut geometry = lock(&self.shared.geometry);
            let resized = Geometry::new(geometry.cols, geometry.rows, width, height);
            if resized == *geometry {
                return;
            }
            *geometry = resized;
            resized
        };
        let _ = self.pty.send(Msg::Resize(window.window_size()));
    }

    /// The window title the child set, if it set one.
    pub fn title(&self) -> Option<String> {
        lock(&self.shared.title).clone()
    }

    /// The modes the child has put the terminal in.
    pub fn modes(&self) -> TermModes {
        TermModes::from_mode(*self.term.lock().mode())
    }

    /// A counter that goes up whenever what [`Terminal::render`] or [`Terminal::cursor`]
    /// would show may have changed: output from the child, a resize, scrolling, a
    /// selection, the focus. Compare it with the value at the last draw to skip a redraw.
    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Relaxed)
    }

    /// Replace the colours reported to the child, after a theme change.
    pub fn set_colors(&self, colors: TermColors) {
        *lock(&self.shared.colors) = colors;
        self.touch();
    }

    // ---- reading the screen -----------------------------------------------------------

    /// Visit every cell of the visible screen, row by row and left to right, as
    /// `f(col, row, cell)`. With the view scrolled back this is the scrollback being
    /// looked at, not the live screen.
    ///
    /// The terminal is locked for the duration: `f` must not call back into it.
    pub fn render(&self, f: &mut dyn FnMut(u16, u16, &Cell<'_>)) {
        let term = self.term.lock();
        let colors = *lock(&self.shared.colors);
        screen::render(&term, &colors, f);
    }

    /// The cursor: `None` when the child has hidden it or the view is scrolled back so far
    /// that its row is off the screen.
    pub fn cursor(&self) -> Option<Cursor> {
        let focused = self.shared.focused.load(Ordering::Relaxed);
        screen::cursor(&self.term.lock(), focused)
    }

    /// The text of one row of the visible screen, without trailing blanks. A wide
    /// character is one character, and combining marks follow their base. Empty for a row
    /// past the bottom.
    pub fn row_text(&self, row: u16) -> String {
        screen::row_text(&self.term.lock(), row)
    }

    /// The text of the visible screen: every row as [`Terminal::row_text`] gives it,
    /// joined by newlines.
    pub fn screen_text(&self) -> String {
        screen::screen_text(&self.term.lock())
    }

    // ---- scrollback -------------------------------------------------------------------

    /// Move the view within the scrollback. Returns whether it moved. The alternate
    /// screen has no scrollback, so there this never moves anything.
    pub fn scroll(&self, scroll: Scroll) -> bool {
        let moved = {
            let mut term = self.term.lock();
            let before = term.grid().display_offset();
            term.scroll_display(match scroll {
                Scroll::Lines(lines) => GridScroll::Delta(lines),
                Scroll::PageUp => GridScroll::PageUp,
                Scroll::PageDown => GridScroll::PageDown,
                Scroll::Top => GridScroll::Top,
                Scroll::Bottom => GridScroll::Bottom,
            });
            term.grid().display_offset() != before
        };
        if moved {
            self.touch();
        }
        moved
    }

    /// How many lines the view is scrolled back: 0 is the live screen.
    pub fn scroll_offset(&self) -> usize {
        self.term.lock().grid().display_offset()
    }

    /// How many lines of scrollback there are above the live screen.
    pub fn history_len(&self) -> usize {
        self.term.lock().history_size()
    }

    // ---- selection --------------------------------------------------------------------

    /// Start a selection at a cell of the visible screen, replacing any other. Meant for
    /// when the child has not asked for the mouse; see [`Terminal::send_mouse`].
    ///
    /// A [`SelectionKind::Simple`] selection is empty until it is updated, so a click
    /// without a drag selects nothing. Word and line selections cover the word or line
    /// under the cell at once. Positions off the screen are clamped to it.
    pub fn selection_start(&self, col: u16, row: u16, kind: SelectionKind) {
        {
            let mut term = self.term.lock();
            let drag = screen::selection_start(&mut term, col, row, kind);
            *lock(&self.shared.drag) = Some(drag);
        }
        self.touch();
    }

    /// Extend the selection to a cell of the visible screen. Both the cell it started at
    /// and this one are included, whichever comes first. Does nothing when no selection
    /// has been started, or when the terminal dropped it because its text was erased.
    pub fn selection_update(&self, col: u16, row: u16) {
        let changed = {
            let mut term = self.term.lock();
            let mut drag = lock(&self.shared.drag);
            match drag.as_mut() {
                Some(drag) => screen::selection_update(&mut term, drag, col, row),
                None => false,
            }
        };
        if changed {
            self.touch();
        }
    }

    /// Drop the selection.
    pub fn selection_clear(&self) {
        let cleared = {
            let mut term = self.term.lock();
            *lock(&self.shared.drag) = None;
            term.selection.take().is_some()
        };
        if cleared {
            self.touch();
        }
    }

    /// The selected text: rows joined by newlines, except where a row is the soft-wrapped
    /// continuation of the one before. `None` when nothing is selected.
    pub fn selection_text(&self) -> Option<String> {
        screen::selection_text(&self.term.lock())
    }

    // ---- input ------------------------------------------------------------------------

    /// Queue bytes for the child, as they are. Returns `false` when there is nothing to
    /// write or nobody to write to (the child has exited). Writing never blocks.
    pub fn write(&self, bytes: &[u8]) -> bool {
        !bytes.is_empty()
            && self
                .pty
                .send(Msg::Input(Cow::Owned(bytes.to_vec())))
                .is_ok()
    }

    /// Send a key, encoded for the keyboard mode the child is in (see
    /// [`encode_key`]). Returns `false` when the key produces no bytes there, such as a
    /// release the child did not ask to hear about.
    ///
    /// A key that goes through brings the view back to the live screen and drops the
    /// selection, as typing in any terminal does.
    pub fn send_key(&self, key: &KeyEvent) -> bool {
        let bytes = encode_key(key, self.modes().key_modes());
        if bytes.is_empty() {
            return false;
        }
        if key.kind != KeyKind::Release && !matches!(key.key, Key::Modifier(_)) {
            self.follow_input();
        }
        self.write(&bytes)
    }

    /// Send a mouse event to a child that has asked for the mouse. `mouse.col` and
    /// `mouse.row` are cells from the terminal's top-left corner; positions past the edge
    /// are clamped to it.
    ///
    /// Returns `true` when the event was the child's business: it has mouse reporting on
    /// (even if this particular event is a motion it did not ask for and nothing was
    /// sent), or the event was a wheel step on the alternate screen that went out as
    /// arrow keys. Returns `false` otherwise, and the application then does what the
    /// event means locally: selecting, or scrolling the scrollback.
    pub fn send_mouse(&self, mouse: &MouseEvent) -> bool {
        let (modes, cols, rows) = {
            let term = self.term.lock();
            (
                TermModes::from_mode(*term.mode()),
                term.columns(),
                term.screen_lines(),
            )
        };
        let mut event = *mouse;
        event.col = event.col.min(cols as u16 - 1);
        event.row = event.row.min(rows as u16 - 1);
        if modes.mouse_reporting {
            if let Some(bytes) = encode_mouse(&event, modes.mouse_modes()) {
                self.write(&bytes);
            }
            return true;
        }
        if modes.alt_screen && modes.alternate_scroll {
            if let Some(bytes) =
                encode_alt_scroll(event.kind, modes.app_cursor, self.alt_scroll_lines)
            {
                self.write(&bytes);
                return true;
            }
        }
        false
    }

    /// Paste text: bracketed when the child asked for that, see [`encode_paste`]. Returns
    /// whether anything was sent.
    pub fn send_paste(&self, text: &str) -> bool {
        let bytes = encode_paste(text, self.modes().bracketed_paste);
        if bytes.is_empty() {
            return false;
        }
        self.follow_input();
        self.write(&bytes)
    }

    /// Tell the terminal whether it has the keyboard focus. Without it the cursor is
    /// reported as [`CursorShape::Hollow`](crate::CursorShape::Hollow). A child that asked
    /// for focus events (mode 1004) is told; returns whether it was.
    pub fn send_focus(&self, focused: bool) -> bool {
        if self.shared.focused.swap(focused, Ordering::Relaxed) != focused {
            self.touch();
        }
        self.modes().focus_reporting && self.write(encode_focus(focused))
    }

    /// Send text as it is, with no encoding and no bracketing: for key sequences the
    /// application spells out itself, such as a herdr prefix chord (`"\u{2}h"`).
    pub fn send_text(&self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        self.follow_input();
        self.write(text.as_bytes())
    }

    /// Answer the oldest unanswered [`TermEvent::ClipboardLoad`] with the clipboard's
    /// text. Returns `false` when the child is not waiting for one.
    pub fn respond_clipboard(&self, text: &str) -> bool {
        let format = lock(&self.shared.clipboard_requests).pop_front();
        match format {
            Some(format) => self.write(format(text).as_bytes()),
            None => false,
        }
    }

    // ---- links ------------------------------------------------------------------------

    /// The link under the cell at `(col, row)`: the segment of it on that row. Find the
    /// rest of a wrapped link by its `id` in [`Terminal::visible_links`].
    pub fn link_at(&self, col: u16, row: u16) -> Option<ScreenLink> {
        self.visible_links()
            .into_iter()
            .find(|link| link.contains(col, row))
    }

    /// Every link on the visible screen, top to bottom and left to right: what
    /// [`detect_links`](crate::detect_links) finds in the text, with soft-wrapped rows
    /// joined first, plus the hyperlinks programs made themselves (OSC 8).
    ///
    /// File paths are candidates, not checked against the disk: resolve one with
    /// [`LinkTarget::resolve`](crate::LinkTarget::resolve) before treating it as a link.
    pub fn visible_links(&self) -> Vec<ScreenLink> {
        screen::links(&self.term.lock())
    }

    // ---- internals --------------------------------------------------------------------

    fn touch(&self) {
        self.shared.generation.fetch_add(1, Ordering::Relaxed);
    }

    /// What typing does to the view: back to the live screen, selection gone.
    fn follow_input(&self) {
        let changed = {
            let mut term = self.term.lock();
            let scrolled = term.grid().display_offset() != 0;
            if scrolled {
                term.scroll_display(GridScroll::Bottom);
            }
            *lock(&self.shared.drag) = None;
            term.selection.take().is_some() || scrolled
        };
        if changed {
            self.touch();
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Relaxed);
        // The reader thread stops, and the pty it owns hangs up on the child as it goes.
        let _ = self.pty.send(Msg::Shutdown);
    }
}

impl fmt::Debug for Terminal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (cols, rows) = self.size();
        f.debug_struct("Terminal")
            .field("pid", &self.pid())
            .field("cols", &cols)
            .field("rows", &rows)
            .field("running", &self.is_running())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminal_can_be_shared_between_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Terminal>();
    }

    #[test]
    fn the_first_wakeup_is_immediate_and_the_rest_are_spaced() {
        let interval = Duration::from_millis(10);
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut throttle = Throttle::new(interval);

        assert!(throttle.request(at(0)));
        assert_eq!(throttle.deadline(), None);
        // A burst right after: one wakeup is owed, however many requests there are.
        assert!(!throttle.request(at(1)));
        assert!(!throttle.request(at(2)));
        assert!(!throttle.request(at(9)));
        assert_eq!(throttle.deadline(), Some(at(10)));
        assert!(!throttle.fire(at(9)));
        assert!(throttle.fire(at(10)));
        assert_eq!(throttle.deadline(), None);
        assert!(!throttle.fire(at(11)));
        // The owed wakeup counts as the last one.
        assert!(!throttle.request(at(15)));
        assert_eq!(throttle.deadline(), Some(at(20)));
        assert!(throttle.fire(at(25)));
        // After a quiet spell the next change is immediate again.
        assert!(throttle.request(at(100)));
        assert_eq!(throttle.deadline(), None);
    }

    #[test]
    fn an_owed_wakeup_can_be_flushed_early() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut throttle = Throttle::new(Duration::from_millis(10));
        assert!(!throttle.flush(at(0)));
        assert!(throttle.request(at(0)));
        assert!(!throttle.request(at(3)));
        assert!(throttle.flush(at(4)));
        assert!(!throttle.flush(at(4)));
        assert_eq!(throttle.deadline(), None);
        // The flush was a wakeup like any other.
        assert!(!throttle.request(at(5)));
        assert_eq!(throttle.deadline(), Some(at(14)));
    }

    #[test]
    fn no_interval_means_no_throttling() {
        let now = Instant::now();
        let mut throttle = Throttle::new(Duration::ZERO);
        for _ in 0..5 {
            assert!(throttle.request(now));
            assert_eq!(throttle.deadline(), None);
        }
    }

    #[test]
    fn colour_slots_map_to_the_applications_colours() {
        let colors = TermColors::default();
        assert_eq!(named_color(&colors, 0), colors.ansi[0]);
        assert_eq!(named_color(&colors, 9), colors.ansi[9]);
        assert_eq!(named_color(&colors, 196), Rgb::new(255, 0, 0));
        assert_eq!(
            named_color(&colors, NamedColor::Foreground as usize),
            colors.fg
        );
        assert_eq!(
            named_color(&colors, NamedColor::Background as usize),
            colors.bg
        );
        assert_eq!(
            named_color(&colors, NamedColor::Cursor as usize),
            colors.cursor
        );
        assert_eq!(
            named_color(&colors, NamedColor::BrightForeground as usize),
            colors.fg
        );
        // Dim colours are two thirds of the colour they dim.
        assert_eq!(
            named_color(&colors, NamedColor::DimRed as usize),
            Rgb::new(0x88, 0, 0)
        );
        assert_eq!(
            named_color(&colors, NamedColor::DimForeground as usize),
            Rgb::new(0x98, 0x98, 0x98)
        );
        // Every slot alacritty has gets an answer.
        for index in 0..color::COUNT {
            let _ = named_color(&colors, index);
        }
    }

    #[test]
    fn sizes_are_never_smaller_than_the_emulator_can_take() {
        let geometry = Geometry::new(0, 0, 0, 0);
        assert_eq!(
            (
                geometry.cols,
                geometry.rows,
                geometry.cell_width,
                geometry.cell_height
            ),
            (2, 1, 1, 1)
        );
        let geometry = Geometry::new(120, 40, 9, 18);
        assert_eq!(
            (
                geometry.columns(),
                geometry.screen_lines(),
                geometry.total_lines()
            ),
            (120, 40, 40)
        );
        let window = geometry.window_size();
        assert_eq!(
            (
                window.num_cols,
                window.num_lines,
                window.cell_width,
                window.cell_height
            ),
            (120, 40, 9, 18)
        );
    }
}
