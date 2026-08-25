//! Colours, by what they mean rather than what they are.
//!
//! A theme is three groups of names, and the groups are the three things
//! textfold puts on a screen.
//!
//! `ui` is the chrome: the tab row, the status bar, the borders of a picker,
//! the words in a dialog. Ten roles, and they are about *tone* — this worked,
//! this is worth a look, this is deliberately in the background.
//!
//! `editor` is the pane the text is in: what is behind a selection, what marks
//! the line you are on, the line numbers, the extra cursors.
//!
//! `syntax` is the code: thirty-one roles, one for each kind of thing a
//! grammar can point at. Every theme textfold ships spells all thirty-one out,
//! because code is what you are looking at all day and "close enough" is not.
//!
//! A theme of your own does not have to. Anything left out is worked out: from
//! the theme named in `base`, and failing that from `ui`, where every kind of
//! code has a tone that already means it. Strings are the colour of things that
//! worked, comments the colour of things deliberately in the background. That
//! gets you a readable editor from ten colours; spelling `syntax` out gets you
//! a good one.
//!
//! The tables are not in this file. Each is a small JSON file: the ones
//! textfold ships live in `themes/` and are built into the binary, and any
//! file dropped in `~/.config/textfold/themes/` is loaded beside them. A file
//! taking a name textfold already uses replaces it, which is how you rewrite
//! one of ours without forking anything.

use std::path::PathBuf;

use ratatui::style::Color;
use serde::Deserialize;

/// Every colour textfold draws with. Copied on every span, so it stays `Copy`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Theme {
    // ---- ui: the chrome around the text ----
    /// What to paint behind everything. [`Color::Reset`] means the terminal's
    /// own, which is what a theme naming no background gets.
    pub background: Color,
    /// Ordinary text you are meant to read.
    pub foreground: Color,
    /// Text that is there when you look for it: counts, labels, positions.
    pub muted: Color,
    /// Anything deliberately in the background: hints, unfocused borders,
    /// the key beside a command in a list.
    pub faint: Color,
    /// Focused borders, titles, the marks that say "you are here".
    pub accent: Color,
    /// Text drawn *on* a coloured chip, so it contrasts with the colours
    /// above rather than with the terminal.
    pub on_accent: Color,
    /// It worked.
    pub success: Color,
    /// Worth a second look.
    pub warning: Color,
    /// It did not work, or it is about to do something irreversible.
    pub error: Color,
    /// Badges that are telling you something rather than warning you.
    pub info: Color,

    // ---- editor: the pane the text is in ----
    /// Behind selected text.
    pub selection: Color,
    /// Behind the line the cursor is on. Equal to [`Theme::background`] means
    /// no highlight at all, which is what a terminal-coloured theme gets,
    /// since there is no background to lighten.
    pub current_line: Color,
    /// Line numbers, and the column they sit in.
    pub gutter: Color,
    /// The line number the cursor is on.
    pub gutter_current: Color,
    /// The block drawn where an extra cursor is. The terminal has one real
    /// cursor and multi-cursor editing needs forty, so the other thirty-nine
    /// are painted.
    pub cursor: Color,
    /// The bracket under the cursor and the one that closes it.
    pub bracket_match: Color,
    /// The dots and arrows that stand in for spaces and tabs, when they are
    /// being shown at all.
    pub whitespace: Color,
    /// The vertical rules down the columns you asked to be warned about.
    pub ruler: Color,

    // ---- git: what has happened to a line since the last commit ----
    /// A line that was not there before.
    pub added: Color,
    /// A line that is not what it was.
    pub changed: Color,
    /// Where a line used to be.
    pub removed: Color,

    /// The colours code is drawn in.
    pub syntax: Syntax,
}

