//! Output abstraction for human-friendly vs machine-readable (porcelain) modes.
//!
//! In porcelain mode, all output is plain text with tab-separated fields:
//! - Status lines: `status\t<key>\t<value>`
//! - Data lines: `<field1>\t<field2>\t...`
//! - No spinners, progress bars, ANSI colors, or box-drawing characters
//!
//! # Verbosity levels
//!
//! Independent of format (interactive vs porcelain), output volume is
//! controlled by [`Verbosity`]:
//!
//! | Level    | Spinners | Detail | Summaries | Errors | Prompts      |
//! |----------|----------|--------|-----------|--------|--------------|
//! | Normal   | yes      | yes    | yes       | yes    | interactive  |
//! | Quiet    | no       | no     | yes       | yes    | auto-accept  |
//! | Silent   | no       | no     | no        | stderr | auto-accept  |
//!
//! When combined with `--porcelain`, the highest suppression wins:
//! `--porcelain --silent` produces no output at all (check exit code).

/// How much output to produce.
///
/// Ordered from most verbose to least. When multiple flags are set,
/// the highest suppression level wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Verbosity {
    /// Full output (default).
    Normal,
    /// Suppress spinners, progress bars, and per-item detail.
    /// Final summary lines and errors are still shown.
    Quiet,
    /// Suppress everything except errors (printed to stderr).
    Silent,
}

/// Output mode controller.
///
/// Passed to every command to control whether output is human-friendly
/// (cliclack spinners, colors, box drawing) or machine-readable (tab-separated).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Output {
    porcelain: bool,
    verbosity: Verbosity,
}

impl Output {
    pub(crate) const fn new(porcelain: bool, verbosity: Verbosity) -> Self {
        Self {
            porcelain,
            verbosity,
        }
    }

    pub(crate) const fn is_porcelain(self) -> bool {
        self.porcelain
    }

    pub(crate) const fn is_quiet(self) -> bool {
        matches!(self.verbosity, Verbosity::Quiet | Verbosity::Silent)
    }

    pub(crate) const fn is_silent(self) -> bool {
        matches!(self.verbosity, Verbosity::Silent)
    }

    /// Whether non-interactive mode is active (porcelain, quiet, or silent).
    pub(crate) const fn is_non_interactive(self) -> bool {
        self.porcelain || self.is_quiet()
    }

    // -- Lifecycle (intro/outro) --

    /// Print a command header.
    ///
    /// Suppressed in quiet/silent modes and porcelain.
    pub(crate) fn intro(self, title: &str) -> eyre::Result<()> {
        if !self.porcelain && !self.is_quiet() {
            cliclack::intro(title)?;
        }
        Ok(())
    }

    /// Print a command footer.
    ///
    /// In quiet mode, printed as a plain summary line.
    /// Suppressed in silent mode and porcelain.
    pub(crate) fn outro(self, msg: &str) -> eyre::Result<()> {
        if self.is_silent() || self.porcelain {
            return Ok(());
        }
        if self.is_quiet() {
            if !msg.is_empty() {
                println!("{msg}");
            }
        } else {
            cliclack::outro(msg)?;
        }
        Ok(())
    }

    // -- Logging --

    /// Log a success message.
    ///
    /// Suppressed in quiet and silent modes.
    pub(crate) fn success(self, msg: &str) -> eyre::Result<()> {
        if self.is_quiet() {
            return Ok(());
        }
        if self.porcelain {
            println!("ok\t{msg}");
        } else {
            cliclack::log::success(msg)?;
        }
        Ok(())
    }

    /// Log an error message.
    ///
    /// Always shown. In silent mode, printed to stderr.
    pub(crate) fn error(self, msg: &str) -> eyre::Result<()> {
        if self.is_silent() {
            eprintln!("error: {msg}");
            return Ok(());
        }
        if self.porcelain {
            println!("error\t{msg}");
        } else {
            cliclack::log::error(msg)?;
        }
        Ok(())
    }

    /// Log a warning message.
    ///
    /// In silent mode, printed to stderr. Suppressed in quiet mode.
    pub(crate) fn warning(self, msg: &str) -> eyre::Result<()> {
        if self.is_silent() {
            eprintln!("warning: {msg}");
            return Ok(());
        }
        if self.is_quiet() {
            return Ok(());
        }
        if self.porcelain {
            println!("warning\t{msg}");
        } else {
            cliclack::log::warning(msg)?;
        }
        Ok(())
    }

