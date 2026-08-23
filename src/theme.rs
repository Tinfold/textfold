//! Colours, by what they mean rather than what they are.
//!
//! Two families of role live here. The first twelve are the interface: borders,
//! titles, the line that says what file you are in. They are the same twelve
//! sshman uses and are named the same, so a theme file written for one editor
//! drops into the other unchanged.
//!
//! The second family is the code itself — keywords, strings, comments. A theme
//! may spell those out, but it does not have to: every one of them has a
//! meaning that one of the twelve already carries. Strings are the colour of
//! things that worked, comments the colour of things deliberately in the
//! background, keywords the colour reserved for the notable. So an sshman
//! theme is a whole textfold theme, and a textfold theme that wants to be
//! precise about code says only the parts it disagrees with.
//!
//! The tables are not in this file. Each is a small JSON file: the ones
//! textfold ships live in `themes/` and are built into the binary, and any
//! file dropped in `~/.config/textfold/themes/` is loaded beside them. A file
//! taking a name textfold already uses replaces it, which is how you rewrite
//! one of ours without forking anything.

use std::path::PathBuf;

use ratatui::style::Color;
use serde::Deserialize;

/// The twelve interface roles, the four that belong to a text pane, and the
/// colours code is drawn in. Copied on every span, so it stays `Copy`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Theme {
    /// Focused borders, titles, the marks that say "you are here".
    pub accent: Color,
    /// Unfocused borders, hints, anything deliberately in the background.
    pub dim: Color,
    /// Ordinary text you are meant to read.
    pub text: Color,
    /// Text that is there when you look for it: counts, labels, positions.
    pub muted: Color,
    /// It worked.
    pub good: Color,
    /// Worth a second look.
    pub warn: Color,
    /// It did not work, or it is about to do something irreversible.
    pub bad: Color,
    /// Directories in a listing.
    pub dir: Color,
    /// Symlinks in a listing.
    pub link: Color,
    /// Files you could run.
    pub exec: Color,
    /// Badges that are telling you something rather than warning you.
    pub info: Color,
    /// Text drawn *on* a coloured chip, so it contrasts with the colours
    /// above rather than with the terminal.
    pub on_accent: Color,
    /// What to paint behind everything. [`Color::Reset`] means the terminal's
    /// own, which is what a theme naming no background gets.
    pub bg: Color,

    /// Behind selected text.
    pub selection: Color,
    /// Behind the line the cursor is on. Equal to [`Theme::bg`] means no
    /// highlight at all, which is what a terminal-coloured theme gets, since
    /// there is no background to lighten.
    pub cursorline: Color,
    /// Line numbers, and the column they sit in.
    pub gutter: Color,
    /// The line number the cursor is on.
    pub gutter_active: Color,
    /// The colours code is drawn in.
    pub syntax: Syntax,
}

/// What each kind of code is coloured. The names are tree-sitter's own capture
/// names, cut down to the ones a person can hold in their head; anything more
/// specific in a grammar's queries falls back along the dots, so
/// `@function.method.builtin` lands on `function` without anyone saying so.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Syntax {
    pub keyword: Color,
    pub function: Color,
    pub type_: Color,
    pub constructor: Color,
    pub string: Color,
    pub escape: Color,
    pub number: Color,
    pub boolean: Color,
    pub comment: Color,
    pub constant: Color,
    pub variable: Color,
    pub parameter: Color,
    pub property: Color,
    pub operator: Color,
    pub punctuation: Color,
    pub attribute: Color,
    pub namespace: Color,
    pub tag: Color,
    pub label: Color,
    pub error: Color,
}

/// One kind of code, as the highlighter hands it to the drawing.
///
/// The order is the order [`CAPTURES`] is in, and the numbers are handed to
/// tree-sitter as capture indices, so the two have to stay together.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Keyword,
    Function,
    Type,
    Constructor,
    String,
    Escape,
    Number,
    Boolean,
    Comment,
    Constant,
    Variable,
    Parameter,
    Property,
    Operator,
    Punctuation,
    Attribute,
    Namespace,
    Tag,
    Label,
    Error,
}

