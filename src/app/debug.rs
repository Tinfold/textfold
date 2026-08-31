//! Debugging.
//!
//! The same shape as everything else the editor talks to: a program somebody
//! else wrote, at the end of a pipe, whose messages arrive on the channel the
//! keyboard arrives on. What is different is that a debugger has a *place* —
//! the line the program is stopped on — and the whole of the interface is
//! about making that place obvious: an arrow in the margin, the cursor put on
//! it, and a panel along the bottom saying how it got there and what the
//! values are.

use super::*;

impl App {
    /// Start debugging, or let a stopped program go again.
    ///
    /// One key for both, which is what every debugger has settled on: while
    /// nothing is running it means "run this", and while something is stopped
    /// it means "carry on". There is never a moment where both readings are
    /// available, so there is never a moment where it is ambiguous.
    pub(super) fn debug(&mut self) {
        if let Some(session) = self.debug.session() {
            if session.state.is_stopped() {
                self.debug.resume();
                return self.refresh_debug_panel();
            }
            if !session.state.is_over() {
                return self.say("it is already running — Shift-F5 stops it");
            }
        }
        // Pressing it again while the compiler is still going is somebody
        // wondering whether it heard them, not somebody asking for a second
        // build of the same file.
        if self.after_build.is_some() {
            return self.say("still building — it starts when that finishes");
        }
        // A compiled language is built first. There is nothing for `gdb` to
        // open until `cc` has run, and an editor that knows how to start a
        // debugger but not how to make the thing it debugs has left the
        // interesting half in another window. See [`AfterBuild`].
        if self.start_building(AfterBuild::Debug) != Building::NotAThing {
            return;
        }
        self.start_debugging();
    }

    /// Build the file in front of you, and nothing more.
    ///
    /// The same build F5 runs, asked for on its own — which is what you want
    /// while you are still fixing the things that stop it compiling, and do
    /// not want a debugger started every time one of them is fixed.
    pub(super) fn build(&mut self) {
        let language = lang::get(self.here().language).name.clone();
        if self.start_building(AfterBuild::Nothing) == Building::NotAThing {
            self.say(format!("nothing installed here knows how to build {language}"));
        }
    }