    /// Log an informational message.
    ///
    /// Suppressed in quiet and silent modes.
    pub(crate) fn info(self, msg: &str) -> eyre::Result<()> {
        if self.is_quiet() {
            return Ok(());
        }
        if self.porcelain {
            println!("info\t{msg}");
        } else {
            cliclack::log::info(msg)?;
        }
        Ok(())
    }

    /// Log a low-priority remark.
    ///
    /// Suppressed in quiet and silent modes.
    pub(crate) fn remark(self, msg: &str) -> eyre::Result<()> {
        if self.is_quiet() {
            return Ok(());
        }
        if self.porcelain {
            // Remarks are low-priority; still emit them for completeness
            println!("info\t{msg}");
        } else {
            cliclack::log::remark(msg)?;
        }
        Ok(())
    }

    /// Log a final summary line.
    ///
    /// Visible in quiet mode (this is _the_ line quiet mode exists to show).
    /// Suppressed only in silent mode.
    pub(crate) fn summary(self, msg: &str) -> eyre::Result<()> {
        if self.is_silent() {
            return Ok(());
        }
        if self.porcelain {
            println!("ok\t{msg}");
        } else if self.is_quiet() {
            println!("{msg}");
        } else {
            cliclack::log::success(msg)?;
        }
        Ok(())
    }

    // -- Structured data --

    /// Print a tab-separated data line (porcelain) or a note block (human).
    ///
    /// Suppressed in quiet and silent modes.
    #[allow(dead_code)]
    pub(crate) fn note(self, title: &str, content: &str) -> eyre::Result<()> {
        if self.is_quiet() {
            return Ok(());
        }
        if self.porcelain {
            // Emit each line of content prefixed with the title as context
            for line in content.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() {
                    println!("{title}\t{trimmed}");
                }
            }
        } else {
            cliclack::note(title, content)?;
        }
        Ok(())
    }

    /// Print a single key-value pair.
    ///
    /// Suppressed in quiet and silent modes.
    pub(crate) fn kv(self, key: &str, value: &str) -> eyre::Result<()> {
        if self.is_quiet() {
            return Ok(());
        }
        if self.porcelain {
            println!("{key}\t{value}");
        } else {
            cliclack::log::info(format!("{key}: {value}"))?;
        }
        Ok(())
    }

    /// Print a raw data line (porcelain only). No-op in human mode.
    ///
    /// Suppressed in silent mode.
    #[allow(dead_code)]
    pub(crate) fn data(self, line: &str) {
        if self.is_silent() {
            return;
        }
        if self.porcelain {
            println!("{line}");
        }
    }

    // -- Detail output (per-file streaming lines) --

    /// Print a per-item detail line (e.g., file created/modified in watch).
    ///
    /// Suppressed in quiet and silent modes.
    pub(crate) fn detail(self, line: &str) {
        if self.is_quiet() {
            return;
        }
        println!("{line}");
    }

    /// Print a per-item detail line in porcelain format.
    ///
    /// Suppressed in silent mode.
    pub(crate) fn detail_porcelain(self, line: &str) {
        if self.is_silent() {
            return;
        }
        if self.porcelain {
            println!("{line}");
        }
    }

    // -- Spinners --

    /// Start a spinner (human) or print a status message (porcelain).
    /// Returns a `Spinner` handle that can be stopped.
    ///
    /// In quiet/silent modes, returns a no-op spinner.
    pub(crate) fn spinner(self, msg: &str) -> Spinner {
        if self.is_quiet() {
            return Spinner::Suppressed;
        }
        if self.porcelain {
            println!("info\t{msg}");
            Spinner::Porcelain
        } else {
            let s = cliclack::spinner();
            s.start(msg);
            Spinner::Interactive(s)
        }
    }

    // -- Progress bars --

    /// Start a progress bar (human) or return a no-op counter (porcelain).
    ///
    /// In quiet/silent modes, returns a no-op progress bar.
    pub(crate) fn progress(self, total: u64, msg: &str) -> Progress {
        if self.is_quiet() {
            return Progress::Suppressed;
        }
        if self.porcelain {
            println!("progress\t{msg}\t{total}");
            Progress::Porcelain
        } else {
            let pb = cliclack::progress_bar(total);
            pb.start(msg);
            Progress::Interactive(pb)
        }
    }

    // -- Confirm --

    /// Ask a yes/no question.
    ///
    /// In porcelain, quiet, or silent mode, returns the default value.
    pub(crate) fn confirm(self, question: &str, default: bool) -> eyre::Result<bool> {
        if self.is_non_interactive() {
            Ok(default)
        } else {
            Ok(cliclack::confirm(question)
                .initial_value(default)
                .interact()?)
        }
    }

    // -- Select --

    /// Prompt the user to select from a list of options.
    ///
    /// Each item is `(value, label, hint)`.
    /// In porcelain, quiet, or silent mode, returns the first item.
    ///
    /// # Errors
    ///
    /// Returns an error if the interactive prompt fails.
    #[cfg(feature = "iroh")]
    pub(crate) fn select<T: Clone + Eq>(
        self,
        prompt: &str,
        items: &[(T, &str, &str)],
    ) -> eyre::Result<T> {
        if self.is_non_interactive() || items.is_empty() {
            return items
                .first()
                .map(|(v, _, _)| v.clone())
                .ok_or_else(|| eyre::eyre!("no items to select from"));
        }

        let mut builder = cliclack::select(prompt);
        for (value, label, hint) in items {
            builder = builder.item(value.clone(), label, hint);
        }
        let result: T = builder.interact()?;
        Ok(result)
    }

    // -- Text input --

    /// Prompt for text input.
    ///
    /// In porcelain, quiet, or silent mode, returns the default or empty string.
    pub(crate) fn input(
        self,
        prompt: &str,
        placeholder: &str,
        default: Option<&str>,
    ) -> eyre::Result<String> {
        if self.is_non_interactive() {
            return Ok(default.unwrap_or("").to_string());
        }

        let mut builder = cliclack::input(prompt).placeholder(placeholder);
        if let Some(d) = default {
            builder = builder.default_input(d);
        }
        let result: String = builder.interact()?;
        Ok(result)
    }
}