/// Every capture name textfold knows, in [`Role`] order. A grammar's query
/// names a capture like `@keyword.control.repeat`; the longest of these that
/// is a prefix of it wins, so a grammar can be as specific as it likes and
/// still be coloured.
pub const CAPTURES: &[(&str, Role)] = &[
    ("keyword", Role::Keyword),
    ("function", Role::Function),
    ("method", Role::Function),
    ("type", Role::Type),
    ("constructor", Role::Constructor),
    ("string", Role::String),
    ("character", Role::String),
    ("escape", Role::Escape),
    ("string.escape", Role::Escape),
    ("string.special", Role::Escape),
    ("number", Role::Number),
    ("float", Role::Number),
    ("boolean", Role::Boolean),
    ("comment", Role::Comment),
    ("constant", Role::Constant),
    ("constant.builtin", Role::Boolean),
    ("variable", Role::Variable),
    ("variable.parameter", Role::Parameter),
    ("parameter", Role::Parameter),
    ("variable.member", Role::Property),
    ("property", Role::Property),
    ("field", Role::Property),
    ("operator", Role::Operator),
    ("punctuation", Role::Punctuation),
    ("attribute", Role::Attribute),
    ("annotation", Role::Attribute),
    ("namespace", Role::Namespace),
    ("module", Role::Namespace),
    ("tag", Role::Tag),
    ("label", Role::Label),
    ("error", Role::Error),
];

impl Theme {
    /// The colour one kind of code is drawn in.
    pub fn role(&self, role: Role) -> Color {
        let s = &self.syntax;
        match role {
            Role::Keyword => s.keyword,
            Role::Function => s.function,
            Role::Type => s.type_,
            Role::Constructor => s.constructor,
            Role::String => s.string,
            Role::Escape => s.escape,
            Role::Number => s.number,
            Role::Boolean => s.boolean,
            Role::Comment => s.comment,
            Role::Constant => s.constant,
            Role::Variable => s.variable,
            Role::Parameter => s.parameter,
            Role::Property => s.property,
            Role::Operator => s.operator,
            Role::Punctuation => s.punctuation,
            Role::Attribute => s.attribute,
            Role::Namespace => s.namespace,
            Role::Tag => s.tag,
            Role::Label => s.label,
            Role::Error => s.error,
        }
    }

    /// Barely there: a scroll bar's track, the rule between two panes.
    /// Halfway from the background towards the dim colour, or the dimmest
    /// thing a terminal-coloured theme has.
    pub fn faint(&self) -> Color {
        blend(self.bg, self.dim, 0.45).unwrap_or(Color::DarkGray)
    }

    /// The colours code gets when a theme says nothing about code: each kind
    /// drawn in whichever of the twelve already means that.
    ///
    /// This is the whole reason an sshman theme is also a textfold theme, and
    /// it is not a fudge — a string literal really is a thing that worked, and
    /// a comment really is text deliberately in the background.
    fn derived_syntax(&self) -> Syntax {
        Syntax {
            keyword: self.link,
            function: self.accent,
            type_: self.info,
            constructor: self.info,
            string: self.good,
            escape: self.link,
            number: self.warn,
            boolean: self.warn,
            comment: self.dim,
            constant: self.warn,
            variable: self.text,
            parameter: self.muted,
            property: self.dir,
            operator: self.muted,
            punctuation: self.muted,
            attribute: self.warn,
            namespace: self.info,
            tag: self.bad,
            label: self.link,
            error: self.bad,
        }
    }

    /// What to paint behind selected text when a theme has not said. A quarter
    /// of the way from the background towards the accent keeps whatever the
    /// text was coloured legible, which a flat blue does not.
    fn derived_selection(&self) -> Color {
        blend(self.bg, self.accent, 0.30).unwrap_or(Color::DarkGray)
    }

    /// What to paint behind the cursor's line. A tenth of the way towards the
    /// text: enough to find, not enough to read through.
    ///
    /// A theme with no background of its own gets none, because there is
    /// nothing to lighten — lightening the terminal's own would mean guessing
    /// what it is.
    fn derived_cursorline(&self) -> Color {
        blend(self.bg, self.text, 0.07).unwrap_or(self.bg)
    }
}

/// Mix two colours, `amount` of the way from `from` to `to`.
///
/// Only meaningful for colours that are actually numbers. A theme naming its
/// colours by terminal palette slot has not told us what they look like, so
/// there is nothing to mix and this says so rather than guessing.
fn blend(from: Color, to: Color, amount: f32) -> Option<Color> {
    let (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) = (from, to) else {
        return None;
    };
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * amount).round() as u8;
    Some(Color::Rgb(mix(r1, r2), mix(g1, g2), mix(b1, b2)))
}

