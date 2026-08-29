//! textfold — a terminal text editor that works the way everything else does.
//!
//! Keyboard and mouse both, language servers and syntax colouring out of the
//! box, and a set of keys nobody has to be taught. One binary, no plugins to
//! install before it is usable, and a settings file you only write the parts
//! of that you disagree with.

mod app;
mod cmd;
mod config;
mod diff;
mod doc;
mod edit;
mod git;
mod host;
mod jdk;
mod keys;
mod lang;
mod lsp;
mod menu;
mod pack;
mod picker;
mod plugin;
mod repo;
mod rpc;
mod session;
mod syntax;
mod term;
mod text;
mod tool;
mod theme;
mod ui;
mod venv;
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

    /// Start empty, rather than opening what was open here last time
    #[arg(long)]
    no_session: bool,

    /// List the themes there are and stop
    #[arg(long)]
    list_themes: bool,

    /// List the languages textfold knows and stop
    #[arg(long)]
    list_languages: bool,

    /// List the plugins there are, and which are on, and stop
    #[arg(long)]
    list_plugins: bool,

    /// List what could be installed, and where from, and stop
    #[arg(long)]
    list_packages: bool,

    /// Install a plugin, by id or by the path of a directory with a
    /// plugin.json in it, and stop
    #[arg(long, value_name = "ID-OR-PATH")]
    install: Option<String>,

    /// Remove a plugin, and undo what installing it did, and stop
    #[arg(long, value_name = "ID")]
    uninstall: Option<String>,

    /// Ask the package repositories what they have now, and stop
    #[arg(long)]
    refresh: bool,

    /// Install a newer version of everything that has one — or of the one
    /// plugin named — and stop
    #[arg(long, value_name = "ID", num_args = 0..=1, default_missing_value = "")]
    update: Option<String>,

    /// Say where language servers' complaints are written and stop
    #[arg(long)]
    log_path: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let mut config = Config::load();
    // Textfold's own programs go on the PATH before anything is started, so
    // that language servers, tools, plugins' own programs and install steps
    // all find them without any of those having to know they exist. Last, so
    // that what you have installed yourself still wins.
    pack::put_tools_on_path();
    // What is switched off decides what the languages are, so it is read
    // before anything asks after one — including the `--list-…` answers,
    // which should say what this machine would actually do.
    plugin::init(&mut config.plugins);
    // Before anything can open a Java file and ask which JDK it is for.
    jdk::configure(config.java_home.as_deref());

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
    if args.list_plugins {
        for plugin in plugin::all() {
            // Three states, not two. A plugin that is on and has nothing to
            // run is the case people spend an afternoon on, so it says so.
            let state = match (plugin::is_on(&plugin.id), plugin.is_ready()) {
                (false, _) => "off",
                (true, true) => "on ",
                (true, false) => "get",
            };
            println!("{state}  {:<22} {}", plugin.id, plugin.detail());
            if !plugin.is_ready() {
                // What to do about it, which is not the same sentence for a
                // plugin that knows how to fetch what it needs and one that
                // only knows where it lives. Saying `--install` for the second
                // kind sends somebody to a command that will do nothing.
                let what = match (plugin.can_install(), &plugin.see) {
                    (true, _) => format!("textfold --install {}", plugin.id),
                    (false, Some(see)) => format!("see {see}"),
                    (false, None) => "install it yourself and textfold will find it".into(),
                };
                println!(
                    "      {:<20} needs {} — {what}",
                    "",
                    plugin.missing().join(", ")
                );
            }
            for server in &plugin.servers {
                let state = if plugin::is_on(&server.id) { "on " } else { "off" };
                println!("{state}    {:<20} runs {}", server.id, server.command);
            }
            for tool in &plugin.tools {
                let state = if plugin::is_on(&tool.id) { "on " } else { "off" };
                println!("{state}    {:<20} runs {}", tool.id, tool.command);
            }
            // A plugin that brings a program of its own says so here as well
            // as in the list inside the editor: it is a different thing to
            // switch on from one that only adds a language.
            if let Some(host) = &plugin.host {
                println!("      {:<20} its own program: {}", "", host.command);
                for command in &plugin.commands {
                    let state = if plugin::is_on(&command.id) { "on " } else { "off" };
                    println!("{state}    {:<20} {}", command.id, command.about);
                }
            }
        }
        for problem in plugin::problems() {
            eprintln!("{problem}");
        }
        return Ok(());
    }
    if args.refresh {
        return do_refresh(&config);
    }
    if let Some(what) = &args.update {
        return do_update(&config, what.trim());
    }
    if args.list_packages {
        let here = pack::receipts();
        for package in pack::available(pack::Sources::of(&config)) {
            println!(
                "{:<7} {:<24} {}",
                package.tag(),
                package.id,
                package.detail()
            );
        }
        if !here.is_empty() {
            println!("\ninstalled by textfold:");
            for (id, from) in here {
                println!("        {id:<24} from {from}");
            }
        }
        return Ok(());
    }
    if let Some(what) = &args.install {
        return do_package(
            pack::find(what, pack::Sources::of(&config)).and_then(|p| pack::install(&p)),
        );
    }
    if let Some(what) = &args.uninstall {
        return do_package(pack::uninstall(&what.trim().to_lowercase()));
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

    if let Some(theme) = args.theme {
        config.theme = Some(theme);
    }
    if args.no_mouse {
        config.mouse = Some(false);
    }
    let wants_mouse = config.mouse();
    let wants_keys = config.enhanced_keys();
    // Worked out before anything is drawn, because the first frame is already
    // a frame that can be wrecked by asking a terminal for a sequence it does
    // not have.
    term::set_underline_colour(config.underline_colour());

    let (tx, rx) = mpsc::channel::<Event>();
    let mut app = App::new(config, tx.clone());
    // Ask the package repositories what they have, on a thread. Nothing waits
    // for it and nothing is installed by it: what it changes is whether the
    // plugins list has an `update` beside anything.
    app.check_for_updates();

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

    // What was open here last time, but only where nothing was named on the
    // command line. `textfold notes.md` means open that file, not open that
    // file and the eleven you had open on Friday.
    if args.files.is_empty() && !args.no_session && app.config.restore_session() {
        let count = app.restore_session(false);
        if count > 0 {
            app.say(format!(
                "{count} {} from last time — --no-session starts empty",
                if count == 1 { "file" } else { "files" }
            ));
        }
    }

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

    // What is open, written down on the way out — this is the one that
    // matters, since it is the only one with the cursors where you left them.
    app.remember_session(true);
    app.lsp.shutdown_all();
    app.hosts.shutdown_all();
    stop(wants_mouse, wants_keys);
    result
}

