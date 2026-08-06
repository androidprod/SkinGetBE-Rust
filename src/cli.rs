//! Command-line interface definitions for SkinGetBE

use clap::Parser;

/// Minecraft Bedrock Edition skin data extraction tool.
#[derive(Debug, Parser)]
#[command(name = "SkinGetBE", version, about)]
pub struct Cli {
    /// Enable loading version/protocol from config.jsonc (default; kept for CLI compatibility).
    #[arg(short, long, action = clap::ArgAction::SetTrue)]
    pub config: bool,

    /// Filter displayed players by name (substring match).
    #[arg(long)]
    pub filter: Option<String>,

    /// Set log level: 0=error, 1=warn, 2=info, 3=debug, 4=trace.
    /// Without a value this enables debug verbosity (3).
    #[arg(
        long,
        value_name = "level",
        num_args = 0..=1,
        default_missing_value = "3",
        value_parser = parse_log_level,
    )]
    pub logs: Option<u8>,

    /// Backward-compatible alias for --logs 3 (debug verbosity).
    #[arg(short = 'd', long, action = clap::ArgAction::SetTrue)]
    pub debug: bool,
}

impl Cli {
    /// Resolve the effective numeric log verbosity (0-4).
    pub fn log_level(&self) -> u8 {
        if let Some(level) = self.logs {
            return level;
        }
        if self.debug {
            return 3;
        }
        2
    }
}

fn parse_log_level(value: &str) -> Result<u8, String> {
    match value.parse::<u8>() {
        Ok(level @ 0..=4) => Ok(level),
        _ => Err(format!(
            "Invalid --logs level: {}. Expected 0, 1, 2, 3, or 4.",
            value
        )),
    }
}

/// Rewrite Windows-style `/flag` arguments to clap-compatible `--flag` form.
pub fn normalize_args<I>(args: I) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    args.into_iter()
        .map(|arg| {
            if arg.starts_with('/') && arg.len() > 1 {
                format!("--{}", &arg[1..])
            } else {
                arg
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_logs_level_and_filter_arguments() {
        let cli = Cli::try_parse_from(["skingetbe", "--logs", "3", "--filter", "Steve"])
            .expect("parse should succeed");

        assert_eq!(cli.log_level(), 3);
        assert_eq!(cli.filter.as_deref(), Some("Steve"));
    }

    #[test]
    fn logs_without_level_enables_debug_verbosity() {
        let cli = Cli::try_parse_from(["skingetbe", "--logs"]).expect("parse should succeed");

        assert_eq!(cli.log_level(), 3);
    }

    #[test]
    fn debug_alias_maps_to_debug_verbosity() {
        let cli = Cli::try_parse_from(["skingetbe", "--debug"]).expect("parse should succeed");

        assert_eq!(cli.log_level(), 3);
    }

    #[test]
    fn defaults_to_info_verbosity() {
        let cli = Cli::try_parse_from(["skingetbe"]).expect("parse should succeed");

        assert_eq!(cli.log_level(), 2);
    }

    #[test]
    fn rejects_invalid_logs_level() {
        let error = Cli::try_parse_from(["skingetbe", "--logs", "9"])
            .expect_err("parse should fail")
            .to_string();

        assert!(error.contains("Invalid --logs level"));
    }

    #[test]
    fn rejects_missing_filter_value() {
        assert!(Cli::try_parse_from(["skingetbe", "--filter"]).is_err());
    }

    #[test]
    fn normalizes_windows_style_arguments() {
        let args = normalize_args(vec![
            "/help".into(),
            "/logs".into(),
            "3".into(),
            "/filter".into(),
            "Steve".into(),
        ]);

        assert_eq!(
            args,
            vec![
                "--help".to_string(),
                "--logs".to_string(),
                "3".to_string(),
                "--filter".to_string(),
                "Steve".to_string()
            ]
        );
    }
}