/// The theme to fall back on. It is the one colours file also written here,
/// because textfold has to be able to draw the screen that tells you your
/// themes could not be read. `themes/terminal.json` says the same thing, and a
/// test below holds the two together.
pub const FALLBACK: Theme = Theme {
    accent: Color::Cyan,
    dim: Color::DarkGray,
    text: Color::White,
    muted: Color::Gray,
    good: Color::Green,
    warn: Color::Yellow,
    bad: Color::Red,
    dir: Color::Blue,
    link: Color::Magenta,
    exec: Color::Green,
    info: Color::Blue,
    on_accent: Color::Black,
    bg: Color::Reset,
    selection: Color::DarkGray,
    cursorline: Color::Reset,
    gutter: Color::DarkGray,
    gutter_active: Color::White,
    syntax: Syntax {
        keyword: Color::Magenta,
        function: Color::Cyan,
        type_: Color::Blue,
        constructor: Color::Blue,
        string: Color::Green,
        escape: Color::Magenta,
        number: Color::Yellow,
        boolean: Color::Yellow,
        comment: Color::DarkGray,
        constant: Color::Yellow,
        variable: Color::White,
        parameter: Color::Gray,
        property: Color::Blue,
        operator: Color::Gray,
        punctuation: Color::Gray,
        attribute: Color::Yellow,
        namespace: Color::Blue,
        tag: Color::Red,
        label: Color::Magenta,
        error: Color::Red,
    },
};

/// What [`FALLBACK`] is called, and so what a config file with nothing in it
/// is asking for.
pub const DEFAULT: &str = "terminal";

impl Default for Theme {
    fn default() -> Self {
        FALLBACK
    }
}

/// A theme and the name you ask for it by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Named {
    pub name: String,
    pub theme: Theme,
    /// The line the file gives about itself, if it gives one.
    pub about: Option<String>,
}

/// The themes there are: the ones built in, then anything found on disk.
#[derive(Clone, Debug, Default)]
pub struct Themes {
    pub entries: Vec<Named>,
    /// Files that could not be used, in the words to show someone wondering
    /// where their theme went. A theme file with a typo in it is worth a
    /// complaint; silently having one fewer theme is not.
    pub problems: Vec<String>,
}

/// The files textfold ships. Built into the binary so a copied executable is
/// still a whole program, and readable in `themes/` so a new one is a matter
/// of copying the nearest.
const BUILT_IN: &[(&str, &str)] = &[
    ("terminal.json", include_str!("../themes/terminal.json")),
    ("catppuccin.json", include_str!("../themes/catppuccin.json")),
    ("dracula.json", include_str!("../themes/dracula.json")),
    ("nord.json", include_str!("../themes/nord.json")),
    ("tokyonight.json", include_str!("../themes/tokyonight.json")),
    ("gruvbox.json", include_str!("../themes/gruvbox.json")),
    ("everforest.json", include_str!("../themes/everforest.json")),
    ("solarized.json", include_str!("../themes/solarized.json")),
    ("onedark.json", include_str!("../themes/onedark.json")),
    ("monokai.json", include_str!("../themes/monokai.json")),
    ("kanagawa.json", include_str!("../themes/kanagawa.json")),
    ("rosepine.json", include_str!("../themes/rosepine.json")),
    ("mariana.json", include_str!("../themes/mariana.json")),
    ("afterglow.json", include_str!("../themes/afterglow.json")),
    ("darcula.json", include_str!("../themes/darcula.json")),
    ("ayu.json", include_str!("../themes/ayu.json")),
    // The light ones last, since most terminals have a dark background and a
    // list you step through should start where you probably want to be.
    (
        "solarized-light.json",
        include_str!("../themes/solarized-light.json"),
    ),
    ("latte.json", include_str!("../themes/latte.json")),
];

impl Themes {
    /// Everything textfold ships, in the order listed above.
    pub fn built_in() -> Self {
        let mut themes = Self::default();
        for (file, text) in BUILT_IN {
            themes.add(text, file);
        }
        themes
    }

    /// The built-in themes, plus every `.json` in the themes directory. A file
    /// naming a theme we already have replaces it where it stands, so stepping
    /// through them keeps a stable order.
    pub fn load() -> Self {
        let mut themes = Self::built_in();
        if let Some(dir) = themes_dir() {
            themes.load_from(&dir);
        }
        themes
    }