    /// What to say about a build that failed in a project that brought a build
    /// of its own.
    ///
    /// The one-file compile textfold ships is right for one file and cannot be
    /// right for anything else: a project with headers in a directory of their
    /// own needs an include path it has no way to guess, and a project of nine
    /// files needs the other eight named. Both come out as a compiler error
    /// that is perfectly true and says nothing about what to do.
    ///
    /// A `Makefile` sitting in the root is the project having already answered
    /// the question. Saying so is not textfold deciding how your project is
    /// built — it does not run the thing, and it stays out of the way of a
    /// build that worked. It is the difference between a wall and a fixable
    /// problem, which is the same reason a debug adapter has a `see`.
    pub(super) fn build_note(&self, tool: &'static Tool, doc: DocId) -> Option<String> {
        // Already pointed at one. Somebody whose `make` is failing does not
        // need to be told about `make`.
        const THEIRS: [&str; 6] = ["make", "gmake", "ninja", "cmake", "meson", "bazel"];
        let program = Path::new(&tool.command).file_stem()?.to_string_lossy();
        if THEIRS.contains(&program.as_ref()) {
            return None;
        }
        let path = self.doc(doc)?.path.clone()?;
        let root = self.root_for(&path, &tool.roots);
        // The names that mean "this project has already said how it is built".
        const KNOWN: [&str; 6] = [
            "Makefile",
            "makefile",
            "GNUmakefile",
            "CMakeLists.txt",
            "meson.build",
            "build.ninja",
        ];
        let found = KNOWN.into_iter().find(|name| root.join(name).is_file())?;
        let plugin = tool.id.split('/').next().unwrap_or(&tool.id);
        // The directory's own name rather than its path relative to the
        // project, which is empty when they are the same directory — and
        // naming it is worth a word: which directory the build decided it was
        // in is exactly what is surprising when it picked the wrong one.
        let here = root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());
        let makefile = found.eq_ignore_ascii_case("makefile") || found == "GNUmakefile";
        // The `command` a Makefile deserves is `make`, because `make` with no
        // arguments is what a Makefile means. For the rest it is left blank:
        // `cmake --build build` and `ninja -C out` are guesses about somebody
        // else's layout, and a wrong command confidently offered is worse than
        // a blank somebody fills in.
        let command = match makefile {
            true => r#""command": "make", "args": []"#,
            false => r#""command": "…", "args": ["…"]"#,
        };
        Some(format!(
            "{here} has a {found}, and this compiled one file with {}. To build it the way \
             the project does: {{\"tools\": {{\"{}\": {{{command}}}}}}} in your settings \
             for the {plugin} plugin.",
            tool.command, tool.name
        ))
    }

    /// Everything the last build printed, in a buffer of its own.
    ///
    /// The backstop behind the margin, and the answer to "it failed and I
    /// cannot see why". The margin holds what a compiler said *about a line of
    /// a file you have open*, which is most of what it says and never all of
    /// it — a linker error names no line, `make` names no file, and a mistake
    /// in a header you have not opened has nowhere to be drawn. This is the
    /// unabridged version, and it is a buffer because searching, scrolling and
    /// copying out of one is already what buffers do.
    pub(super) fn show_build_output(&mut self) {
        let Some(built) = &self.last_build else {
            return self.say("nothing has been built yet");
        };
        let (name, ok) = (built.name.clone(), built.ok);
        // The editor's own line goes after everything the compiler said,
        // separated and marked, so that "everything it printed" stays exactly
        // that and the way out is on the screen with it.
        let text = match &built.note {
            Some(note) => format!("{}\n\ntextfold: {note}\n", built.text.trim_end()),
            None => built.text.clone(),
        };
        if built.text.trim().is_empty() && built.note.is_none() {
            return self.say(match ok {
                true => format!("{name} finished without a word, which is a compiler working"),
                false => format!("{name} failed and printed nothing at all"),
            });
        }
        self.show_in_a_buffer(&format!("{name} output"), &text);
    }

    /// Where something the editor runs on this file should be run.
    ///
    /// The markers first, exactly as [`lang::project_root`] reads them — a
    /// `Makefile`, a `Cargo.toml`, a `.git`. What is different is what happens
    /// when none of them is there. `project_root` falls back to the directory
    /// the file happens to sit in, which is the only answer it can give: it is
    /// handed a path and knows nothing else.
    ///
    /// The editor knows something else. It was opened on a directory, and that
    /// directory is the project as far as everything else here is concerned —
    /// it is where the file picker looks and what a project-wide search
    /// searches. A build run in `src/` because the project has no `.git` in it
    /// is a `make` with no `Makefile` in front of it, and the answer was
    /// sitting one level up the whole time.
    ///
    /// Only for a file inside it. A file opened from somewhere else entirely
    /// is not part of this project, and running its build in this project's
    /// directory would be a worse guess than the directory it lives in.
    pub(super) fn root_for(&self, path: &Path, markers: &[String]) -> PathBuf {
        if let Some(marked) = lang::marked_root(path, markers) {
            return marked;
        }
        match path.starts_with(&self.project) {
            true => self.project.clone(),
            false => lang::project_root(path, markers),
        }
    }

    /// The tool that turns a file of this language into something that can be
    /// run, where there is one.
    ///
    /// An ordinary tool with one flag on it, found the way everything else
    /// finds a command: in the one registry that has both what textfold ships
    /// and what the plugins brought. Which means a build is switched off,
    /// renamed, given different flags or bound to a key by exactly the
    /// machinery that already does those things for a formatter.
    pub(super) fn build_for(&self, language: &str) -> Option<&'static Tool> {
        crate::cmd::all()
            .iter()
            .filter_map(|cmd| cmd.tool())
            .find(|tool| tool.builds && tool.wants(language))
    }

    /// Start the build for the file in front of you. See [`Building`], which
    /// is the whole of what the answer means.
    pub(super) fn start_building(&mut self, then: AfterBuild) -> Building {
        let doc = self.here();
        let (id, modified, saved) = (doc.id, doc.is_modified(), doc.path.is_some());
        let language = lang::get(doc.language).name.clone();
        let Some(tool) = self.build_for(&language) else {
            return Building::NotAThing;
        };
        if !saved {
            self.say(format!("save this to a file first — {} needs one", tool.name));
            return Building::Refused;
        }
        // The compiler reads the file on disk, exactly as the debugger runs
        // the file on disk. Compiling yesterday's text and then debugging it
        // is the same lost afternoon twice over.
        if modified {
            self.save(None);
        }
        self.after_build = Some(then);
        // `start_tool` has already said why — that `cc` is not installed, most
        // likely, which is a sentence somebody can act on.
        if !self.start_tool(tool, id) {
            self.after_build = None;
            return Building::Refused;
        }
        self.say(format!("{}…", tool.name));
        Building::Started
    }

    /// Run the file in front of you under a debug adapter.
    pub(super) fn start_debugging(&mut self) {
        let doc = self.here();
        let Some(path) = doc.path.clone() else {
            return self.say("save this to a file first — there is nothing to run yet");
        };
        let language = doc.language;
        let modified = doc.is_modified();
        let adapters = lang::get(language).debuggers.clone();
        if adapters.is_empty() {
            let name = lang::get(language).name.clone();
            return self.say(format!("nothing installed here knows how to debug {name}"));
        }
        // The adapter runs the file on disk, not the buffer. Saving first
        // beats debugging yesterday's code and wondering why the breakpoint
        // is in the wrong place — which is the single commonest way to lose
        // twenty minutes to a debugger.
        if modified {
            self.save(None);
        }

        // The first one that starts, in the order the manifests wrote them.
        // More than one is ordinary — an adapter that launches a program and
        // one that attaches to a running process are two — and which of them
        // is installed on this machine is not something the person pressing
        // the key should have to know.
        let mut why_not = None;
        for config in &adapters {
            let root = self.root_for(&path, &config.roots);
            // An adapter that lives inside a language server is asked for
            // rather than started, and the answer comes back later — see
            // [`App::ask_server_for_adapter`], which finishes the job when it
            // does.
            if config.started_by().is_some() {
                match self.ask_server_for_adapter(config, &root, &path) {
                    Ok(()) => return,
                    Err(why) => {
                        why_not = Some(why);
                        continue;
                    }
                }
            }
            // The same interpreter the language servers were pointed at,
            // including one chosen by hand — a debugger running under a
            // different Python from the type checker is a debugger that will
            // disagree with your editor about what is installed.
            let environment = self.lsp.environments.get(&root).cloned();
            match self.debug.start(config, &root, &path, environment.as_deref()) {
                Ok(()) => return self.debugging_now(config, &path),
                // The last word rather than the first: the last adapter tried
                // is the one whose absence is worth reporting, and a list of
                // every one that is not installed is not a message anybody
                // reads.
                Err(why) => why_not = Some(why),
            }
        }
        if let Some(why) = why_not {
            self.say_bad(why);
        }
    }

    /// Offer the programs that are running, to attach the debugger to one.
    ///
    /// A debugger that can only run programs it started itself is half a
    /// debugger. The bugs worth a debugger are very often in something that
    /// has been up for hours — a server holding a connection, a simulation
    /// four hours in — and the whole point of attaching is not having to
    /// reproduce that from the beginning.
    ///
    /// A list, because a process id is a number that means nothing to a person
    /// and is different every time. Writing one into a settings file is the
    /// interface this replaces, and it was one that had to be redone after
    /// every restart of the thing being debugged.
    pub(super) fn open_attach_picker(&mut self) {
        let doc = self.here();
        let language = doc.language;
        let path = doc.path.clone();
        let name = lang::get(language).name.clone();
        if !lang::get(language).debuggers.iter().any(|d| d.can_attach()) {
            return self.say(format!(
                "nothing installed here knows how to attach to a running {name} program"
            ));
        }
        // An adapter that attaches to a *port* has nothing to pick out of a
        // list: one program is waiting on it, and a hundred and fifty
        // processes none of which matters is not a choice but a form to get
        // past. What it may want instead is the address, which is a question
        // with a keyboard rather than a list. See [`crate::dap::needs_a_process`].
        let attaches: Vec<&Value> = lang::get(language)
            .debuggers
            .iter()
            .filter_map(|d| d.attach.as_ref())
            .collect();
        if !attaches.iter().copied().any(crate::dap::needs_a_process) {
            if attaches.iter().copied().any(crate::dap::needs_an_address) {
                return self.ask_debug_address();
            }
            return self.attach_with(None, None);
        }
        let running = crate::proc::running();
        if running.is_empty() {
            return self.say("nothing of yours is running to attach to");
        }
        // The project's own binaries first. On a machine with two hundred
        // processes on it, the one you want is nearly always the thing you
        // just built — and the editor knows which those are, so making
        // somebody scroll past `pipewire` to find it would be withholding an
        // answer it already has.
        let root = path.map(|path| {
            let roots: Vec<String> = lang::get(language)
                .debuggers
                .iter()
                .flat_map(|d| d.roots.clone())
                .collect();
            self.root_for(&path, &roots)
        });
        let mine = |process: &crate::proc::Process| {
            root.as_deref().is_some_and(|root| process.is_inside(root))
        };
        let (ours, theirs): (Vec<_>, Vec<_>) =
            running.into_iter().partition(mine);

        let rows: Vec<Row> = ours
            .iter()
            .map(|process| self.process_row(process, true))
            .chain(theirs.iter().map(|process| self.process_row(process, false)))
            .collect();
        self.overlay = Overlay::Picker(Picker::new(Kind::Processes, rows));
    }

    /// One running program, as a row of that list.
    pub(super) fn process_row(&self, process: &crate::proc::Process, ours: bool) -> Row {
        // The whole command line under the name, because three copies of the
        // same program with different arguments is the case this list exists
        // for and the name alone cannot tell them apart.
        let row = Row::new(
            format!("{}  {}", process.name, process.pid),
            Choice::Process(process.pid),
        )
        .detail(process.command.clone());
        match ours {
            true => row.tag("here"),
            false => row,
        }
    }

    /// Attach the debugger to a program that is already running.
    ///
    /// No build first, unlike F5. The program in front of us is *running*; it
    /// was built by whoever started it, and compiling over the top of it now
    /// would at best be pointless and at worst replace the file the symbols
    /// are being read from.
    pub(super) fn attach_to(&mut self, pid: u32) {
        let Some(process) = crate::proc::running().into_iter().find(|p| p.pid == pid) else {
            // Between the list going up and a row being chosen. Rare, and the
            // alternative is attaching to whatever has been given the number
            // since.
            return self.say(format!("process {pid} is not there any more"));
        };
        self.attach_with(Some(process), None);
    }

    /// Ask where to attach, with the last answer for this project already in
    /// the box.
    ///
    /// A port is a number nobody holds in their head, and it is different for
    /// the two JVMs somebody has up. Remembered per project rather than per
    /// plugin, because a port written in a settings file would be the port
    /// every Java project you ever open tries to attach to — which is the same
    /// objection the jdtls plugin already makes about `mainClass`.
    pub(super) fn ask_debug_address(&mut self) {
        let known = self.remembered_address();
        let mut prompt = Prompt::new(PromptKind::DebugAddress);
        prompt.caret = known.chars().count();
        prompt.input = known;
        self.overlay = Overlay::Prompt(prompt);
    }

    /// Where this project was last attached, or where the manifest says to
    /// start looking.
    pub(super) fn remembered_address(&self) -> String {
        let root = self.attach_root();
        if let Some(said) = self
            .config
            .debug_addresses
            .get(&root.display().to_string())
        {
            return said.clone();
        }
        // Nothing remembered, so whatever the adapter says is conventional for
        // it: 5005 for JDWP, 5678 for debugpy. See
        // [`crate::dap::suggested_address`].
        let suggested = lang::get(self.here().language)
            .debuggers
            .iter()
            .filter_map(|d| d.attach.as_ref())
            .find_map(crate::dap::suggested_address);
        match suggested {
            Some((host, port)) => format!("{host}:{port}"),
            None => "127.0.0.1:5005".to_string(),
        }
    }

    pub(super) fn remember_address(&mut self, address: &str) {
        let root = self.attach_root().display().to_string();
        self.config
            .debug_addresses
            .insert(root, address.to_string());
        self.remember_settings();
    }

    /// The project an attach is remembered against.
    pub(super) fn attach_root(&self) -> PathBuf {
        let doc = self.here();
        let roots: Vec<String> = lang::get(doc.language)
            .debuggers
            .iter()
            .flat_map(|d| d.roots.clone())
            .collect();
        match doc.path.clone() {
            Some(path) => self.root_for(&path, &roots),
            None => self.project.clone(),
        }
    }

    /// Attach: to a process that was picked, to an address that was typed, or
    /// to whatever the settings already name.
    pub(super) fn attach_with(
        &mut self,
        process: Option<crate::proc::Process>,
        address: Option<(String, u16)>,
    ) {
        let doc = self.here();
        let language = doc.language;
        let path = doc
            .path
            .clone()
            // Attaching does not need a file of yours at all — the program is
            // the thing. A buffer with no path still has a project to be in.
            .unwrap_or_else(|| self.project.join("."));

        let adapters: Vec<lang::Debugger> = lang::get(language)
            .debuggers
            .iter()
            .filter(|config| config.can_attach())
            .cloned()
            .collect();
        let mut why_not = None;
        for config in &adapters {
            let root = self.root_for(&path, &config.roots);
            // The attach request, with the process in it, put where a launch
            // would go. Everything after this is the debugger it always was —
            // the same `initialize`, the same breakpoints, the same panel, and
            // the same two ways of getting hold of an adapter.
            let mut config = config.clone();
            let Some(attach) = config.attach.clone() else {
                continue;
            };
            let attach = match &address {
                Some((host, port)) => crate::dap::at_address(&attach, host, *port),
                None => attach,
            };
            config.launch = match &process {
                Some(process) => {
                    crate::dap::about_process(&attach, process.pid, process.program.as_deref())
                }
                None => attach,
            };
            // An adapter that lives inside a language server is asked for
            // rather than started. Java's debugger attaches to a JVM over a
            // port, and it is the same question as launching one — see
            // [`App::ask_server_for_adapter`], which finishes the job.
            if config.started_by().is_some() {
                match self.ask_server_for_adapter(&config, &root, &path) {
                    Ok(()) => return,
                    Err(why) => {
                        why_not = Some(why);
                        continue;
                    }
                }
            }
            let environment = self.lsp.environments.get(&root).cloned();
            match self.debug.start(&config, &root, &path, environment.as_deref()) {
                Ok(()) => {
                    self.say_good(match (&process, &address) {
                        (Some(process), _) => format!(
                            "{}: attached to {} ({})",
                            config.name, process.name, process.pid
                        ),
                        (None, Some((host, port))) => {
                            format!("{}: attaching to {host}:{port}", config.name)
                        }
                        (None, None) => format!("{}: attaching", config.name),
                    });
                    return self.open_debug_panel();
                }
                Err(why) => why_not = Some(why),
            }
        }
        match why_not {
            Some(why) => self.say_bad(why),
            None => self.say("nothing here can attach to a running program"),
        }
    }

    /// Say a session has begun, and put the panel where it will be filled.
    pub(super) fn debugging_now(&mut self, config: &lang::Debugger, path: &Path) {
        let what = short(path, &self.project);
        self.say_good(format!("{}: debugging {what}", config.name));
        self.open_debug_panel();
    }

    /// Ask a language server to start a debug adapter for us.
    ///
    /// Java's adapter is a plugin to the Java language server rather than a
    /// program — see [`crate::lang::FromServer`] — so starting one is a
    /// question rather than a `spawn`, and the answer arrives on the same
    /// channel as everything else a server says.
    pub(super) fn ask_server_for_adapter(
        &mut self,
        config: &lang::Debugger,
        root: &Path,
        path: &Path,
    ) -> Result<(), String> {
        // Filled here rather than in the debugger, because what goes to the
        // server has `${…}` in it too — the file it is being asked about.
        let environment = self.lsp.environments.get(root).cloned();
        let config = crate::dap::filled(config, root, path, environment.as_deref());
        let from = config.started_by().ok_or("no server named")?.clone();
        let doc = self.here();
        let Some(server) = self.lsp.named(&from.server, doc) else {
            return Err(format!(
                "{} debugs through {}, and that is not running here — {}",
                config.name,
                from.server,
                match config.see.as_deref() {
                    Some(see) => see.to_string(),
                    None => format!("install the {} plugin", from.server),
                }
            ));
        };

        // Two questions where there is something to resolve, one where there
        // is not. The first fills in what only the server knows; the second
        // asks for the adapter itself.
        let (command, arguments, ask) = match &from.resolve {
            Some(resolve) => (
                resolve.command.clone(),
                resolve.arguments.clone(),
                Ask::DebugLaunch {
                    config: Box::new(config.clone()),
                    root: root.to_path_buf(),
                    file: path.to_path_buf(),
                },
            ),
            None => (
                from.start.clone(),
                Vec::new(),
                Ask::DebugAdapter {
                    config: Box::new(config.clone()),
                    root: root.to_path_buf(),
                    file: path.to_path_buf(),
                },
            ),
        };
        if !self
            .lsp
            .start_debug_session(server, &command, &arguments, ask)
        {
            return Err(format!(
                "{} is still starting — it has to read the project before it \
                 can debug it",
                from.server
            ));
        }
        // Not `say_good`: nothing is debugging yet, and a session that never
        // arrives should not have said it had.
        self.say(format!("asking {} for a debug session…", from.server));
        Ok(())
    }

    /// The server has answered the question the adapter needed answering.
    ///
    /// Its answer goes into the launch arguments by the names the manifest
    /// gave, and then the adapter itself is asked for. Nothing here knows
    /// what a classpath is — only that a field of an answer was to be put in
    /// a field of a launch.
    pub(super) fn take_debug_launch(
        &mut self,
        server: ServerId,
        mut config: crate::lang::Debugger,
        root: PathBuf,
        file: PathBuf,
        answer: Value,
    ) {
        let Some(from) = config.from_server.clone() else {
            return;
        };
        if let Some(resolve) = &from.resolve {
            crate::dap::fold_into_launch(&mut config.launch, resolve, &answer);
        }
        let ask = Ask::DebugAdapter {
            config: Box::new(config),
            root,
            file,
        };
        if !self
            .lsp
            .start_debug_session(server, &from.start, &[], ask)
        {
            self.say_bad(format!("{} would not start a debug session", from.server));
        }
    }

    /// A language server answered with the port its adapter is listening on.
    pub(super) fn take_debug_adapter(
        &mut self,
        config: crate::lang::Debugger,
        root: PathBuf,
        file: PathBuf,
        result: Value,
    ) {
        // Every adapter that works this way answers with a bare number. One
        // that answers with something else has not started anything, and
        // guessing a port would be connecting to whatever happens to be
        // listening on it.
        let port = result
            .as_u64()
            .or_else(|| result.get("port").and_then(Value::as_u64))
            .and_then(|port| u16::try_from(port).ok());
        let Some(port) = port.filter(|port| *port != 0) else {
            return self.say_bad(format!(
                "{} would not say which port to debug on",
                config.name
            ));
        };
        let environment = self.lsp.environments.get(&root).cloned();
        match self
            .debug
            .connect(&config, &root, &file, environment.as_deref(), port)
        {
            Ok(()) => self.debugging_now(&config, &file),
            Err(why) => self.say_bad(why),
        }
    }

    /// Stop the program and the adapter with it.
    pub(super) fn stop_debugging(&mut self) {
        // Including one that has not started yet. "Stop" pressed while the
        // compiler is running has to mean the debugger is not coming, or it
        // arrives half a minute later on top of whatever you moved on to.
        if self.after_build == Some(AfterBuild::Debug) {
            self.after_build = None;
            return self.say("the build finishes, and nothing is debugged");
        }
        if self.debug.session().is_none() {
            return self.say("nothing is being debugged");
        }
        self.debug.stop();
        self.refresh_debug_panel();
        self.say("stopped debugging");
    }

    pub(super) fn debug_step(&mut self, what: crate::dap::Step) {
        if !self.debug.session().is_some_and(|s| s.state.is_stopped()) {
            return self.say("nothing is stopped — F5 starts it");
        }
        self.debug.step(what);
        self.refresh_debug_panel();
    }

    /// Ask for an expression, and work it out where the program is stopped.
    ///
    /// The word selected, if there is one, since asking what the thing you
    /// have highlighted comes to is most of what this is for.
    pub(super) fn ask_debug_evaluate(&mut self) {
        if !self.debug.session().is_some_and(|s| s.state.is_stopped()) {
            return self.say("nothing is stopped, so there is nowhere to work it out");
        }
        let selected = {
            let (doc, view) = (self.here(), self.view());
            let range = view.sel.primary();
            match range.is_empty() {
                true => text::word_text_at(&doc.rope, range.head),
                false => Some(doc.rope.slice(range.start()..range.end()).to_string()),
            }
        };
        let mut prompt = Prompt::new(PromptKind::DebugEvaluate);
        if let Some(word) = selected {
            prompt.caret = word.chars().count();
            prompt.input = word;
        }
        self.overlay = Overlay::Prompt(prompt);
    }

    pub(super) fn debug_pause(&mut self) {
        match self.debug.session() {
            Some(session) if !session.state.is_over() => {
                self.debug.pause();
                self.say("asked it to stop");
            }
            _ => self.say("nothing is running"),
        }
    }

    /// Put a breakpoint on the line the cursor is on, or take it off.
    pub(super) fn toggle_breakpoint(&mut self) {
        let at = self.view().cursor();
        let id = self.view().doc;
        let Some(doc) = self.doc_mut(id) else { return };
        if doc.panel.is_some() {
            return;
        }
        let line = crate::text::line_of(&doc.rope, at);
        let on = doc.toggle_breakpoint(line);
        self.tell_debugger_about_breakpoints();
        self.say(match on {
            true => format!("breakpoint on line {}", line + 1),
            false => format!("breakpoint off line {}", line + 1),
        });
    }

    /// Start remembering what you do, or stop and keep it.
    ///
    /// One macro, not a keyboard full of them. The overwhelming majority of
    /// what anybody records is "this, and now the same thing forty more
    /// times", recorded and played within the minute — and a register to name
    /// is a thing to remember for a macro that will not outlive the hour.
    pub(super) fn record_macro(&mut self) {
        match self.recorder.stop() {
            None => {
                self.recorder.start();
                self.say("recording — run it again to stop");
            }
            Some(0) => self.say("nothing was recorded"),
            Some(n) => self.say_good(format!("{} recorded", count("step", n))),
        }
    }

    /// Whether something is being recorded, for the status bar to say so. A
    /// recorder nobody can see running is a recorder somebody left on.
    pub fn is_recording(&self) -> bool {
        self.recorder.on()
    }

    /// Do it all again.
    pub(super) fn play_macro(&mut self) {
        if self.recorder.on() {
            // Playing what is still being recorded would record what it
            // played, and the recording would grow while it ran.
            return self.say("still recording — run record-macro to stop");
        }
        if self.recorder.kept().is_empty() {
            return self.say("nothing recorded to play");
        }
        if self.recorder.playing {
            return;
        }
        self.recorder.playing = true;
        for step in self.recorder.kept().to_vec() {
            match step {
                Recorded::Did(cmd) => self.run(cmd),
                Recorded::Typed(c) => self.type_char(c),
            }
        }
        self.recorder.playing = false;
    }

    /// Put back the stretch of this file the cursor is standing in, as it was
    /// committed.
    ///
    /// An ordinary edit, so it is one thing to undo and the margin, the
    /// language servers and everything else hear about it the way they hear
    /// about typing. Nothing is written to the disk and nothing is written to
    /// the repository: this is a change to your buffer that happens to be
    /// shaped like the last commit.
    pub(super) fn revert_hunk(&mut self) {
        let id = self.view().doc;
        let at = self.view().cursor();
        let Some(doc) = self.doc(id) else { return };
        let line = text::line_of(&doc.rope, at);
        let Some(base) = self.git.baseline(id).map(str::to_string) else {
            return self.say("git has not seen this file");
        };
        let now = doc.text();
        let Some(hunk) = crate::git::hunks(&base, &now)
            .into_iter()
            .find(|hunk| hunk.holds(line))
        else {
            return self.say("nothing has changed on this line");
        };

        // What it was, as text. A hunk with no old lines is one that is
        // entirely new, and putting it back means taking it away.
        let was: String = base
            .lines()
            .skip(hunk.was.start)
            .take(hunk.was.len())
            .map(|row| format!("{row}\n"))
            .collect();

        let Some(doc) = self.doc(id) else { return };
        let from = text::line_start(&doc.rope, hunk.lines.start.min(doc.len_lines() - 1));
        let to = match hunk.lines.end < doc.len_lines() {
            true => text::line_start(&doc.rope, hunk.lines.end),
            false => doc.len_chars(),
        };
        let from = match hunk.lines.is_empty() {
            // A deletion has nowhere of its own; the lines go back in above
            // the line that took their place.
            true => text::line_start(&doc.rope, hunk.lines.start.min(doc.len_lines() - 1)),
            false => from,
        };
        let to = match hunk.lines.is_empty() {
            true => from,
            false => to,
        };

        let sel = Selections::single(Range::point(from));
        let Some(doc) = self.doc_mut(id) else { return };
        let edits = doc.apply_atomic(vec![crate::doc::Change::replace(from, to, was)], &sel);
        // `None`, because no pane has taken these edits in yet: this one was
        // made against the document rather than through a pane's selection,
        // and a pane told it has already absorbed an edit keeps a cursor
        // pointing at text that has moved.
        self.after_edit_to(id, edits, None);
        self.say_good("put back as it was committed");
    }

    /// Put the stretch of this file the cursor is standing in into the index.
    ///
    /// The one thing textfold does that changes a repository, and it does it
    /// by handing git the patch for that hunk — so what lands in the index is
    /// git's own doing, and `git diff --cached` afterwards says exactly what
    /// you just looked at.
    pub(super) fn stage_hunk(&mut self) {
        let id = self.view().doc;
        let at = self.view().cursor();
        let Some(doc) = self.doc(id) else { return };
        let line = text::line_of(&doc.rope, at);
        let Some(path) = doc.path.clone() else {
            return self.say("this buffer has no file to stage");
        };
        let now = doc.text();
        let Some(base) = self.git.baseline(id).map(str::to_string) else {
            return self.say("git has not seen this file");
        };
        let Some(repo) = self.git.repo() else {
            return self.say("not in a repository");
        };
        let Some(name) = repo.relative(&path) else {
            return self.say("that file is not in this repository");
        };
        let Some(hunk) = crate::git::hunks(&base, &now)
            .into_iter()
            .find(|hunk| hunk.holds(line))
        else {
            return self.say("nothing has changed on this line");
        };
        let patch = crate::git::patch_for(&name, &base, &now, &hunk);
        match repo.stage(&path, &patch) {
            Ok(()) => self.say_good(format!(
                "staged {} of {name}",
                count("line", hunk.lines.len())
            )),
            // Git's own words. "does not apply" nearly always means the index
            // already has part of this file in it, and paraphrasing that would
            // only make it harder to look up.
            Err(why) => self.say_bad(why),
        }
    }

    /// Who last touched this line, and when, and why.
    ///
    /// On a thread, because `git blame` on a large file in a large repository
    /// is not instant and an editor that stops answering the keyboard to
    /// answer a question about one line has answered the wrong question.
    pub(super) fn blame_line(&mut self) {
        let at = self.view().cursor();
        let Some(doc) = self.doc(self.view().doc) else {
            return;
        };
        let line = text::line_of(&doc.rope, at);
        let Some(path) = doc.path.clone() else {
            return self.say("this buffer has no file to blame");
        };
        let Some(repo) = self.git.repo().cloned() else {
            return self.say("not in a repository");
        };
        let tx = self.tx.clone();
        self.say("asking git…");
        std::thread::Builder::new()
            .name("blame".into())
            .spawn(move || {
                let said = repo
                    .blame(&path, line)
                    .unwrap_or_else(|| "git has nothing to say about that line".into());
                tx.send(Event::Blamed(said)).ok();
            })
            .ok();
    }

    /// To the next place git could not merge on its own, or the one before.
    pub(super) fn conflict_step(&mut self, forwards: bool) {
        let at = self.view().cursor();
        let doc = self.here();
        let line = text::line_of(&doc.rope, at);
        let found = crate::git::conflicts(&doc.text());
        if found.is_empty() {
            return self.say("no conflict markers in this file");
        }
        let to = match forwards {
            true => found.iter().find(|c| c.start > line).or_else(|| found.first()),
            false => found
                .iter()
                .rev()
                .find(|c| c.start < line)
                .or_else(|| found.last()),
        };
        let Some(conflict) = to else { return };
        self.go_to_line(conflict.start);
        self.say(format!(
            "conflict, {} yours and {} theirs",
            count("line", conflict.ours().len()),
            count("line", conflict.theirs().len()),
        ));
    }

    /// Settle the conflict the cursor is in by keeping one side of it and
    /// throwing away the markers.
    pub(super) fn take_side(&mut self, ours: bool) {
        let id = self.view().doc;
        let at = self.view().cursor();
        let Some(doc) = self.doc(id) else { return };
        let line = text::line_of(&doc.rope, at);
        let text = doc.text();
        let Some(conflict) = crate::git::conflicts(&text)
            .into_iter()
            .find(|c| c.start <= line && line <= c.end)
        else {
            return self.say("the cursor is not in a conflict");
        };
        let keep = match ours {
            true => conflict.ours(),
            false => conflict.theirs(),
        };
        let kept: String = text
            .lines()
            .skip(keep.start)
            .take(keep.len())
            .map(|row| format!("{row}\n"))
            .collect();
        let Some(doc) = self.doc(id) else { return };
        let from = text::line_start(&doc.rope, conflict.start);
        let to = match conflict.end + 1 < doc.len_lines() {
            true => text::line_start(&doc.rope, conflict.end + 1),
            false => doc.len_chars(),
        };
        let sel = Selections::single(Range::point(from));
        let Some(doc) = self.doc_mut(id) else { return };
        let edits = doc.apply_atomic(vec![crate::doc::Change::replace(from, to, kept)], &sel);
        self.after_edit_to(id, edits, None);
        self.say_good(match ours {
            true => "kept yours",
            false => "kept theirs",
        });
    }

    /// Mark this line, or take the mark off it.
    pub(super) fn toggle_bookmark(&mut self) {
        let at = self.view().cursor();
        let id = self.view().doc;
        let Some(doc) = self.doc_mut(id) else { return };
        let line = crate::text::line_of(&doc.rope, at);
        let on = doc.toggle_bookmark(line);
        self.say(match on {
            true => format!("bookmarked line {}", line + 1),
            false => format!("bookmark off line {}", line + 1),
        });
    }

    /// To the next bookmark in this file, or the one before.
    pub(super) fn bookmark_step(&mut self, forwards: bool) {
        let at = self.view().cursor();
        let doc = self.here();
        let line = crate::text::line_of(&doc.rope, at);
        let Some(to) = doc.bookmark_from(line, forwards) else {
            return self.say("no bookmarks in this file");
        };
        self.go_to_line(to);
    }

    /// Take every bookmark in this file away.
    pub(super) fn clear_bookmarks_here(&mut self) {
        let id = self.view().doc;
        let name = self.here().name.clone();
        let Some(doc) = self.doc_mut(id) else { return };
        let had = doc.bookmarks.len();
        doc.bookmarks.clear();
        match had {
            0 => self.say(format!("no bookmarks in {name}")),
            n => self.say_good(format!("{} gone", count("bookmark", n))),
        }
    }

    /// Every bookmark in every open buffer, as a list.
    ///
    /// Across buffers rather than in this one, because "where was that" is
    /// nearly always a question about the last hour rather than about the file
    /// in front of you — and the list says which file each one is in.
    pub(super) fn open_bookmarks_picker(&mut self) {
        let here = self.view().doc;
        let mut rows: Vec<Row> = Vec::new();
        for doc in &self.docs {
            for line in doc.bookmark_lines() {
                let text = doc
                    .rope
                    .get_line(line)
                    .map(|l| l.to_string())
                    .unwrap_or_default();
                let label = text.trim().to_string();
                let where_ = match &doc.path {
                    Some(path) if doc.id != here => {
                        format!("{}:{}", short(path, &self.project), line + 1)
                    }
                    _ => format!("line {}", line + 1),
                };
                let choice = match (&doc.path, doc.id == here) {
                    (_, true) => Choice::Here(crate::text::line_start(&doc.rope, line)),
                    (Some(path), false) => Choice::There {
                        path: path.clone(),
                        line,
                        column: 0,
                    },
                    (None, false) => Choice::Buffer(doc.id),
                };
                // A blank line can be bookmarked, and a row with nothing in it
                // is a row you cannot pick out of a list.
                let label = match label.is_empty() {
                    true => where_.clone(),
                    false => label,
                };
                rows.push(Row::new(label, choice).detail(where_).tag(doc.name.clone()));
            }
        }
        if rows.is_empty() {
            return self.say("nothing is bookmarked");
        }
        self.overlay = Overlay::Picker(Picker::new(Kind::Bookmarks, rows));
    }

    /// Take every breakpoint in this file away.
    ///
    /// The one you want nine times out of ten: you were debugging one thing,
    /// you put six of them in while working out what it did, and you are done
    /// with that file rather than with debugging.
    pub(super) fn clear_breakpoints_here(&mut self) {
        let id = self.view().doc;
        let name = self.here().name.clone();
        let Some(doc) = self.doc_mut(id) else { return };
        let had = doc.breakpoints.len();
        doc.breakpoints.clear();
        self.tell_debugger_about_breakpoints();
        self.say(match had {
            0 => format!("there were none in {name}"),
            1 => format!("the breakpoint in {name} is gone"),
            n => format!("all {n} breakpoints in {name} are gone"),
        });
    }

    /// Take every breakpoint in every open buffer away. The way out of having
    /// twenty of them and no memory of where.
    pub(super) fn clear_all_breakpoints(&mut self) {
        let had: usize = self.docs.iter().map(|d| d.breakpoints.len()).sum();
        let files = self
            .docs
            .iter()
            .filter(|d| !d.breakpoints.is_empty())
            .count();
        for doc in &mut self.docs {
            doc.breakpoints.clear();
        }
        self.tell_debugger_about_breakpoints();
        self.say(match (had, files) {
            (0, _) => "there were none".to_string(),
            (1, _) => "the breakpoint is gone".to_string(),
            (n, 1) => format!("all {n} breakpoints are gone"),
            // How many files it reached across, because "all 14 breakpoints
            // are gone" after you meant to clear one file is a thing you want
            // to find out now rather than the next time you run.
            (n, files) => format!("all {n} breakpoints across {files} files are gone"),
        });
    }

    /// Where the breakpoints are now, per file, for the adapter.
    ///
    /// Only buffers with a file behind them: a breakpoint in an unsaved
    /// scratch buffer has no path to name it by, and an adapter told about a
    /// file that does not exist answers with an error rather than ignoring it.
    pub(super) fn breakpoints_now(&self) -> Vec<(PathBuf, Vec<usize>)> {
        self.docs
            .iter()
            .filter(|d| d.panel.is_none())
            .filter_map(|d| Some((d.path.clone()?, d.breakpoint_lines())))
            .filter(|(_, lines)| !lines.is_empty())
            .collect()
    }

    pub(super) fn tell_debugger_about_breakpoints(&mut self) {
        if !self.debug.is_running() {
            return;
        }
        let where_ = self.breakpoints_now();
        self.debug.send_breakpoints(&where_);
    }

    /// Everything a debug adapter says.
    pub(super) fn on_dap(&mut self, id: crate::dap::SessionId, message: Incoming) {
        let where_ = self.breakpoints_now();
        match self.debug.on(id, message, &where_) {
            crate::dap::Change::Stopped => {
                self.show_where_it_stopped();
                self.refresh_debug_panel();
            }
            crate::dap::Change::Resumed => self.refresh_debug_panel(),
            crate::dap::Change::Ended => {
                self.refresh_debug_panel();
                let Some(session) = self.debug.session() else {
                    return;
                };
                // "The program finished" is the wrong thing to say about a
                // debugger that never ran one — and it is the *worst* wrong
                // thing, because it sounds like it worked. What the adapter
                // printed on its way out is the only account of why there is.
                if !session.ever_started() {
                    let name = session.name.clone();
                    let why = session.why_not();
                    self.open_debug_panel();
                    return match why {
                        Some(why) => self.say_bad(format!(
                            "{name} would not start: {}",
                            crate::text::truncate(&why, 90)
                        )),
                        None => self.say_bad(format!("{name} would not start")),
                    };
                }
                let why = session.state.label().to_string();
                self.say(format!("the program {why}"));
            }
            crate::dap::Change::Nothing => self.refresh_debug_panel(),
        }
        for problem in std::mem::take(&mut self.debug.problems) {
            self.say_bad(problem);
        }
    }

    /// Open the file the program stopped in and put the cursor on the line.
    ///
    /// Into a pane that is not the panel and not a dock, because the whole
    /// point is that you can see the code — landing in the sidebar would be
    /// the debugger stopping somewhere you cannot read.
    pub(super) fn show_where_it_stopped(&mut self) {
        let Some((path, line, column)) = self.debug.session().and_then(|s| s.here()) else {
            return;
        };
        let panel = self.debug_panel;
        // Never into the panel itself. `open_path` picks a pane for the file,
        // and the pane it should not pick is the one the stack is drawn in.
        if self.panes.get(self.focus).map(|p| p.doc) == panel
            && let Some(at) = self.beside_the_docks()
        {
            self.focus = at;
        }
        self.open_path(&path);
        self.go_to(line, column);
    }

    /// Which file and line the program is stopped at, for the margin.
    pub fn stopped_at(&self) -> Option<(&Path, usize)> {
        let session = self.debug.session()?;
        if !session.state.is_stopped() {
            return None;
        }
        let frame = session.selected()?;
        Some((frame.path.as_deref()?, frame.line))
    }

    // ---- The panel ----

    /// Show the debugger's panel along the bottom, or put it away.
    pub(super) fn toggle_debug_panel(&mut self) {
        match self.debug_panel {
            Some(id) if self.pane_showing_docked(id).is_some() => {
                self.close_debug_panel();
            }
            _ => {
                self.open_debug_panel_taking_focus(true);
                if self.debug.session().is_none() {
                    self.say("nothing is being debugged yet — F5 starts it");
                }
            }
        }
    }

    /// Take the panel off the bottom of the screen.
    ///
    /// The buffer goes with the pane. A plugin's panel keeps its buffer when
    /// its sidebar closes, because what is in it came from the plugin and
    /// asking for it again would mean asking the plugin again — but this one
    /// is drawn from the session every time anything changes, so there is
    /// nothing in it to keep. And a kept one is worse than nothing: with no
    /// dock to live in it becomes an ordinary buffer, which means a tab
    /// called `Debug` in the row at the top, which is not a thing anybody
    /// asked for.
    pub(super) fn close_debug_panel(&mut self) {
        let Some(id) = self.debug_panel.take() else {
            return;
        };
        self.close_doc(id);
    }

    /// Make the panel if there is not one, and put it back if it was closed.
    ///
    /// Without taking the keyboard. Opening a sidebar puts the cursor in it,
    /// which is right when you asked for the sidebar and wrong when it opened
    /// because you pressed F5 — pressing "run" should not move your cursor out
    /// of your code.
    pub(super) fn open_debug_panel(&mut self) {
        self.open_debug_panel_taking_focus(false);
    }

    pub(super) fn open_debug_panel_taking_focus(&mut self, take_focus: bool) {
        let was = self.focus;
        let id = match self.debug_panel.filter(|id| self.doc(*id).is_some()) {
            Some(id) => id,
            None => {
                let id = self.new_id();
                let mut doc = Document::scratch(id, "Debug".into(), self.default_indent());
                // A panel is not yours to type into, and saying so here means
                // every key that would have changed the text is free for the
                // panel to use — the same bargain a plugin's panel makes.
                doc.read_only = true;
                doc.panel = Some(crate::doc::Panel {
                    owner: crate::doc::Owner::Debugger,
                    id: "debug".into(),
                    spans: Vec::new(),
                    actions: Vec::new(),
                });
                doc.mark_saved();
                self.docs.push(doc);
                self.debug_panel = Some(id);
                id
            }
        };
        if self.pane_showing_docked(id).is_none() {
            let edge = crate::view::Edge::Bottom;
            self.dock_panel(id, crate::view::Dock::new(edge, Some(DEBUG_PANEL_ROWS)));
            // Folded, unlike an ordinary pane. Most of what goes in here is a
            // line of prose — an adapter's complaint, a Python traceback, the
            // repr of something big — and a panel fourteen rows tall that cuts
            // the interesting half off the right-hand edge is a panel that
            // makes you go and read the log anyway.
            if let Some(at) = self.pane_showing_docked(id) {
                self.panes[at].wrap = true;
            }
            if !take_focus {
                // The panel is added at the end, so nothing before it was
                // renumbered and the pane that had the keyboard still has its
                // place in the list.
                self.focus = was.min(self.panes.len().saturating_sub(1));
            }
        }
        self.refresh_debug_panel();
    }

    /// Draw the session into the panel: where it is, how it got there, what
    /// the values are, and what it printed.
    ///
    /// Rebuilt whole every time anything changes, which is what the panel
    /// machinery is built for — see [`App::write_panel`]. Cheap, and it means
    /// there is exactly one function that decides what the panel says.
    pub(super) fn refresh_debug_panel(&mut self) {
        let Some(id) = self.debug_panel.filter(|id| self.doc(*id).is_some()) else {
            return;
        };
        let lines = self.debug_panel_lines();
        self.write_panel(id, &lines);
    }

    pub(super) fn debug_panel_lines(&self) -> Vec<Value> {
        let mut lines: Vec<Value> = Vec::new();
        let plain = |text: &str, style: &str| json!({ "spans": [{ "text": text, "style": style }] });

        let Some(session) = self.debug.session() else {
            lines.push(self.debug_buttons(None));
            lines.push(json!(""));
            match (self.after_build, &self.last_build) {
                // A compile of one file is quick; a project's build is not,
                // and a panel that says "nothing is being debugged" for half a
                // minute after F5 reads as a key that did nothing.
                (Some(AfterBuild::Debug), _) => {
                    lines.push(plain("Building, and then debugging it.", "muted"));
                }
                // The commonest reason there is no session: the thing being
                // debugged was never made. Saying "nothing is being debugged"
                // here is true and is not the answer — the answer is what the
                // compiler said, and the way to the whole of it.
                (_, Some(built)) if !built.ok => {
                    lines.push(json!({ "spans": [
                        { "text": format!("{} failed, so there is nothing to debug.", built.name),
                          "style": "warning" },
                        { "text": "   see what it printed", "style": "muted",
                          "action": "do:build-output" },
                    ]}));
                }
                _ => lines.push(plain("Nothing is being debugged.", "muted")),
            }
            lines.push(json!(""));
            let key = self.keys.shortcut(Cmd::DEBUG).unwrap_or_else(|| "F5".into());
            lines.push(plain(
                &format!("{key} runs the file you are looking at under a debugger."),
                "muted",
            ));
            let key = self
                .keys
                .shortcut(Cmd::TOGGLE_BREAKPOINT)
                .unwrap_or_else(|| "F9".into());
            lines.push(plain(
                &format!("{key} puts a breakpoint on the line the cursor is on."),
                "muted",
            ));
            return lines;
        };

        // The buttons first, so they are in the same place whatever the
        // session is doing. A row that moves down the panel as the stack gets
        // deeper is a row you have to look for.
        lines.push(self.debug_buttons(Some(session)));
        lines.push(json!(""));

        // The headline: what is being debugged and what it is doing.
        let tone = match &session.state {
            crate::dap::State::Stopped(_) => "warning",
            crate::dap::State::Ended(_) => "muted",
            _ => "string",
        };
        lines.push(json!({ "spans": [
            { "text": format!("{} ", session.name), "style": "function" },
            { "text": format!("{} ", session.what), "style": "muted" },
            { "text": session.state.label(), "style": tone },
        ]}));

        // The stack, innermost first, each row a place you can go.
        if !session.frames.is_empty() {
            lines.push(json!(""));
            lines.push(plain("Where it is", "keyword"));
            for frame in &session.frames {
                let here = session.frame == Some(frame.id);
                let place = match &frame.path {
                    Some(path) => format!(
                        "{}:{}",
                        short(path, &self.project),
                        frame.line + 1
                    ),
                    None => "no source".to_string(),
                };
                lines.push(json!({ "spans": [
                    { "text": if here { "▸ " } else { "  " }, "style": "warning" },
                    { "text": frame.name.clone(),
                      "style": if here { "function" } else { "variable" },
                      "action": format!("frame:{}", frame.id) },
                    { "text": format!("  {place}"), "style": "muted",
                      "action": format!("frame:{}", frame.id) },
                ]}));
            }
        }

        // The values in view at the frame being looked at.
        for scope in &session.scopes {
            let open = session.open.contains(&scope.reference);
            lines.push(json!(""));
            lines.push(json!({ "spans": [
                { "text": if open { "▾ " } else { "▸ " }, "style": "muted",
                  "action": format!("open:{}", scope.reference) },
                { "text": scope.name.clone(), "style": "keyword",
                  "action": format!("open:{}", scope.reference) },
            ]}));
            if open {
                append_values(&mut lines, session, scope.reference, 1);
            }
        }

        // The threads, under the values rather than over them.
        //
        // Only when there is more than one, because a list of one thread
        // called `MainThread` is a row of noise. And below, because a JVM has
        // six before your program has done anything — `Reference Handler`,
        // `Finalizer`, `Signal Dispatcher` — and six rows of threads nobody
        // asked about between the stack and the values is the values off the
        // bottom of the panel.
        if session.threads.len() > 1 {
            lines.push(json!(""));
            lines.push(plain("Threads", "keyword"));
            for thread in &session.threads {
                let here = session.thread == Some(thread.id);
                lines.push(json!({ "spans": [
                    { "text": if here { "▸ " } else { "  " }, "style": "warning" },
                    { "text": thread.name.clone(),
                      "style": if here { "function" } else { "variable" } },
                ]}));
            }
        }

        // What the program printed, last first in the sense that the end of
        // the list is what is on screen — a panel scrolled to the top showing
        // the first thing a program ever said is showing the wrong end.
        if !session.output.is_empty() {
            lines.push(json!(""));
            // A debugger that never ran anything has printed nothing of your
            // program's, so calling this "what it printed" would be pointing
            // at somebody else's error message and calling it your output.
            let printed = session.has_printed();
            let mut heading = vec![json!({
                "text": match printed {
                    true => "What it printed",
                    false => "What went wrong",
                },
                "style": "keyword",
            })];
            // The panel keeps the last few dozen lines; the session keeps
            // several hundred. A program that printed more than fits here is
            // exactly the program whose output somebody needs to read, and
            // scrolling a panel fourteen rows tall is not reading.
            if printed {
                heading.push(json!({
                    "text": "   see all of it",
                    "style": "muted",
                    "action": "do:output",
                }));
            }
            lines.push(json!({ "spans": heading }));
            for line in session.output.iter().rev().take(OUTPUT_SHOWN).rev() {
                // Coloured by who said it, which is the whole reason the two
                // are told apart: your program's output reads as output, a
                // traceback stands out in the middle of it, and the editor's
                // own account of the run stays quietly in the background
                // rather than looking like something you printed.
                let style = match line.from {
                    crate::dap::Printer::Err => "warning",
                    crate::dap::Printer::Out => "string",
                    crate::dap::Printer::Note => "muted",
                };
                lines.push(plain(&format!("  {}", line.text), style));
            }
        }
        lines
    }

    /// The row of buttons along the top of the panel.
    ///
    /// Not a second way of doing what F10 does. F10 is for somebody who
    /// already knows it is F10, and a debugger is very often the first thing
    /// in an editor that somebody uses before they have learned any of its
    /// keys — so the panel that is already on the screen showing where the
    /// program stopped is the obvious place to say what can be done about it.
    ///
    /// Everything is always drawn, and what cannot be done now is drawn
    /// without an action on it. A row whose buttons come and go is a row you
    /// have to read every time; one whose buttons grey out can be aimed at
    /// from memory, and says what the states of a debugger *are*.
    pub(super) fn debug_buttons(&self, session: Option<&crate::dap::Session>) -> Value {
        let state = session.map(|s| &s.state);
        let stopped = state.is_some_and(|s| s.is_stopped());
        let running = state.is_some_and(|s| !s.is_stopped() && !s.is_over());
        let alive = stopped || running;
        // A build that a debugger is waiting on is a run that has begun, as
        // far as the buttons are concerned: there is nothing to start and
        // there is something to call off.
        let coming = self.after_build == Some(AfterBuild::Debug);

        let mut spans: Vec<Value> = Vec::new();
        let mut button = |label: &str, action: &str, on: bool| {
            spans.push(json!({
                "text": format!(" {label} "),
                "style": if on { "function" } else { "muted" },
                "action": on.then(|| format!("do:{action}")),
            }));
            spans.push(json!({ "text": " ", "style": "muted" }));
        };

        match alive {
            // "Carry on" and "start it" are the same key and the same button,
            // for the same reason: there is never a moment where both
            // readings are available. See [`App::debug`].
            true => button("▶ Continue", "start", stopped),
            false => button("▶ Start", "start", !coming),
        }
        button("❚❚ Pause", "pause", running);
        button("↷ Over", "over", stopped);
        button("↓ Into", "into", stopped);
        button("↑ Out", "out", stopped);
        button("■ Stop", "stop", alive || coming);
        // Attaching, where the language has an adapter that can. Only while
        // nothing is running, because it is another way of *starting* — and a
        // row that offered it mid-session would be offering to throw the
        // session away.
        let can_attach = self.the_code().is_some_and(|doc| {
            lang::get(doc.language).debuggers.iter().any(|d| d.can_attach())
        });
        if !alive && !coming && can_attach {
            button("⚯ Attach", "attach", true);
        }
        // Not part of the run, and last: the build is what you press when the
        // program is not the thing that needs fixing yet.
        //
        // The language of your *code*, not of the panel. Clicking in the panel
        // puts the keyboard in a scratch buffer that is no language at all,
        // and a button that disappeared when you clicked near it would be a
        // button nobody could hit twice.
        let language = self
            .the_code()
            .map(|doc| lang::get(doc.language).name.clone())
            .unwrap_or_default();
        if let Some(build) = self.build_for(&language) {
            button(&format!("⚒ {}", build.name), "build", true);
        }
        json!({ "spans": spans })
    }

    /// The buffer somebody is actually working in, as against a panel they
    /// have clicked in. `None` only where every pane on the screen is a dock,
    /// which is a screen with nowhere to put a file.
    pub(super) fn the_code(&self) -> Option<&Document> {
        let at = self.beside_the_docks()?;
        self.doc(self.panes.get(at)?.doc)
    }

    /// Everything the program printed, in a buffer of its own.
    ///
    /// A buffer rather than a bigger panel, because what somebody wants to do
    /// with a thousand lines of output is search it, scroll it and copy out of
    /// it — and a buffer is already the thing in this editor that does all
    /// three. The panel keeps showing the last of it either way.
    pub(super) fn show_program_output(&mut self) {
        let Some(session) = self.debug.session() else {
            return self.say("nothing is being debugged");
        };
        let (text, what, ran) = (
            session.program_printed(),
            session.what.clone(),
            session.ever_started(),
        );
        if text.trim().is_empty() {
            return self.say(match ran {
                true => "it has not printed anything".to_string(),
                false => "nothing ran, so nothing printed — the panel says why".to_string(),
            });
        }
        self.show_in_a_buffer(&format!("{what} output"), &text);
    }

    /// Something in the panel was pressed or clicked.
    pub(super) fn debug_action(&mut self, action: &str) {
        let Some((what, rest)) = action.split_once(':') else {
            return;
        };
        if what == "do" {
            return self.debug_button(rest);
        }
        let Ok(number) = rest.parse::<i64>() else {
            return;
        };
        match what {
            // A frame: go and look at it, and show its values.
            "frame" => {
                self.debug.select_frame(number);
                self.show_where_it_stopped();
                self.refresh_debug_panel();
            }
            // A structured value: open it up, or fold it away.
            "open" => {
                self.debug.toggle_value(number);
                self.refresh_debug_panel();
            }
            _ => {}
        }
    }

    /// One of the buttons along the top of the panel.
    ///
    /// Each is the command of the same name and nothing else, so there is one
    /// account of what "step over" means and the button and the key cannot
    /// drift apart.
    pub(super) fn debug_button(&mut self, what: &str) {
        // Clicking in the panel puts the keyboard in the panel, and the panel
        // is a buffer with no file in it. "Run this file" asked there is a
        // question about the wrong buffer — so the two buttons that are about
        // your code send the keyboard back to your code first, which is where
        // it wanted to be anyway.
        if matches!(what, "start" | "build" | "attach")
            && self.here().panel.is_some()
            && let Some(at) = self.beside_the_docks()
        {
            self.focus = at;
        }
        match what {
            "start" => self.debug(),
            "pause" => self.debug_pause(),
            "over" => self.debug_step(crate::dap::Step::Over),
            "into" => self.debug_step(crate::dap::Step::Into),
            "out" => self.debug_step(crate::dap::Step::Out),
            "stop" => self.stop_debugging(),
            "build" => self.build(),
            "attach" => self.open_attach_picker(),
            "output" => self.show_program_output(),
            "build-output" => self.show_build_output(),
            _ => {}
        }
        self.refresh_debug_panel();
    }
}

