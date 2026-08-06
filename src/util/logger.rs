//! Logging utilities using tracing

use chrono::Local;
use std::fmt;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::{
    fmt::{format::Writer, FmtContext, FormatEvent, FormatFields},
    registry::LookupSpan,
};

#[cfg(windows)]
use windows::Win32::System::Console::{
    GetConsoleMode, GetStdHandle, SetConsoleMode, CONSOLE_MODE, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    STD_OUTPUT_HANDLE,
};

struct CppStyleFormatter;

impl<S, N> FormatEvent<S, N> for CppStyleFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let timestamp = Local::now().format("%H:%M:%S");
        let target = event.metadata().target();

        let (label, color) = if target == "success" {
            ("SUCCESS", "\x1b[92m")
        } else {
            match *event.metadata().level() {
                tracing::Level::ERROR => ("ERROR", "\x1b[91m"),
                tracing::Level::WARN => ("WARN", "\x1b[93m"),
                tracing::Level::INFO => ("INFO", "\x1b[96m"),
                tracing::Level::DEBUG => ("DEBUG", "\x1b[90m"),
                tracing::Level::TRACE => ("TRACE", "\x1b[90m"),
            }
        };

        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);

        let rendered = format!(
            "\x1b[90m[{}]\x1b[0m [{}{}\x1b[0m] {}",
            timestamp, color, label, visitor.message
        );

        let _ = ctx;
        writeln!(writer, "{}", rendered)
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: String,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{:?}", value);
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        }
    }
}

/// Logger initialization
pub struct Logger;

impl Logger {
    /// Initialize the logger with numeric verbosity.
    ///
    /// Levels:
    /// 0 = error, 1 = warn, 2 = info, 3 = debug, 4+ = trace.
    pub fn init_with_verbosity(verbosity: u8) {
        Self::enable_virtual_terminal_processing();
        Self::init_with_level(level_from_verbosity(verbosity));
    }

    /// Initialize with specific level
    fn init_with_level(level: tracing::Level) {
        tracing_subscriber::fmt()
            .event_format(CppStyleFormatter)
            .with_max_level(level)
            .with_env_filter(
                tracing_subscriber::EnvFilter::from_default_env().add_directive(level.into()),
            )
            .init();
    }

    #[cfg(windows)]
    fn enable_virtual_terminal_processing() {
        unsafe {
            if let Ok(handle) = GetStdHandle(STD_OUTPUT_HANDLE) {
                let mut mode = CONSOLE_MODE(0);
                if GetConsoleMode(handle, &mut mode).is_ok() {
                    let _ = SetConsoleMode(
                        handle,
                        CONSOLE_MODE(mode.0 | ENABLE_VIRTUAL_TERMINAL_PROCESSING.0),
                    );
                }
            }
        }
    }

    #[cfg(not(windows))]
    fn enable_virtual_terminal_processing() {}

    pub fn info(message: impl fmt::Display) {
        tracing::info!("{}", message);
    }

    /// Print a multi-line info block without the timestamp/label prefix (keeps INFO color)
    pub fn info_block(message: impl fmt::Display) {
        // INFO color: \x1b[96m
        println!("\x1b[96m{}\x1b[0m", message);
    }

    pub fn warn(message: impl fmt::Display) {
        tracing::warn!("{}", message);
    }

    pub fn error(message: impl fmt::Display) {
        tracing::error!("{}", message);
    }

    pub fn debug(message: impl fmt::Display) {
        tracing::debug!("{}", message);
    }

    /// Emit a SUCCESS-level message in the same style as the C++ logger.
    pub fn success(message: impl fmt::Display) {
        tracing::event!(target: "success", tracing::Level::INFO, "{}", message);
    }

    pub fn section(title: impl fmt::Display) {
        tracing::info!("\n=== {} ===", title);
    }

    pub fn status(label: impl fmt::Display, value: impl fmt::Display) {
        tracing::info!("{:>16}: {}", label, value);
    }
}

/// Initialize logging with numeric verbosity.
pub fn init_with_verbosity(verbosity: u8) {
    Logger::init_with_verbosity(verbosity);
}

fn level_from_verbosity(verbosity: u8) -> tracing::Level {
    match verbosity {
        0 => tracing::Level::ERROR,
        1 => tracing::Level::WARN,
        2 => tracing::Level::INFO,
        3 => tracing::Level::DEBUG,
        _ => tracing::Level::TRACE,
    }
}