    fn load_from(&mut self, dir: &std::path::Path) {
        let Ok(read) = std::fs::read_dir(dir) else {
            // No themes directory is the ordinary case, not a problem.
            return;
        };
        let mut files: Vec<PathBuf> = read
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("json")))
            .collect();
        // A fixed order, so two files claiming one name settle the same way
        // every time rather than by whatever the directory happens to say.
        files.sort();
        for path in files {
            let shown = match path.file_name() {
                Some(name) => name.to_string_lossy().into_owned(),
                None => path.display().to_string(),
            };
            match std::fs::read_to_string(&path) {
                Ok(text) => self.add(&text, &shown),
                Err(e) => self.problems.push(format!("{shown}: {e}")),
            }
        }
    }

    /// Take one file's worth of theme, or write down why not.
    fn add(&mut self, text: &str, from: &str) {
        let file: FileTheme = match serde_json::from_str::<FileTheme>(text) {
            Ok(file) => file,
            Err(e) => {
                // Serde's line and column are about the file we are quoting
                // back at someone who has it open, and crowd out the part
                // saying what is actually wrong with it.
                let said = e.to_string();
                let said = said.split(" at line ").next().unwrap_or(&said);
                self.problems.push(format!("{from}: {said}"));
                return;
            }
        };
        let name = file.name.trim().to_lowercase();
        if name.is_empty() {
            self.problems.push(format!("{from}: a theme needs a name"));
            return;
        }
        // Anything left out comes from the theme it is based on, so a file
        // that only wants to change the accent only has to say the accent.
        let base = match &file.base {
            Some(base) => match self.by_name(base) {
                Some(theme) => theme,
                None => {
                    self.problems
                        .push(format!("{from}: there is no theme called {base:?}"));
                    return;
                }
            },
            None => FALLBACK,
        };
        let named = Named {
            theme: file.resolve(base),
            about: file.about,
            name,
        };
        match self.entries.iter_mut().find(|e| e.name == named.name) {
            Some(existing) => *existing = named,
            None => self.entries.push(named),
        }
    }

    /// The theme of that name, or `None` for one we do not have.
    pub fn by_name(&self, name: &str) -> Option<Theme> {
        let wanted = name.trim().to_lowercase();
        self.entries
            .iter()
            .find(|e| e.name == wanted)
            .map(|e| e.theme)
    }

    /// The next one along, for a picker that cycles rather than asks you to
    /// type. `step` of -1 goes back. A name we do not have — a theme whose
    /// file has since been deleted — starts from the beginning.
    pub fn cycle(&self, name: &str, step: isize) -> Named {
        if self.entries.is_empty() {
            return Named {
                name: DEFAULT.into(),
                theme: FALLBACK,
                about: None,
            };
        }
        let wanted = name.trim().to_lowercase();
        let at = self
            .entries
            .iter()
            .position(|e| e.name == wanted)
            .map(|at| at as isize + step)
            .unwrap_or(0);
        let len = self.entries.len() as isize;
        self.entries[at.rem_euclid(len) as usize].clone()
    }
}

/// Where a theme of your own goes. Beside the settings, so there is one
/// directory to look in and one to back up.
pub fn themes_dir() -> Option<PathBuf> {
    Some(crate::config::config_dir()?.join("themes"))
}

