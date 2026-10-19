//! `IOInterface`: console output with Composer's `<info>`-style tags and verbosity levels.

use std::io::{IsTerminal, Write};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verbosity {
    Quiet,
    Normal,
    Verbose,
    VeryVerbose,
    Debug,
}

/// Output sink used everywhere. Messages may contain `<info>`, `<comment>`, `<warning>`,
/// `<error>`, `<question>`, `<highlight>` tags, rendered as colors or stripped.
pub struct Io {
    verbosity: Verbosity,
    decorated: bool,
    interactive: bool,
    lock: Mutex<()>,
    /// `BufferIO`: collect stdout and stderr output instead of printing it
    buffer: Option<Mutex<String>>,
    /// leave `<warning>` and `<highlight>` as they are (see [`Io::buffer_io`])
    symfony_styles_only: bool,
}

impl Io {
    pub fn new(verbosity: Verbosity, decorated: Option<bool>, interactive: bool) -> Io {
        let decorated = decorated.unwrap_or_else(|| std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none());
        Io { verbosity, decorated, interactive, lock: Mutex::new(()), buffer: None, symfony_styles_only: false }
    }

    /// Silent IO for tests and internal use.
    pub fn null() -> Io {
        Io { verbosity: Verbosity::Quiet, decorated: false, interactive: false, lock: Mutex::new(()), buffer: None, symfony_styles_only: false }
    }

    /// `BufferIO`: undecorated, non-interactive, keeps everything written for [`Io::output`].
    pub fn buffered(verbosity: Verbosity) -> Io {
        Io { buffer: Some(Mutex::new(String::new())), ..Io::new(verbosity, Some(false), false) }
    }

    /// Composer's tests' `new BufferIO('', $verbosity, new OutputFormatter(false))`: a plain formatter
    /// only knows Symfony's styles, so Composer's `<warning>` and `<highlight>` stay in the output.
    pub fn buffer_io(verbosity: Verbosity) -> Io {
        Io { symfony_styles_only: true, ..Io::buffered(verbosity) }
    }

    /// What a buffered IO collected.
    pub fn output(&self) -> String {
        self.buffer.as_ref().map(|b| b.lock().unwrap().clone()).unwrap_or_default()
    }

    pub fn verbosity(&self) -> Verbosity {
        self.verbosity
    }

    pub fn is_verbose(&self) -> bool {
        self.verbosity >= Verbosity::Verbose
    }

    pub fn is_very_verbose(&self) -> bool {
        self.verbosity >= Verbosity::VeryVerbose
    }

    pub fn is_debug(&self) -> bool {
        self.verbosity >= Verbosity::Debug
    }

    pub fn is_decorated(&self) -> bool {
        self.decorated
    }

    pub fn is_interactive(&self) -> bool {
        self.interactive
    }

    /// `write()`: stdout at normal verbosity.
    pub fn write(&self, msg: &str) {
        self.write_at(msg, Verbosity::Normal)
    }

    pub fn write_at(&self, msg: &str, verbosity: Verbosity) {
        if self.shows(verbosity) {
            self.emit(msg, false);
        }
    }

    /// `writeError()`: stderr at normal verbosity (Composer's progress output goes here).
    pub fn write_error(&self, msg: &str) {
        self.write_error_at(msg, Verbosity::Normal)
    }

    pub fn write_error_at(&self, msg: &str, verbosity: Verbosity) {
        if self.shows(verbosity) {
            self.emit(msg, true);
        }
    }

    fn emit(&self, msg: &str, stderr: bool) {
        let line = self.format(msg);
        if let Some(buffer) = &self.buffer {
            let mut b = buffer.lock().unwrap();
            b.push_str(&line);
            b.push('\n');
            return;
        }
        let _guard = self.lock.lock();
        let _ = if stderr { writeln!(std::io::stderr(), "{line}") } else { writeln!(std::io::stdout(), "{line}") };
    }

    /// `ask()`: prints the question on stderr and reads a trimmed line from stdin; the default
    /// when the answer is empty, stdin is closed or the IO is not interactive.
    pub fn ask(&self, question: &str, default: Option<&str>) -> Option<String> {
        if !self.interactive {
            return default.map(String::from);
        }
        let _ = write!(std::io::stderr(), "{}", self.format(question));
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        match std::io::stdin().read_line(&mut answer) {
            Ok(n) if n > 0 && !answer.trim().is_empty() => Some(answer.trim().to_string()),
            _ => default.map(String::from),
        }
    }

    /// `askConfirmation()`: answers starting with `y` are yes, an empty answer is the default.
    pub fn ask_confirmation(&self, question: &str, default: bool) -> bool {
        match self.ask(question, None) {
            Some(answer) => answer.to_lowercase().starts_with('y'),
            None => default,
        }
    }

    /// `<warning>` line on stderr.
    pub fn warning(&self, msg: &str) {
        self.write_error(&format!("<warning>{msg}</warning>"))
    }

    /// `<error>` line on stderr, shown even when quiet.
    pub fn error(&self, msg: &str) {
        self.write_error_at(&format!("<error>{msg}</error>"), Verbosity::Quiet)
    }

    fn shows(&self, verbosity: Verbosity) -> bool {
        verbosity == Verbosity::Quiet || (self.verbosity != Verbosity::Quiet && verbosity <= self.verbosity)
    }

    /// Renders or strips the tags.
    pub fn format(&self, msg: &str) -> String {
        const TAGS: [(&str, &str); 6] = [
            ("info", "\x1b[32m"),
            ("comment", "\x1b[33m"),
            ("warning", "\x1b[30;43m"),
            ("error", "\x1b[37;41m"),
            ("question", "\x1b[30;46m"),
            ("highlight", "\x1b[31m"),
        ];
        let mut out = msg.to_string();
        for (tag, color) in TAGS {
            if self.symfony_styles_only && matches!(tag, "warning" | "highlight") {
                continue;
            }
            let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
            if self.decorated {
                out = out.replace(&open, color).replace(&close, "\x1b[0m");
            } else {
                out = out.replace(&open, "").replace(&close, "");
            }
        }
        // `<href=URL>text</>` links keep only their text; `</>` closes any style
        static HREF: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"<href=[^>]*>").unwrap());
        out = HREF.replace_all(&out, "").into_owned();
        out = out.replace("</>", if self.decorated { "\x1b[0m" } else { "" });
        // `\<` is how literal tags are escaped
        out.replace("\\<", "<")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_tags_when_undecorated() {
        let io = Io::new(Verbosity::Normal, Some(false), false);
        assert_eq!(io.format("<info>Installing</info> <comment>a/b</comment>"), "Installing a/b");
        assert_eq!(io.format("see <href=https://x.org/a>PKSA-1</>"), "see PKSA-1");
    }

    #[test]
    fn buffer_io_keeps_composer_styles() {
        let io = Io::buffer_io(Verbosity::Normal);
        io.warning("<info>Careful</info>");
        assert_eq!(io.output(), "<warning>Careful</warning>\n");
    }
}
