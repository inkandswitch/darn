//! Output abstraction for human-friendly vs machine-readable (porcelain) modes.
//!
//! In porcelain mode, all output is plain text with tab-separated fields:
//! - Status lines: `status\t<key>\t<value>`
//! - Data lines: `<field1>\t<field2>\t...`
//! - No spinners, progress bars, ANSI colors, or box-drawing characters

/// Output mode controller.
///
/// Passed to every command to control whether output is human-friendly
/// (cliclack spinners, colors, box drawing) or machine-readable (tab-separated).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Output {
    porcelain: bool,
}

impl Output {
    pub(crate) fn new(porcelain: bool) -> Self {
        Self { porcelain }
    }

    pub(crate) fn is_porcelain(&self) -> bool {
        self.porcelain
    }

    // -- Lifecycle (intro/outro) --

    /// Print a command header. In porcelain mode, this is a no-op.
    pub(crate) fn intro(&self, title: &str) -> eyre::Result<()> {
        if !self.porcelain {
            cliclack::intro(title)?;
        }
        Ok(())
    }

    /// Print a command footer. In porcelain mode, this is a no-op.
    pub(crate) fn outro(&self, msg: &str) -> eyre::Result<()> {
        if !self.porcelain {
            cliclack::outro(msg)?;
        }
        Ok(())
    }

    // -- Logging --

    pub(crate) fn success(&self, msg: &str) -> eyre::Result<()> {
        if self.porcelain {
            println!("ok\t{msg}");
        } else {
            cliclack::log::success(msg)?;
        }
        Ok(())
    }

    pub(crate) fn error(&self, msg: &str) -> eyre::Result<()> {
        if self.porcelain {
            println!("error\t{msg}");
        } else {
            cliclack::log::error(msg)?;
        }
        Ok(())
    }

    pub(crate) fn warning(&self, msg: &str) -> eyre::Result<()> {
        if self.porcelain {
            println!("warning\t{msg}");
        } else {
            cliclack::log::warning(msg)?;
        }
        Ok(())
    }

    pub(crate) fn info(&self, msg: &str) -> eyre::Result<()> {
        if self.porcelain {
            println!("info\t{msg}");
        } else {
            cliclack::log::info(msg)?;
        }
        Ok(())
    }

    pub(crate) fn remark(&self, msg: &str) -> eyre::Result<()> {
        if self.porcelain {
            // Remarks are low-priority; still emit them for completeness
            println!("info\t{msg}");
        } else {
            cliclack::log::remark(msg)?;
        }
        Ok(())
    }

    // -- Structured data --

    /// Print a tab-separated data line (porcelain) or a note block (human).
    #[allow(dead_code)]
    pub(crate) fn note(&self, title: &str, content: &str) -> eyre::Result<()> {
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

    /// Print a single key-value pair. In human mode, uses cliclack::log::info.
    pub(crate) fn kv(&self, key: &str, value: &str) -> eyre::Result<()> {
        if self.porcelain {
            println!("{key}\t{value}");
        } else {
            cliclack::log::info(format!("{key}: {value}"))?;
        }
        Ok(())
    }

    /// Print a raw data line (porcelain only). No-op in human mode.
    #[allow(dead_code)]
    pub(crate) fn data(&self, line: &str) {
        if self.porcelain {
            println!("{line}");
        }
    }

    // -- Spinners --

    /// Start a spinner (human) or print a status message (porcelain).
    /// Returns a `Spinner` handle that can be stopped.
    pub(crate) fn spinner(&self, msg: &str) -> Spinner {
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
    pub(crate) fn progress(&self, total: u64, msg: &str) -> Progress {
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

    /// Ask a yes/no question. In porcelain mode, returns the default value.
    pub(crate) fn confirm(&self, question: &str, default: bool) -> eyre::Result<bool> {
        if self.porcelain {
            Ok(default)
        } else {
            Ok(cliclack::confirm(question)
                .initial_value(default)
                .interact()?)
        }
    }
}

/// Spinner abstraction: wraps cliclack::ProgressBar or is a no-op.
pub(crate) enum Spinner {
    Interactive(cliclack::ProgressBar),
    Porcelain,
}

impl Spinner {
    pub(crate) fn stop(&self, msg: impl std::fmt::Display) {
        match self {
            Spinner::Interactive(s) => s.stop(msg),
            Spinner::Porcelain => println!("ok\t{msg}"),
        }
    }

    pub(crate) fn clear(&self) {
        match self {
            Spinner::Interactive(s) => s.clear(),
            Spinner::Porcelain => {}
        }
    }

    #[allow(dead_code)]
    pub(crate) fn set_message(&self, msg: impl std::fmt::Display) {
        match self {
            Spinner::Interactive(s) => s.set_message(msg),
            Spinner::Porcelain => {}
        }
    }
}

/// Progress bar abstraction: wraps cliclack::ProgressBar or is a no-op.
pub(crate) enum Progress {
    Interactive(cliclack::ProgressBar),
    Porcelain,
}

impl Progress {
    pub(crate) fn inc(&self, n: u64) {
        match self {
            Progress::Interactive(pb) => pb.inc(n),
            Progress::Porcelain => {}
        }
    }

    pub(crate) fn set_message(&self, msg: impl std::fmt::Display) {
        match self {
            Progress::Interactive(pb) => pb.set_message(msg),
            Progress::Porcelain => {}
        }
    }

    pub(crate) fn set_length(&self, len: u64) {
        match self {
            Progress::Interactive(pb) => pb.set_length(len),
            Progress::Porcelain => {}
        }
    }

    pub(crate) fn stop(&self, msg: impl std::fmt::Display) {
        match self {
            Progress::Interactive(pb) => pb.stop(msg),
            Progress::Porcelain => println!("ok\t{msg}"),
        }
    }
}