/// A theme as its file writes it. Every colour is optional: what a file leaves
/// out comes from the theme it is based on, and what no theme in the chain
/// mentions is worked out from the twelve.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileTheme {
    name: String,
    /// A line about where these colours come from. Nothing reads it but a
    /// person, which is the point — JSON has nowhere else to put a comment.
    #[serde(default)]
    about: Option<String>,
    /// The theme to take anything left out from.
    #[serde(default)]
    base: Option<String>,

    #[serde(default)]
    accent: Option<Colour>,
    #[serde(default)]
    dim: Option<Colour>,
    #[serde(default)]
    text: Option<Colour>,
    #[serde(default)]
    muted: Option<Colour>,
    #[serde(default)]
    good: Option<Colour>,
    #[serde(default)]
    warn: Option<Colour>,
    #[serde(default)]
    bad: Option<Colour>,
    #[serde(default)]
    dir: Option<Colour>,
    #[serde(default)]
    link: Option<Colour>,
    #[serde(default)]
    exec: Option<Colour>,
    #[serde(default)]
    info: Option<Colour>,
    #[serde(default)]
    on_accent: Option<Colour>,
    #[serde(default)]
    bg: Option<Colour>,

    #[serde(default)]
    selection: Option<Colour>,
    #[serde(default)]
    cursorline: Option<Colour>,
    #[serde(default)]
    gutter: Option<Colour>,
    #[serde(default)]
    gutter_active: Option<Colour>,

    /// The colours code is drawn in. A theme leaving this out is not leaving
    /// code uncoloured — it is saying the twelve above already cover it.
    #[serde(default)]
    syntax: Option<FileSyntax>,

    /// Sixteen terminal colours, accepted and ignored. textfold draws no
    /// terminal, but sshman's themes carry these and a shared theme file
    /// should not have to be edited to cross between the two.
    #[serde(default)]
    #[allow(dead_code)]
    ansi: Option<Vec<Colour>>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileSyntax {
    #[serde(default)]
    keyword: Option<Colour>,
    #[serde(default)]
    function: Option<Colour>,
    #[serde(default, rename = "type")]
    type_: Option<Colour>,
    #[serde(default)]
    constructor: Option<Colour>,
    #[serde(default)]
    string: Option<Colour>,
    #[serde(default)]
    escape: Option<Colour>,
    #[serde(default)]
    number: Option<Colour>,
    #[serde(default)]
    boolean: Option<Colour>,
    #[serde(default)]
    comment: Option<Colour>,
    #[serde(default)]
    constant: Option<Colour>,
    #[serde(default)]
    variable: Option<Colour>,
    #[serde(default)]
    parameter: Option<Colour>,
    #[serde(default)]
    property: Option<Colour>,
    #[serde(default)]
    operator: Option<Colour>,
    #[serde(default)]
    punctuation: Option<Colour>,
    #[serde(default)]
    attribute: Option<Colour>,
    #[serde(default)]
    namespace: Option<Colour>,
    #[serde(default)]
    tag: Option<Colour>,
    #[serde(default)]
    label: Option<Colour>,
    #[serde(default)]
    error: Option<Colour>,
}

impl FileTheme {
    fn resolve(&self, base: Theme) -> Theme {
        let or = |c: Option<Colour>, fallback: Color| c.map(|c| c.0).unwrap_or(fallback);
        let mut theme = Theme {
            accent: or(self.accent, base.accent),
            dim: or(self.dim, base.dim),
            text: or(self.text, base.text),
            muted: or(self.muted, base.muted),
            good: or(self.good, base.good),
            warn: or(self.warn, base.warn),
            bad: or(self.bad, base.bad),
            dir: or(self.dir, base.dir),
            link: or(self.link, base.link),
            exec: or(self.exec, base.exec),
            info: or(self.info, base.info),
            on_accent: or(self.on_accent, base.on_accent),
            bg: or(self.bg, base.bg),
            // Filled in below, once the twelve they are worked out from are
            // settled.
            selection: base.selection,
            cursorline: base.cursorline,
            gutter: base.gutter,
            gutter_active: base.gutter_active,
            syntax: base.syntax,
        };

        // A theme that changed the twelve has changed what the rest mean, so
        // the derivations run again from the new ones. A theme that spelled a
        // colour out keeps it: saying so is the whole point of saying so.
        let derived = theme.derived_syntax();
        let s = self.syntax.as_ref();
        let pick = |chosen: Option<Colour>, derived: Color| match chosen {
            Some(c) => c.0,
            None => derived,
        };
        theme.syntax = Syntax {
            keyword: pick(s.and_then(|s| s.keyword), derived.keyword),
            function: pick(s.and_then(|s| s.function), derived.function),
            type_: pick(s.and_then(|s| s.type_), derived.type_),
            constructor: pick(s.and_then(|s| s.constructor), derived.constructor),
            string: pick(s.and_then(|s| s.string), derived.string),
            escape: pick(s.and_then(|s| s.escape), derived.escape),
            number: pick(s.and_then(|s| s.number), derived.number),
            boolean: pick(s.and_then(|s| s.boolean), derived.boolean),
            comment: pick(s.and_then(|s| s.comment), derived.comment),
            constant: pick(s.and_then(|s| s.constant), derived.constant),
            variable: pick(s.and_then(|s| s.variable), derived.variable),
            parameter: pick(s.and_then(|s| s.parameter), derived.parameter),
            property: pick(s.and_then(|s| s.property), derived.property),
            operator: pick(s.and_then(|s| s.operator), derived.operator),
            punctuation: pick(s.and_then(|s| s.punctuation), derived.punctuation),
            attribute: pick(s.and_then(|s| s.attribute), derived.attribute),
            namespace: pick(s.and_then(|s| s.namespace), derived.namespace),
            tag: pick(s.and_then(|s| s.tag), derived.tag),
            label: pick(s.and_then(|s| s.label), derived.label),
            error: pick(s.and_then(|s| s.error), derived.error),
        };
        theme.selection = or(self.selection, theme.derived_selection());
        theme.cursorline = or(self.cursorline, theme.derived_cursorline());
        theme.gutter = or(self.gutter, theme.dim);
        theme.gutter_active = or(self.gutter_active, theme.text);
        theme
    }
}