/// What each kind of code is coloured.
///
/// The names are tree-sitter's capture names, cut down to the ones a person
/// can hold in their head. Anything more specific in a grammar's queries falls
/// back along the dots, so `@function.method.builtin` lands on `method`
/// without anyone saying so, and `@keyword.operator.overload` lands on
/// `keyword`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Syntax {
    /// `let`, `pub`, `struct` — the words that are the language.
    pub keyword: Color,
    /// `if`, `for`, `return`, `throw` — the words that are the control flow.
    pub keyword_control: Color,
    /// A function where it is defined, and where it is called.
    pub function: Color,
    /// One the language provides: `len`, `printf`, `sizeof`.
    pub function_builtin: Color,
    /// A function reached through a value: `x.parse()`.
    pub method: Color,
    /// `println!`, `#define`, `@decorator` — code that writes code.
    pub macro_: Color,
    /// A type by name: `HashMap`, `Widget`.
    pub type_: Color,
    /// One the language has always had: `u32`, `int`, `string`.
    pub type_builtin: Color,
    /// A type used to make one: `Some(x)`, `Point { .. }`.
    pub constructor: Color,
    /// A string literal.
    pub string: Color,
    /// `\n`, `\u{1f600}` — the parts of a string that are not text.
    pub string_escape: Color,
    /// A string the language reads rather than prints: a regex, a format
    /// specifier, a path in an import.
    pub string_special: Color,
    /// `'a'` — a character literal, which is not quite a string.
    pub character: Color,
    /// A number literal.
    pub number: Color,
    /// `true`, `false`, `nil`, `None`.
    pub boolean: Color,
    /// A comment.
    pub comment: Color,
    /// A comment that is documentation: `///`, `/** */`, a docstring.
    pub comment_doc: Color,
    /// A constant by name: `MAX_SIZE`, an enum member.
    pub constant: Color,
    /// A variable by name.
    pub variable: Color,
    /// One the language gives you: `self`, `this`, `super`.
    pub variable_builtin: Color,
    /// A parameter, where it is declared and where it is used.
    pub parameter: Color,
    /// A field on a value: the `name` in `widget.name`.
    pub property: Color,
    /// `+`, `==`, `=>`.
    pub operator: Color,
    /// Anything punctuation that is not a bracket or a separator.
    pub punctuation: Color,
    /// `(`, `[`, `{` and their partners.
    pub bracket: Color,
    /// `,`, `;`, `.` — what separates one thing from the next.
    pub delimiter: Color,
    /// `#[derive(..)]`, `@Override`, `[Obsolete]`.
    pub attribute: Color,
    /// A module or namespace by name.
    pub namespace: Color,
    /// `<div>` — a markup tag.
    pub tag: Color,
    /// `'outer:` — a loop label, a goto target.
    pub label: Color,
    /// Text the grammar could not parse.
    pub error: Color,
}

/// One kind of code, as the highlighter hands it to the drawing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Keyword,
    KeywordControl,
    Function,
    FunctionBuiltin,
    Method,
    Macro,
    Type,
    TypeBuiltin,
    Constructor,
    String,
    StringEscape,
    StringSpecial,
    Character,
    Number,
    Boolean,
    Comment,
    CommentDoc,
    Constant,
    Variable,
    VariableBuiltin,
    Parameter,
    Property,
    Operator,
    Punctuation,
    Bracket,
    Delimiter,
    Attribute,
    Namespace,
    Tag,
    Label,
    Error,
}

