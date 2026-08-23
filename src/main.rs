//! textfold — a terminal text editor that works the way everything else does.
//!
//! Keyboard and mouse both, language servers and syntax colouring out of the
//! box, and a set of keys nobody has to be taught. One binary, no plugins to
//! install before it is usable, and a settings file you only write the parts
//! of that you disagree with.

mod app;
mod cmd;
mod config;
mod doc;
mod edit;
mod keys;
mod lang;
mod lsp;
mod picker;
mod syntax;
mod term;
mod text;
mod theme;
mod ui;
mod view;

use std::io::{self, Stdout, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};

use anyhow::{Context, Result};
use clap::Parser;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

use app::{App, Event};
use config::Config;

#[derive(Parser, Debug)]
#[command(
    name = "textfold",
    version,
    about = "A terminal text editor for the keyboard and the mouse",
    long_about = "Open files and edit them. Ctrl-S saves, Ctrl-Q leaves, Ctrl-P finds a \
                  file, Alt-X finds a command, and F1 lists the rest.\n\n\
                  Syntax colouring and language servers are built in: open a Rust file \
                  in a Cargo project and rust-analyzer starts on its own.\n\n\
                  A file can be named as PATH:LINE or PATH:LINE:COLUMN, which is what \
                  compilers and grep print."
)]
struct Args {
    /// Files to open, as PATH, PATH:LINE, or PATH:LINE:COLUMN
    files: Vec<String>,

    /// Colours to use, for this run only
    #[arg(long, value_name = "NAME")]
    theme: Option<String>,

    /// Go to this line in the first file
    #[arg(short = 'l', long, value_name = "N")]
    line: Option<usize>,

    /// Start with the mouse left to the terminal
    #[arg(long)]
    no_mouse: bool,

    /// List the themes there are and stop
    #[arg(long)]
    list_themes: bool,

    /// List the languages textfold knows and stop
    #[arg(long)]
    list_languages: bool,

    /// Say where language servers' complaints are written and stop
    #[arg(long)]
    log_path: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    if args.list_themes {
        let themes = theme::Themes::load();
        for named in &themes.entries {
            match &named.about {
                Some(about) => println!("{:<18} {about}", named.name),
                None => println!("{}", named.name),
            }
        }
        for problem in &themes.problems {
            eprintln!("{problem}");
        }
        return Ok(());
    }
    if args.list_languages {
        lang::init();
        for (id, name) in lang::names() {
            let language = lang::get(id);
            let mut about = Vec::new();
            if language.has_grammar() {
                about.push("coloured".to_string());
            }
            for server in &language.servers {
                about.push(server.command.clone());
            }
            println!("{name:<14} {}", about.join(", "));
        }
        for problem in &lang::all().problems {
            eprintln!("{problem}");
        }
        return Ok(());
    }
    if args.log_path {
        match lsp::log_path() {
            Some(path) => println!("{}", path.display()),
            None => println!("nowhere to write one"),
        }
        return Ok(());
    }

    let mut config = Config::load();
    if let Some(theme) = args.theme {
        config.theme = Some(theme);
    }
    if args.no_mouse {
        config.mouse = Some(false);
    }
    let wants_mouse = config.mouse();
    let wants_keys = config.enhanced_keys();

    let (tx, rx) = mpsc::channel::<Event>();
    let mut app = App::new(config, tx.clone());