/// A colour as a theme file writes it: `"#7aa2f7"`, or one of the terminal's
/// own by name.
#[derive(Clone, Copy)]
struct Colour(Color);

impl<'de> Deserialize<'de> for Colour {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        parse_colour(&text)
            .map(Colour)
            .ok_or_else(|| serde::de::Error::custom(format!("{text:?} is not a colour")))
    }
}

/// A colour from the way a person writes one.
fn parse_colour(text: &str) -> Option<Color> {
    let text = text.trim();
    if let Some(hex) = text.strip_prefix('#') {
        let n = u32::from_str_radix(hex, 16).ok()?;
        return match hex.len() {
            6 => Some(Color::Rgb(
                (n >> 16) as u8,
                ((n >> 8) & 0xff) as u8,
                (n & 0xff) as u8,
            )),
            // `#f0c` for `#ff00cc`, since that is how CSS spells it and
            // people reach for it without thinking.
            3 => {
                let up = |v: u32| ((v * 17) & 0xff) as u8;
                Some(Color::Rgb(up(n >> 8), up((n >> 4) & 0xf), up(n & 0xf)))
            }
            _ => None,
        };
    }
    // A number is a palette slot, which is what a 256-colour terminal calls
    // its colours.
    if let Ok(n) = text.parse::<u8>() {
        return Some(Color::Indexed(n));
    }
    // `dark gray`, `dark-grey` and `darkgray` are all the same colour, and
    // nobody should have to remember which spelling we picked.
    let name: String = text
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    Some(match name.as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" | "purple" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" | "lightgray" | "lightgrey" => Color::Gray,
        "darkgray" | "darkgrey" | "brightblack" => Color::DarkGray,
        "lightred" | "brightred" => Color::LightRed,
        "lightgreen" | "brightgreen" => Color::LightGreen,
        "lightyellow" | "brightyellow" => Color::LightYellow,
        "lightblue" | "brightblue" => Color::LightBlue,
        "lightmagenta" | "brightmagenta" => Color::LightMagenta,
        "lightcyan" | "brightcyan" => Color::LightCyan,
        "white" | "brightwhite" => Color::White,
        // Not a colour so much as the absence of one: whatever the terminal
        // is already using.
        "default" | "none" | "terminal" => Color::Reset,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shipped_theme_reads() {
        let themes = Themes::built_in();
        assert!(themes.problems.is_empty(), "{:?}", themes.problems);
        assert_eq!(themes.entries.len(), BUILT_IN.len());
    }

    #[test]
    fn the_fallback_and_its_file_agree() {
        let themes = Themes::built_in();
        assert_eq!(themes.by_name(DEFAULT), Some(FALLBACK));
    }

    #[test]
    fn a_theme_naming_no_code_colours_still_has_them() {
        let themes = Themes::built_in();
        let tokyo = themes.by_name("tokyonight").expect("shipped");
        // Not the fallback's, and not all the same: worked out from the
        // twelve this theme does name.
        assert_eq!(tokyo.syntax.string, tokyo.good);
        assert_eq!(tokyo.syntax.comment, tokyo.dim);
        assert_ne!(tokyo.syntax.keyword, tokyo.syntax.function);
    }

    #[test]
    fn a_theme_with_a_background_gets_a_selection_to_match() {
        let themes = Themes::built_in();
        let tokyo = themes.by_name("tokyonight").expect("shipped");
        assert!(matches!(tokyo.selection, Color::Rgb(..)));
        assert_ne!(tokyo.selection, tokyo.bg);
        // And one without leaves the terminal's own alone.
        let terminal = themes.by_name("terminal").expect("shipped");
        assert_eq!(terminal.cursorline, Color::Reset);
    }

    #[test]
    fn colours_are_read_the_way_people_write_them() {
        assert_eq!(parse_colour("#7aa2f7"), Some(Color::Rgb(0x7a, 0xa2, 0xf7)));
        assert_eq!(parse_colour("#f0c"), Some(Color::Rgb(0xff, 0x00, 0xcc)));
        assert_eq!(parse_colour("39"), Some(Color::Indexed(39)));
        assert_eq!(parse_colour("blue"), Some(Color::Blue));
        assert_eq!(parse_colour("#gg"), None);
    }
}
