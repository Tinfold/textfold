//! The settings, and the colours.
//!
//! Everything here writes what you decided back to the file — and only what
//! you decided, so a settings file says the handful of things you changed
//! rather than repeating forty defaults at you.

use super::*;

impl App {
    pub(super) fn step_theme(&mut self, by: isize) {
        let named = self.themes.cycle(self.config.theme_name(), by);
        self.theme = named.theme;
        self.config.theme = Some(named.name.clone());
        self.remember_settings();
        self.say(match named.about {
            Some(about) => format!("{} — {about}", named.name),
            None => named.name,
        });
    }

    pub(super) fn set_theme(&mut self, name: &str) {
        if let Some(theme) = self.themes.by_name(name) {
            self.theme = theme;
            self.config.theme = Some(name.to_string());
        }
    }

    pub(super) fn toggle_setting(&mut self, which: &str) {
        let said = match which {
            "line_numbers" => {
                let off = self.config.line_numbers() == LineNumbers::Off;
                self.config.line_numbers = Some(if off { "absolute" } else { "off" }.into());
                if off {
                    "line numbers on"
                } else {
                    "line numbers off"
                }
            }
            "relative_numbers" => {
                let relative = matches!(
                    self.config.line_numbers(),
                    LineNumbers::Relative | LineNumbers::Both
                );
                self.config.line_numbers = Some(if relative { "absolute" } else { "both" }.into());
                if relative {
                    "line numbers count from the top"
                } else {
                    "line numbers count from the cursor"
                }
            }
            "show_whitespace" => {
                let on = !self.config.show_whitespace();
                self.config.show_whitespace = Some(on);
                if on {
                    "showing spaces and tabs"
                } else {
                    "not showing spaces and tabs"
                }
            }
            "mouse" => {
                let on = !self.config.mouse();
                self.config.mouse = Some(on);
                self.mouse_on = on;
                if on {
                    "the mouse is textfold's"
                } else {
                    "the mouse is the terminal's — select and copy as usual"
                }
            }
            "wrap" => {
                let on = !self.config.wrap();
                self.config.wrap = Some(on);
                for pane in &mut self.panes {
                    pane.wrap = on;
                    pane.left = 0;
                }
                if on {
                    "long lines fold"
                } else {
                    "long lines run off the side"
                }
            }
            "auto_completion" => {
                let on = !self.config.auto_completion();
                self.config.auto_completion = Some(on);
                if on {
                    "completions appear as you type"
                } else {
                    "completions only when asked for"
                }
            }
            "auto_pairs" => {
                let on = !self.config.auto_pairs();
                self.config.auto_pairs = Some(on);
                if on {
                    "brackets close themselves"
                } else {
                    "brackets are yours to close"
                }
            }
            "inlay_hints" => {
                let on = !self.config.inlay_hints();
                self.config.inlay_hints = Some(on);
                if !on {
                    // Off means gone now, not gone the next time somebody
                    // asks a server something.
                    for doc in &mut self.docs {
                        doc.said.inlays.clear();
                    }
                }
                self.ask_the_servers_about_this_file();
                if on {
                    "showing the types the code does not say"
                } else {
                    "showing only what the code says"
                }
            }
            "code_lenses" => {
                let on = !self.config.code_lenses();
                self.config.code_lenses = Some(on);
                if !on {
                    for doc in &mut self.docs {
                        doc.said.lenses.clear();
                    }
                }
                self.ask_the_servers_about_this_file();
                if on {
                    "showing what the servers have to say about each line"
                } else {
                    "not showing the servers' notes"
                }
            }
            "format_on_save" => {
                let on = !self.config.format_on_save();
                self.config.format_on_save = Some(on);
                if on {
                    "formatting on save"
                } else {
                    "not formatting on save"
                }
            }
            "spaces" => {
                let on = !self.config.spaces();
                self.config.spaces = Some(on);
                if on {
                    "new files use spaces"
                } else {
                    "new files use tabs"
                }
            }
            "trim_trailing_whitespace" => {
                let on = !self.config.trim_trailing_whitespace();
                self.config.trim_trailing_whitespace = Some(on);
                if on {
                    "trailing spaces go on save"
                } else {
                    "trailing spaces stay"
                }
            }
            "code_actions_on_save" => {
                let on = self.config.code_actions_on_save().is_empty();
                self.config.code_actions_on_save = Some(match on {
                    true => vec![
                        SOURCE_FIX_ALL.to_string(),
                        SOURCE_ORGANIZE_IMPORTS.to_string(),
                    ],
                    false => Vec::new(),
                });
                if on {
                    "the servers fix what they can on save"
                } else {
                    "the servers leave the file alone on save"
                }
            }
            "restore_session" => {
                let on = !self.config.restore_session();
                self.config.restore_session = Some(on);
                if on {
                    "the tabs come back next time"
                } else {
                    "textfold starts empty"
                }
            }
            "underline_colour" => {
                let on = !self.config.underline_colour();
                self.config.underline_colour = Some(if on { "on" } else { "off" }.into());
                crate::term::set_underline_colour(on);
                if on {
                    "problems are underlined in colour — if the file has gone \
                     strange, this terminal does not have it"
                } else {
                    "problems are underlined plainly"
                }
            }
            _ => return,
        };
        self.remember_settings();
        self.say(said);
    }

    pub(super) fn remember_settings(&mut self) {
        if let Err(e) = self.config.save() {
            self.say_bad(format!("could not write the settings: {e}"));
        }
    }
}