/// Every capture name textfold knows. A grammar's query names a capture like
/// `@keyword.control.repeat`; the longest of these that is a prefix of it
/// along the dots wins, so a grammar can be as specific as it likes and still
/// be coloured.
pub const CAPTURES: &[(&str, Role)] = &[
    ("keyword", Role::Keyword),
    ("keyword.control", Role::KeywordControl),
    ("keyword.conditional", Role::KeywordControl),
    ("keyword.repeat", Role::KeywordControl),
    ("keyword.return", Role::KeywordControl),
    ("keyword.exception", Role::KeywordControl),
    ("keyword.coroutine", Role::KeywordControl),
    ("conditional", Role::KeywordControl),
    ("repeat", Role::KeywordControl),
    ("exception", Role::KeywordControl),
    ("keyword.directive", Role::Macro),
    ("function", Role::Function),
    ("function.builtin", Role::FunctionBuiltin),
    ("function.method", Role::Method),
    ("method", Role::Method),
    ("function.macro", Role::Macro),
    ("macro", Role::Macro),
    ("preproc", Role::Macro),
    ("type", Role::Type),
    ("type.builtin", Role::TypeBuiltin),
    ("constructor", Role::Constructor),
    ("string", Role::String),
    ("string.escape", Role::StringEscape),
    ("escape", Role::StringEscape),
    ("string.special", Role::StringSpecial),
    ("string.regexp", Role::StringSpecial),
    ("string.regex", Role::StringSpecial),
    ("regex", Role::StringSpecial),
    ("character", Role::Character),
    ("character.special", Role::StringEscape),
    ("number", Role::Number),
    ("float", Role::Number),
    ("boolean", Role::Boolean),
    ("constant", Role::Constant),
    ("constant.builtin", Role::Boolean),
    ("constant.macro", Role::Macro),
    ("comment", Role::Comment),
    ("comment.documentation", Role::CommentDoc),
    ("comment.doc", Role::CommentDoc),
    ("variable", Role::Variable),
    ("variable.builtin", Role::VariableBuiltin),
    ("variable.parameter", Role::Parameter),
    ("parameter", Role::Parameter),
    ("variable.member", Role::Property),
    ("property", Role::Property),
    ("field", Role::Property),
    ("operator", Role::Operator),
    ("punctuation", Role::Punctuation),
    ("punctuation.bracket", Role::Bracket),
    ("punctuation.delimiter", Role::Delimiter),
    ("attribute", Role::Attribute),
    ("annotation", Role::Attribute),
    ("tag.attribute", Role::Attribute),
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
            Role::KeywordControl => s.keyword_control,
            Role::Function => s.function,
            Role::FunctionBuiltin => s.function_builtin,
            Role::Method => s.method,
            Role::Macro => s.macro_,
            Role::Type => s.type_,
            Role::TypeBuiltin => s.type_builtin,
            Role::Constructor => s.constructor,
            Role::String => s.string,
            Role::StringEscape => s.string_escape,
            Role::StringSpecial => s.string_special,
            Role::Character => s.character,
            Role::Number => s.number,
            Role::Boolean => s.boolean,
            Role::Comment => s.comment,
            Role::CommentDoc => s.comment_doc,
            Role::Constant => s.constant,
            Role::Variable => s.variable,
            Role::VariableBuiltin => s.variable_builtin,
            Role::Parameter => s.parameter,
            Role::Property => s.property,
            Role::Operator => s.operator,
            Role::Punctuation => s.punctuation,
            Role::Bracket => s.bracket,
            Role::Delimiter => s.delimiter,
            Role::Attribute => s.attribute,
            Role::Namespace => s.namespace,
            Role::Tag => s.tag,
            Role::Label => s.label,
            Role::Error => s.error,
        }
    }

    /// Barely there: the tab row and the status bar sit on this, and so does
    /// the rule between two panes. Halfway from the background towards the
    /// faint colour, or the dimmest thing a terminal-coloured theme has.
    pub fn chrome(&self) -> Color {
        blend(self.background, self.faint, 0.45).unwrap_or(Color::DarkGray)
    }

    /// The colours code gets where nothing has said: each kind drawn in
    /// whichever of the ten `ui` tones already means it.
    ///
    /// It is not a fudge — a string literal really is a thing that worked, and
    /// a comment really is text deliberately in the background. It gets a
    /// ten-colour theme a readable editor. Every theme textfold ships says
    /// more than this, because an editor you read all day is worth more.
    fn derived_syntax(&self) -> Syntax {
        // A hue between the accent and the warning, for the kinds of code that
        // want to be distinct from both. Falls back to the accent where the
        // theme's colours are terminal slots and there is nothing to mix.
        let between = blend(self.accent, self.warning, 0.5).unwrap_or(self.accent);
        Syntax {
            keyword: self.accent,
            keyword_control: self.accent,
            function: self.info,
            function_builtin: self.info,
            method: self.info,
            macro_: between,
            type_: self.warning,
            type_builtin: self.warning,
            constructor: self.warning,
            string: self.success,
            string_escape: between,
            string_special: between,
            character: self.success,
            number: self.warning,
            boolean: self.warning,
            comment: self.faint,
            comment_doc: self.faint,
            constant: self.warning,
            variable: self.foreground,
            variable_builtin: self.accent,
            parameter: self.muted,
            property: self.info,
            operator: self.muted,
            punctuation: self.muted,
            bracket: self.muted,
            delimiter: self.muted,
            attribute: between,
            namespace: self.info,
            tag: self.accent,
            label: between,
            error: self.error,
        }
    }

    /// What to paint behind selected text when a theme has not said. A quarter
    /// of the way from the background towards the accent keeps whatever the
    /// text was coloured legible, which a flat blue does not.
    fn derived_selection(&self) -> Color {
        blend(self.background, self.accent, 0.30).unwrap_or(Color::DarkGray)
    }

    /// What to paint behind the cursor's line. A tenth of the way towards the
    /// text: enough to find, not enough to read through.
    ///
    /// A theme with no background of its own gets none, because there is
    /// nothing to lighten — lightening the terminal's own would mean guessing
    /// what it is.
    fn derived_current_line(&self) -> Color {
        blend(self.background, self.foreground, 0.07).unwrap_or(self.background)
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
    background: Color::Reset,
    foreground: Color::White,
    muted: Color::Gray,
    faint: Color::DarkGray,
    accent: Color::Cyan,
    on_accent: Color::Black,
    success: Color::Green,
    warning: Color::Yellow,
    error: Color::Red,
    info: Color::Blue,

    selection: Color::DarkGray,
    current_line: Color::Reset,
    gutter: Color::DarkGray,
    gutter_current: Color::White,
    cursor: Color::Cyan,
    bracket_match: Color::Cyan,
    whitespace: Color::DarkGray,
    ruler: Color::DarkGray,

    added: Color::Green,
    changed: Color::Blue,
    removed: Color::Red,

    syntax: Syntax {
        keyword: Color::Magenta,
        keyword_control: Color::LightMagenta,
        function: Color::Cyan,
        function_builtin: Color::LightCyan,
        method: Color::Cyan,
        macro_: Color::LightMagenta,
        type_: Color::Blue,
        type_builtin: Color::LightBlue,
        constructor: Color::LightBlue,
        string: Color::Green,
        string_escape: Color::LightGreen,
        string_special: Color::LightGreen,
        character: Color::Green,
        number: Color::Yellow,
        boolean: Color::LightYellow,
        comment: Color::DarkGray,
        comment_doc: Color::Gray,
        constant: Color::LightYellow,
        variable: Color::White,
        variable_builtin: Color::LightRed,
        parameter: Color::Gray,
        property: Color::Blue,
        operator: Color::Gray,
        punctuation: Color::Gray,
        bracket: Color::Gray,
        delimiter: Color::Gray,
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

/// A theme as its file writes it.
///
/// Every colour is optional: what a file leaves out comes from the theme named
/// in `base`, and what no theme in the chain mentions is worked out from `ui`.
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
    ui: Option<FileUi>,
    #[serde(default)]
    editor: Option<FileEditor>,
    #[serde(default)]
    syntax: Option<FileSyntax>,

    /// A theme file written for sshman, whose twelve roles are flat at the top
    /// level and named for a file manager's job rather than an editor's.
    /// textfold reads one — a theme you already have should not stop working
    /// because the names got better — and everything above wins over it.
    #[serde(flatten)]
    legacy: Legacy,
}

/// The chrome: ten tones.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileUi {
    #[serde(default)]
    background: Option<Colour>,
    #[serde(default)]
    foreground: Option<Colour>,
    #[serde(default)]
    muted: Option<Colour>,
    #[serde(default)]
    faint: Option<Colour>,
    #[serde(default)]
    accent: Option<Colour>,
    #[serde(default)]
    on_accent: Option<Colour>,
    #[serde(default)]
    success: Option<Colour>,
    #[serde(default)]
    warning: Option<Colour>,
    #[serde(default)]
    error: Option<Colour>,
    #[serde(default)]
    info: Option<Colour>,
}