    let mut terminal = start(wants_mouse, wants_keys)?;
    // Whatever happens from here — a panic in a widget, a bug in a grammar —
    // the terminal goes back to how it was found. An editor that leaves a
    // terminal in raw mode with the alternate screen up is a bug people have
    // to reboot a shell to escape.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        stop(wants_mouse, wants_keys);
        previous(info);
    }));

    // The screen is up before anything is read from disk, so that opening a
    // very large file shows an editor thinking about it rather than a
    // terminal that has not come back.
    terminal.draw(|frame| ui::draw(frame, &mut app))?;

    // Files named on the command line, in order, so that the last one named is
    // the one you end up looking at.
    let mut go_to: Option<(usize, usize)> = None;
    for name in &args.files {
        let (path, place) = split_place(name);
        app.open_path(&path);
        go_to = place;
    }
    if let Some(line) = args.line {
        go_to = Some((line.saturating_sub(1), 0));
    }
    if let Some((line, column)) = go_to {
        app.jump_to(line, column);
    }

    spawn_input(tx);
    let result = run(&mut terminal, &mut app, &rx, wants_mouse);

    app.lsp.shutdown_all();
    stop(wants_mouse, wants_keys);
    result
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    rx: &mpsc::Receiver<Event>,
    mut mouse: bool,
) -> Result<()> {
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;
        if app.quit {
            return Ok(());
        }

        match rx.recv_timeout(app.idle()) {
            Ok(event) => {
                app.handle(event);
                // Take whatever else is waiting before drawing again: a
                // language server can send a hundred messages in a burst, and
                // redrawing between each of them is a hundred wasted frames.
                // Capped, so that a server talking without pause cannot stop
                // the screen from ever being drawn.
                for _ in 0..64 {
                    match rx.try_recv() {
                        Ok(event) if !app.quit => app.handle(event),
                        _ => break,
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            // The input thread has gone, which means the terminal has.
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
        app.tick();

        // The mouse can be handed back to the terminal from inside the editor,
        // and taken again.
        if app.mouse_on != mouse {
            mouse = app.mouse_on;
            let mut out = io::stdout();
            if mouse {
                execute!(out, EnableMouseCapture).ok();
            } else {
                execute!(out, DisableMouseCapture).ok();
            }
        }
    }
}

/// Read the terminal on a thread of its own, so that the main loop can wait on
/// one channel for both the keyboard and the language servers.
fn spawn_input(tx: Sender<Event>) {
    std::thread::Builder::new()
        .name("input".into())
        .spawn(move || {
            loop {
                match event::read() {
                    Ok(event) => {
                        if tx.send(Event::Term(event)).is_err() {
                            return;
                        }
                    }
                    Err(_) => return,
                }
            }
        })
        .ok();
}

fn start(mouse: bool, keys: bool) -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().context("putting the terminal into raw mode")?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, EnableBracketedPaste)?;
    if mouse {
        execute!(out, EnableMouseCapture)?;
    }
    // Without this, a terminal cannot tell Ctrl-Shift-P from Ctrl-P, or a key
    // being let go from one being pressed. With it, the extra bindings work.
    //
    // Asked for rather than checked for. Checking means sending the terminal a
    // question and waiting up to two seconds for an answer it may never give —
    // which is two seconds of nothing on the screen on every terminal that
    // does not implement the protocol, to find out something we are about to
    // ask for anyway. A terminal that does not know these sequences ignores
    // them, which is exactly what the check would have told us.
    if keys {
        execute!(
            out,
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
            )
        )
        .ok();
    }
    out.flush()?;
    Ok(Terminal::new(CrosstermBackend::new(io::stdout()))?)
}

fn stop(mouse: bool, keys: bool) {
    let mut out = io::stdout();
    if keys {
        execute!(out, PopKeyboardEnhancementFlags).ok();
    }
    if mouse {
        execute!(out, DisableMouseCapture).ok();
    }
    execute!(out, DisableBracketedPaste, LeaveAlternateScreen).ok();
    disable_raw_mode().ok();
    out.flush().ok();
}

/// `src/main.rs:42:8` as a path and a place.
///
/// What every compiler and every `grep -n` prints, so pasting one straight
/// onto the command line lands where the error is. A file that really is
/// called `notes:1` still opens, because the path is checked first.
fn split_place(name: &str) -> (PathBuf, Option<(usize, usize)>) {
    let whole = PathBuf::from(name);
    if whole.exists() {
        return (whole, None);
    }
    let mut parts = name.rsplitn(3, ':');
    let last = parts.next().unwrap_or_default();
    let middle = parts.next();
    let rest = parts.next();

    match (rest, middle, last.parse::<usize>()) {
        // `path:line:column`
        (Some(path), Some(line), Ok(column)) => match line.parse::<usize>() {
            Ok(line) => (
                PathBuf::from(path),
                Some((line.saturating_sub(1), column.saturating_sub(1))),
            ),
            Err(_) => (whole, None),
        },
        // `path:line`
        (None, Some(path), Ok(line)) => (
            PathBuf::from(path),
            Some((line.saturating_sub(1), 0)),
        ),
        _ => (whole, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_compiler_error_can_be_pasted_straight_in() {
        assert_eq!(
            split_place("src/main.rs:42:8"),
            (PathBuf::from("src/main.rs"), Some((41, 7)))
        );
        assert_eq!(
            split_place("src/main.rs:42"),
            (PathBuf::from("src/main.rs"), Some((41, 0)))
        );
        assert_eq!(split_place("src/main.rs"), (PathBuf::from("src/main.rs"), None));
        // Not a number, so not a line.
        assert_eq!(
            split_place("weird:name"),
            (PathBuf::from("weird:name"), None)
        );
    }
}