/// Ask every repository what it has now, and say so.
fn do_refresh(config: &Config) -> Result<()> {
    let repositories = repo::repositories(config.package_repositories());
    let mut bad = false;
    for repository in &repositories {
        match repo::refresh(repository) {
            Ok(count) => println!("{}: {count} plugins", repository.name),
            Err(why) => {
                eprintln!("{}: {why}", repository.name);
                bad = true;
            }
        }
    }
    // One repository being unreachable is worth an exit code, since this is
    // the command a script would run — but the others were still refreshed.
    match bad {
        true => std::process::exit(1),
        false => Ok(()),
    }
}

/// Install a newer version of everything with one, or of the one named.
///
/// The index is asked first. `--update` that acted on what was cached last
/// week would report nothing to do on the day a release came out, which is the
/// one day anybody runs it.
fn do_update(config: &Config, what: &str) -> Result<()> {
    for why in pack::refresh(config.package_repositories()) {
        eprintln!("{why}");
    }
    let sources = pack::Sources::of(config);
    let mut updates = pack::updates(sources);
    if !what.is_empty() {
        let wanted = what.to_lowercase();
        updates.retain(|p| p.id == wanted);
        if updates.is_empty() {
            println!("{what} is already at the newest version there is");
            return Ok(());
        }
    }
    if updates.is_empty() {
        println!("everything is at the newest version there is");
        return Ok(());
    }
    for package in updates {
        println!("
{}", package.detail());
        do_package(pack::install(&package))?;
    }
    Ok(())
}

/// Carry out an install or an uninstall from the command line.
///
/// The same plan the editor runs, run straight through with what it says
/// printed rather than sent down a channel — which is the whole reason a plan
/// is a thing you can hold rather than something an install does to itself.
///
/// What it is about to do is printed before it does any of it. A plugin's
/// installer runs programs on your machine, and naming them first is the least
/// an editor can do.
fn do_package(plan: std::result::Result<pack::Plan, String>) -> Result<()> {
    let plan = match plan {
        Ok(plan) => plan,
        Err(why) => {
            eprintln!("{why}");
            std::process::exit(1);
        }
    };
    if plan.is_empty() {
        println!("{} has nothing to do — it is already here", plan.name);
        return Ok(());
    }
    println!("{}:", plan.name);
    for line in plan.lines() {
        println!("  {line}");
    }
    if !plan.removing {
        match (plan.touches_system(), pack::tools_dir()) {
            (true, _) => {
                println!("\nSome of this installs system-wide — the lines that say so above.")
            }
            (false, Some(tools)) => println!("\nInto {}", tools.display()),
            (false, None) => {}
        }
    }
    println!();

    let ok = plan.run(&mut |note| match note {
        pack::Note::Doing { at, of, about } if of > 0 => println!("[{at}/{of}] {about}"),
        pack::Note::Doing { about, .. } => println!("{about}"),
        pack::Note::Skipped { about, why } => println!("  {about} — skipped, {why}"),
        pack::Note::Did { about, ok, output } => {
            print!("{output}");
            if !ok {
                eprintln!("{about} failed");
            }
        }
        pack::Note::Done { ok, why } => match ok {
            true => println!("\n{why}"),
            false => eprintln!("\n{why}"),
        },
    });
    match ok {
        true => Ok(()),
        false => std::process::exit(1),
    }
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