/// Spinner abstraction: wraps `cliclack::ProgressBar`, is a porcelain stub,
/// or is fully suppressed (quiet/silent).
pub(crate) enum Spinner {
    Interactive(cliclack::ProgressBar),
    Porcelain,
    Suppressed,
}

impl Spinner {
    pub(crate) fn stop(&self, msg: impl std::fmt::Display) {
        match self {
            Spinner::Interactive(s) => s.stop(msg),
            Spinner::Porcelain => println!("ok\t{msg}"),
            Spinner::Suppressed => {}
        }
    }

    pub(crate) fn clear(&self) {
        match self {
            Spinner::Interactive(s) => s.clear(),
            Spinner::Porcelain | Spinner::Suppressed => {}
        }
    }

    #[allow(dead_code)]
    pub(crate) fn set_message(&self, msg: impl std::fmt::Display) {
        match self {
            Spinner::Interactive(s) => s.set_message(msg),
            Spinner::Porcelain | Spinner::Suppressed => {}
        }
    }
}

/// Progress bar abstraction: wraps `cliclack::ProgressBar`, is a porcelain stub,
/// or is fully suppressed (quiet/silent).
pub(crate) enum Progress {
    Interactive(cliclack::ProgressBar),
    Porcelain,
    Suppressed,
}

impl Progress {
    pub(crate) fn inc(&self, n: u64) {
        match self {
            Progress::Interactive(pb) => pb.inc(n),
            Progress::Porcelain | Progress::Suppressed => {}
        }
    }

    pub(crate) fn set_message(&self, msg: impl std::fmt::Display) {
        match self {
            Progress::Interactive(pb) => pb.set_message(msg),
            Progress::Porcelain | Progress::Suppressed => {}
        }
    }

    pub(crate) fn set_length(&self, len: u64) {
        match self {
            Progress::Interactive(pb) => pb.set_length(len),
            Progress::Porcelain | Progress::Suppressed => {}
        }
    }

    pub(crate) fn stop(&self, msg: impl std::fmt::Display) {
        match self {
            Progress::Interactive(pb) => pb.stop(msg),
            Progress::Porcelain => println!("ok\t{msg}"),
            Progress::Suppressed => {}
        }
    }
}