/// How many rows the debugger's panel gets when it opens. Enough for a stack,
/// a set of locals and a few lines of output without covering the code.
pub(crate) const DEBUG_PANEL_ROWS: u16 = 14;

/// How much of a program's output the panel shows at once. The rest is still
/// kept — see [`crate::dap`] — and scrolling is what the panel is for.
pub(crate) const OUTPUT_SHOWN: usize = 60;

/// One level of variables, and whatever has been opened up under them.
///
/// The depth limit is not tidiness. The tree being walked is the *program's*,
/// and a program can perfectly well hold a list that holds itself — which
/// without a limit is a panel that never finishes drawing.
pub(crate) fn append_values(
    lines: &mut Vec<Value>,
    session: &crate::dap::Session,
    reference: i64,
    depth: usize,
) {
    const DEEPEST: usize = 6;
    if depth > DEEPEST {
        return;
    }
    let Some(values) = session.values.get(&reference) else {
        // Asked for and not back yet. Saying so beats a gap that looks like
        // an empty scope.
        lines.push(json!({ "spans": [
            { "text": format!("{}…", "  ".repeat(depth)), "style": "muted" },
        ]}));
        return;
    };
    for value in values {
        let pad = "  ".repeat(depth);
        let open = value.reference != 0 && session.open.contains(&value.reference);
        let arrow = match (value.reference != 0, open) {
            (false, _) => "  ",
            (true, false) => "▸ ",
            (true, true) => "▾ ",
        };
        let action = (value.reference != 0).then(|| format!("open:{}", value.reference));
        let mut spans = vec![
            json!({ "text": format!("{pad}{arrow}"), "style": "muted" }),
            json!({ "text": value.name.clone(), "style": "property", "action": action }),
        ];
        // Nothing after the name where there is nothing to say. `debugpy`
        // gathers the dunders of a frame under a row called `special
        // variables` with no value of its own, and `special variables = `
        // reads as a variable whose value is missing rather than as a heading.
        if !value.value.is_empty() {
            spans.push(json!({ "text": " = ", "style": "operator" }));
            spans.push(json!({ "text": value.value.clone(), "style": "string", "action": action }));
        }
        if let Some(kind) = &value.kind {
            spans.push(json!({ "text": format!("  {kind}"), "style": "type" }));
        }
        lines.push(json!({ "spans": spans }));
        if open {
            append_values(lines, session, value.reference, depth + 1);
        }
    }
}
