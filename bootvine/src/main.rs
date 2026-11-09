use std::{
    error::Error, fmt::Display, io::ErrorKind, path::PathBuf, process::ExitCode, time::Duration,
};

use bootvine::{
    console_session::{
        resume_console::{close_resume_console, open_resume_console},
        watch_loop::{Settings, run},
    },
    screen_settings::config_file::{
        Config, ConfigError, Timing, default_config_table, read_config_file,
    },
    system_install::install_files::{
        InstallOptions, UninstallOptions, install_files, uninstall_files,
    },
    systemd_link::{boot_feed::RootRequest, systemd_bus::Scope},
};
use clap::{Args, Parser, Subcommand, ValueEnum};
use jiff::fmt::friendly::SpanParser;

#[derive(Parser)]
#[command(
    version,
    about = "Live dependency tree of systemd jobs while the system boots",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    watch: WatchOptions,
}

#[derive(Args)]
struct WatchOptions {
    #[arg(long, help = "Watch the user service manager instead of PID 1")]
    user: bool,
    #[arg(long, help = "Keep Plymouth visible until Escape requests the tree")]
    plymouth: bool,
    #[arg(
        long,
        help = "Follow a shutdown instead of a boot; started by bootvine-shutdown.service"
    )]
    shutdown: bool,
    #[arg(
        long,
        help = "Root unit of the tree; defaults to the manager's default target"
    )]
    target: Option<String>,
    #[arg(
        long,
        value_parser = |raw: &str| SpanParser::new().parse_unsigned_duration(raw),
        help = "How long the final frame stays on screen, e.g. 1s or 500ms; overrides \
                timing.keep-final-frame-for"
    )]
    hold: Option<Duration>,
    #[arg(
        long,
        default_value = "/etc/bootvine.toml",
        help = "TOML file with [colors] and [timing] tables; see `bootvine defaults`"
    )]
    config: PathBuf,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Print every setting with its default value")]
    Defaults(DefaultsOptions),
    #[command(about = "Exit with an error if a config file is unreadable or invalid")]
    Check(CheckOptions),
    #[command(
        about = "Install this binary and the systemd units, and enable them; asks sudo when needed"
    )]
    Install(InstallOptions),
    #[command(about = "Disable and remove what install put in place")]
    Uninstall(UninstallOptions),
    #[command(
        about = "Let the kernel's image loading progress reach the console; started before \
                 systemd-hibernate-resume.service"
    )]
    OpenResumeConsole,
    #[command(about = "Undo open-resume-console when no hibernated image was restored")]
    CloseResumeConsole,
}

#[derive(Args)]
struct DefaultsOptions {
    #[arg(long, value_enum, default_value_t = DefaultsFormat::Toml)]
    format: DefaultsFormat,
}

#[derive(Args)]
struct CheckOptions {
    #[arg(help = "Config file to check, such as /etc/bootvine.toml")]
    path: PathBuf,
}

#[derive(Clone, Copy, ValueEnum)]
enum DefaultsFormat {
    Toml,
    Nix,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Defaults(options)) => report_outcome(
            render_defaults(options.format).map(|text| println!("{}", text.trim_end())),
        ),
        Some(Command::Check(options)) => report_outcome(
            read_config_file(&options.path)
                .map(|_| ())
                .map_err(|error| format!("{}: {error}", options.path.display())),
        ),
        Some(Command::Install(options)) => report_outcome(install_files(&options)),
        Some(Command::Uninstall(options)) => report_outcome(uninstall_files(&options)),
        Some(Command::OpenResumeConsole) => report_outcome(open_resume_console()),
        Some(Command::CloseResumeConsole) => report_outcome(close_resume_console()),
        None => watch_boot(cli.watch),
    }
}

fn report_outcome(outcome: Result<(), impl Display>) -> ExitCode {
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("bootvine: {error}");
            ExitCode::FAILURE
        }
    }
}

fn render_defaults(format: DefaultsFormat) -> Result<String, Box<dyn Error>> {
    let defaults = default_config_table()?;
    Ok(match format {
        DefaultsFormat::Toml => toml::to_string_pretty(&defaults)?,
        DefaultsFormat::Nix => ser_nix::to_string(&defaults)?,
    })
}

fn watch_boot(options: WatchOptions) -> ExitCode {
    let config = match read_config_file(&options.config) {
        Ok(config) => config,
        Err(ConfigError::Unreadable(error)) if error.kind() == ErrorKind::NotFound => {
            Config::default()
        }
        Err(error) => {
            eprintln!("bootvine: ignoring {}: {error}", options.config.display());
            Config::default()
        }
    };
    let settings = Settings {
        plymouth: options.plymouth,
        scope: if options.user {
            Scope::User
        } else {
            Scope::System
        },
        shutdown: options.shutdown,
        target: options
            .target
            .map_or(RootRequest::ManagerDefault, RootRequest::Named),
        timing: Timing {
            keep_final_frame_for: options.hold.unwrap_or(config.timing.keep_final_frame_for),
            ..config.timing
        },
        colors: config.colors,
    };
    report_outcome(run(&settings))
}