/// The pane the text is in.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileEditor {
    #[serde(default)]
    selection: Option<Colour>,
    #[serde(default)]
    current_line: Option<Colour>,
    #[serde(default)]
    gutter: Option<Colour>,
    #[serde(default)]
    gutter_current: Option<Colour>,
    #[serde(default)]
    cursor: Option<Colour>,
    #[serde(default)]
    bracket_match: Option<Colour>,
    #[serde(default)]
    whitespace: Option<Colour>,
    #[serde(default)]
    ruler: Option<Colour>,
    #[serde(default)]
    added: Option<Colour>,
    #[serde(default)]
    changed: Option<Colour>,
    #[serde(default)]
    removed: Option<Colour>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileSyntax {
    #[serde(default)]
    keyword: Option<Colour>,
    #[serde(default)]
    keyword_control: Option<Colour>,
    #[serde(default)]
    function: Option<Colour>,
    #[serde(default)]
    function_builtin: Option<Colour>,
    #[serde(default)]
    method: Option<Colour>,
    #[serde(default, rename = "macro")]
    macro_: Option<Colour>,
    #[serde(default, rename = "type")]
    type_: Option<Colour>,
    #[serde(default)]
    type_builtin: Option<Colour>,
    #[serde(default)]
    constructor: Option<Colour>,
    #[serde(default)]
    string: Option<Colour>,
    #[serde(default, alias = "escape")]
    string_escape: Option<Colour>,
    #[serde(default)]
    string_special: Option<Colour>,
    #[serde(default)]
    character: Option<Colour>,
    #[serde(default)]
    number: Option<Colour>,
    #[serde(default)]
    boolean: Option<Colour>,
    #[serde(default)]
    comment: Option<Colour>,
    #[serde(default)]
    comment_doc: Option<Colour>,
    #[serde(default)]
    constant: Option<Colour>,
    #[serde(default)]
    variable: Option<Colour>,
    #[serde(default)]
    variable_builtin: Option<Colour>,
    #[serde(default)]
    parameter: Option<Colour>,
    #[serde(default)]
    property: Option<Colour>,
    #[serde(default)]
    operator: Option<Colour>,
    #[serde(default)]
    punctuation: Option<Colour>,
    #[serde(default)]
    bracket: Option<Colour>,
    #[serde(default)]
    delimiter: Option<Colour>,
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

/// sshman's twelve, flat at the top level, plus the four pane colours textfold
/// used to keep beside them.
///
/// `dir`, `link` and `exec` are the colours of a directory, a symlink and
/// something you could run in a file listing. textfold does not draw a file
/// listing, so it reads them and does nothing with them rather than pretending
/// they mean something here. Same for `ansi`: textfold draws no terminal.
#[derive(Deserialize, Default)]
struct Legacy {
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

    #[serde(default)]
    #[allow(dead_code)]
    dir: Option<Colour>,
    #[serde(default)]
    #[allow(dead_code)]
    link: Option<Colour>,
    #[serde(default)]
    #[allow(dead_code)]
    exec: Option<Colour>,
    #[serde(default)]
    #[allow(dead_code)]
    ansi: Option<Vec<Colour>>,
}

/// The first of these that says anything, or the fallback.
fn first(chosen: [Option<Colour>; 2], fallback: Color) -> Color {
    chosen
        .into_iter()
        .flatten()
        .next()
        .map(|c| c.0)
        .unwrap_or(fallback)
}

impl FileTheme {
    fn resolve(&self, base: Theme) -> Theme {
        let ui = self.ui.as_ref();
        let ed = self.editor.as_ref();
        let old = &self.legacy;

        let mut theme = Theme {
            background: first([ui.and_then(|u| u.background), old.bg], base.background),
            foreground: first([ui.and_then(|u| u.foreground), old.text], base.foreground),
            muted: first([ui.and_then(|u| u.muted), old.muted], base.muted),
            faint: first([ui.and_then(|u| u.faint), old.dim], base.faint),
            accent: first([ui.and_then(|u| u.accent), old.accent], base.accent),
            on_accent: first([ui.and_then(|u| u.on_accent), old.on_accent], base.on_accent),
            success: first([ui.and_then(|u| u.success), old.good], base.success),
            warning: first([ui.and_then(|u| u.warning), old.warn], base.warning),
            error: first([ui.and_then(|u| u.error), old.bad], base.error),
            info: first([ui.and_then(|u| u.info), old.info], base.info),
            // Filled in below, once the ten they are worked out from are
            // settled.
            selection: base.selection,
            current_line: base.current_line,
            gutter: base.gutter,
            gutter_current: base.gutter_current,
            cursor: base.cursor,
            bracket_match: base.bracket_match,
            whitespace: base.whitespace,
            ruler: base.ruler,
            added: base.added,
            changed: base.changed,
            removed: base.removed,
            syntax: base.syntax,
        };

        // What a theme did not say about code comes from the theme it is
        // based on — that is what "based on" means, and a file that changes
        // one comment colour should not lose the other thirty.
        //
        // A theme based on nothing has no such palette to inherit, so its code
        // colours are worked out from the ten it just named. Re-deriving here
        // rather than taking the fallback's is the point: they have to be this
        // theme's colours, not the terminal's.
        let d = match self.base {
            Some(_) => base.syntax,
            None => theme.derived_syntax(),
        };
        let s = self.syntax.as_ref();
        let pick = |chosen: Option<Colour>, derived: Color| match chosen {
            Some(c) => c.0,
            None => derived,
        };
        theme.syntax = Syntax {
            keyword: pick(s.and_then(|s| s.keyword), d.keyword),
            keyword_control: pick(s.and_then(|s| s.keyword_control), d.keyword_control),
            function: pick(s.and_then(|s| s.function), d.function),
            function_builtin: pick(s.and_then(|s| s.function_builtin), d.function_builtin),
            method: pick(s.and_then(|s| s.method), d.method),
            macro_: pick(s.and_then(|s| s.macro_), d.macro_),
            type_: pick(s.and_then(|s| s.type_), d.type_),
            type_builtin: pick(s.and_then(|s| s.type_builtin), d.type_builtin),
            constructor: pick(s.and_then(|s| s.constructor), d.constructor),
            string: pick(s.and_then(|s| s.string), d.string),
            string_escape: pick(s.and_then(|s| s.string_escape), d.string_escape),
            string_special: pick(s.and_then(|s| s.string_special), d.string_special),
            character: pick(s.and_then(|s| s.character), d.character),
            number: pick(s.and_then(|s| s.number), d.number),
            boolean: pick(s.and_then(|s| s.boolean), d.boolean),
            comment: pick(s.and_then(|s| s.comment), d.comment),
            comment_doc: pick(s.and_then(|s| s.comment_doc), d.comment_doc),
            constant: pick(s.and_then(|s| s.constant), d.constant),
            variable: pick(s.and_then(|s| s.variable), d.variable),
            variable_builtin: pick(s.and_then(|s| s.variable_builtin), d.variable_builtin),
            parameter: pick(s.and_then(|s| s.parameter), d.parameter),
            property: pick(s.and_then(|s| s.property), d.property),
            operator: pick(s.and_then(|s| s.operator), d.operator),
            punctuation: pick(s.and_then(|s| s.punctuation), d.punctuation),
            bracket: pick(s.and_then(|s| s.bracket), d.bracket),
            delimiter: pick(s.and_then(|s| s.delimiter), d.delimiter),
            attribute: pick(s.and_then(|s| s.attribute), d.attribute),
            namespace: pick(s.and_then(|s| s.namespace), d.namespace),
            tag: pick(s.and_then(|s| s.tag), d.tag),
            label: pick(s.and_then(|s| s.label), d.label),
            error: pick(s.and_then(|s| s.error), d.error),
        };

        theme.selection = first(
            [ed.and_then(|e| e.selection), old.selection],
            theme.derived_selection(),
        );
        theme.current_line = first(
            [ed.and_then(|e| e.current_line), old.cursorline],
            theme.derived_current_line(),
        );
        theme.gutter = first([ed.and_then(|e| e.gutter), old.gutter], theme.faint);
        theme.gutter_current = first(
            [ed.and_then(|e| e.gutter_current), old.gutter_active],
            theme.foreground,
        );
        theme.cursor = first([ed.and_then(|e| e.cursor), None], theme.accent);
        theme.bracket_match = first([ed.and_then(|e| e.bracket_match), None], theme.accent);
        theme.whitespace = first([ed.and_then(|e| e.whitespace), None], theme.faint);
        theme.ruler = first([ed.and_then(|e| e.ruler), None], theme.faint);
        // A theme that says nothing about git gets the three colours every
        // diff has used since diffs were in colour, taken from the tones it
        // did name so that they belong to it rather than to the terminal.
        theme.added = first([ed.and_then(|e| e.added), None], theme.success);
        theme.changed = first([ed.and_then(|e| e.changed), None], theme.info);
        theme.removed = first([ed.and_then(|e| e.removed), None], theme.error);
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

    /// Every role there is, so a test that means "all of them" says so once.
    const ALL_ROLES: &[Role] = &[
        Role::Keyword,
        Role::KeywordControl,
        Role::Function,
        Role::FunctionBuiltin,
        Role::Method,
        Role::Macro,
        Role::Type,
        Role::TypeBuiltin,
        Role::Constructor,
        Role::String,
        Role::StringEscape,
        Role::StringSpecial,
        Role::Character,
        Role::Number,
        Role::Boolean,
        Role::Comment,
        Role::CommentDoc,
        Role::Constant,
        Role::Variable,
        Role::VariableBuiltin,
        Role::Parameter,
        Role::Property,
        Role::Operator,
        Role::Punctuation,
        Role::Bracket,
        Role::Delimiter,
        Role::Attribute,
        Role::Namespace,
        Role::Tag,
        Role::Label,
        Role::Error,
    ];

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
    fn every_shipped_theme_spells_out_every_kind_of_code() {
        // The point of the derived colours is that a ten-colour theme still
        // works. The point of shipping eighteen is that they are better than
        // that — so none of them may be leaning on the derivation.
        for (file, text) in BUILT_IN {
            let parsed: FileTheme = serde_json::from_str(text).expect("reads");
            let syntax = parsed.syntax.as_ref().unwrap_or_else(|| {
                panic!("{file} says nothing about code");
            });
            let missing: Vec<&str> = [
                ("keyword", syntax.keyword.is_none()),
                ("keyword_control", syntax.keyword_control.is_none()),
                ("function", syntax.function.is_none()),
                ("function_builtin", syntax.function_builtin.is_none()),
                ("method", syntax.method.is_none()),
                ("macro", syntax.macro_.is_none()),
                ("type", syntax.type_.is_none()),
                ("type_builtin", syntax.type_builtin.is_none()),
                ("constructor", syntax.constructor.is_none()),
                ("string", syntax.string.is_none()),
                ("string_escape", syntax.string_escape.is_none()),
                ("string_special", syntax.string_special.is_none()),
                ("character", syntax.character.is_none()),
                ("number", syntax.number.is_none()),
                ("boolean", syntax.boolean.is_none()),
                ("comment", syntax.comment.is_none()),
                ("comment_doc", syntax.comment_doc.is_none()),
                ("constant", syntax.constant.is_none()),
                ("variable", syntax.variable.is_none()),
                ("variable_builtin", syntax.variable_builtin.is_none()),
                ("parameter", syntax.parameter.is_none()),
                ("property", syntax.property.is_none()),
                ("operator", syntax.operator.is_none()),
                ("punctuation", syntax.punctuation.is_none()),
                ("bracket", syntax.bracket.is_none()),
                ("delimiter", syntax.delimiter.is_none()),
                ("attribute", syntax.attribute.is_none()),
                ("namespace", syntax.namespace.is_none()),
                ("tag", syntax.tag.is_none()),
                ("label", syntax.label.is_none()),
                ("error", syntax.error.is_none()),
            ]
            .into_iter()
            .filter(|(_, missing)| *missing)
            .map(|(name, _)| name)
            .collect();
            assert!(missing.is_empty(), "{file} says nothing about {missing:?}");
        }
    }

    #[test]
    fn every_shipped_theme_colours_code_against_its_own_background() {
        // A role left at the fallback's colour in a theme with a background of
        // its own is a role somebody forgot, and it shows up as one word in
        // the wrong palette halfway down a file.
        let themes = Themes::built_in();
        for named in &themes.entries {
            if !matches!(named.theme.background, Color::Rgb(..)) {
                continue;
            }
            for role in ALL_ROLES {
                let colour = named.theme.role(*role);
                assert!(
                    matches!(colour, Color::Rgb(..)),
                    "{}: {role:?} is {colour:?}, not a colour of its own",
                    named.name
                );
            }
        }
    }

    #[test]
    fn a_theme_naming_no_code_colours_still_has_them() {
        let themes = Themes::built_in();
        let ten = r##"{
            "name": "ten",
            "ui": {
                "background": "#101010", "foreground": "#eeeeee",
                "muted": "#aaaaaa", "faint": "#666666",
                "accent": "#7aa2f7", "on_accent": "#101010",
                "success": "#9ece6a", "warning": "#e0af68",
                "error": "#f7768e", "info": "#2ac3de"
            }
        }"##;
        let mut themes = themes;
        themes.add(ten, "ten.json");
        assert!(themes.problems.is_empty(), "{:?}", themes.problems);
        let theme = themes.by_name("ten").expect("added");
        // Worked out from the ten, not left at the fallback's.
        assert_eq!(theme.syntax.string, theme.success);
        assert_eq!(theme.syntax.comment, theme.faint);
        assert_ne!(theme.syntax.keyword, theme.syntax.function);
        for role in ALL_ROLES {
            assert!(matches!(theme.role(*role), Color::Rgb(..)), "{role:?}");
        }
    }

    #[test]
    fn a_theme_written_for_sshman_still_reads() {
        // The twelve flat roles, `dir`/`link`/`exec` and all, plus the sixteen
        // terminal colours. Nothing here is textfold's schema, and all of it
        // has to land somewhere sensible.
        let mut themes = Themes::built_in();
        themes.add(
            r##"{
                "name": "fromsshman",
                "accent": "#7aa2f7", "dim": "#565f89", "text": "#c0caf5",
                "muted": "#a9b1d6", "good": "#9ece6a", "warn": "#e0af68",
                "bad": "#f7768e", "dir": "#7dcfff", "link": "#bb9af7",
                "exec": "#9ece6a", "info": "#2ac3de", "bg": "#1a1b26",
                "on_accent": "#1a1b26",
                "ansi": ["black", "red", "green", "yellow", "blue", "magenta",
                         "cyan", "gray", "darkgray", "lightred", "lightgreen",
                         "lightyellow", "lightblue", "lightmagenta",
                         "lightcyan", "white"]
            }"##,
            "fromsshman.json",
        );
        assert!(themes.problems.is_empty(), "{:?}", themes.problems);
        let theme = themes.by_name("fromsshman").expect("added");
        assert_eq!(theme.background, Color::Rgb(0x1a, 0x1b, 0x26));
        assert_eq!(theme.foreground, Color::Rgb(0xc0, 0xca, 0xf5));
        assert_eq!(theme.faint, Color::Rgb(0x56, 0x5f, 0x89));
        assert_eq!(theme.success, Color::Rgb(0x9e, 0xce, 0x6a));
        assert_eq!(theme.error, Color::Rgb(0xf7, 0x76, 0x8e));
        // And it is coloured throughout, from those alone.
        for role in ALL_ROLES {
            assert!(matches!(theme.role(*role), Color::Rgb(..)), "{role:?}");
        }
    }

    #[test]
    fn the_new_names_win_over_the_old_ones() {
        let mut themes = Themes::built_in();
        themes.add(
            r##"{
                "name": "both",
                "text": "#111111",
                "ui": { "foreground": "#222222" }
            }"##,
            "both.json",
        );
        assert!(themes.problems.is_empty(), "{:?}", themes.problems);
        let theme = themes.by_name("both").expect("added");
        assert_eq!(theme.foreground, Color::Rgb(0x22, 0x22, 0x22));
    }

    #[test]
    fn a_theme_with_a_background_gets_a_selection_to_match() {
        let themes = Themes::built_in();
        let tokyo = themes.by_name("tokyonight").expect("shipped");
        assert!(matches!(tokyo.selection, Color::Rgb(..)));
        assert_ne!(tokyo.selection, tokyo.background);
        // And one without leaves the terminal's own alone.
        let terminal = themes.by_name("terminal").expect("shipped");
        assert_eq!(terminal.current_line, Color::Reset);
    }

    #[test]
    fn a_theme_can_be_a_change_to_another_one() {
        let mut themes = Themes::built_in();
        themes.add(
            r##"{
                "name": "mine",
                "base": "tokyonight",
                "syntax": { "comment": "#4a4a5e" }
            }"##,
            "mine.json",
        );
        assert!(themes.problems.is_empty(), "{:?}", themes.problems);
        let mine = themes.by_name("mine").expect("added");
        let tokyo = themes.by_name("tokyonight").expect("shipped");
        assert_eq!(mine.syntax.comment, Color::Rgb(0x4a, 0x4a, 0x5e));
        // Everything it did not mention is the theme it came from.
        assert_eq!(mine.syntax.keyword, tokyo.syntax.keyword);
        assert_eq!(mine.background, tokyo.background);
    }

    #[test]
    fn a_capture_name_falls_back_along_its_dots() {
        let role = |name: &str| {
            let mut candidate = name;
            loop {
                if let Some((_, role)) = CAPTURES
                    .iter()
                    .filter(|(key, _)| *key == candidate)
                    .max_by_key(|(key, _)| key.len())
                {
                    return Some(*role);
                }
                match candidate.rfind('.') {
                    Some(at) => candidate = &candidate[..at],
                    None => return None,
                }
            }
        };
        assert_eq!(role("keyword"), Some(Role::Keyword));
        assert_eq!(role("keyword.control.repeat"), Some(Role::KeywordControl));
        assert_eq!(role("keyword.operator"), Some(Role::Keyword));
        assert_eq!(role("function.method.call"), Some(Role::Method));
        assert_eq!(role("punctuation.bracket"), Some(Role::Bracket));
        assert_eq!(role("punctuation.delimiter"), Some(Role::Delimiter));
        assert_eq!(role("type.builtin"), Some(Role::TypeBuiltin));
        assert_eq!(role("variable.parameter.builtin"), Some(Role::Parameter));
        assert_eq!(role("comment.documentation"), Some(Role::CommentDoc));
        assert_eq!(role("nothing.like.this"), None);
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
