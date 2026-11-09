//! `vision install` and `vision uninstall`: the system timer that runs `vision tick` each
//! minute. On macOS it is a launchd agent; on Linux, a systemd user service and timer.
//!
//! `--print` shows the files and the commands and changes nothing. Tests use only `--print`.

use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::ctx::Ctx;
use crate::paths::{user_home, Paths, HOME_ENV};
use crate::util::fs::{atomic_write, ensure_dir};
use crate::util::ids::sha256_hex;

/// Seconds between two ticks.
pub const INTERVAL_SECONDS: u32 = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Platform {
    Launchd,
    Systemd,
}

impl Platform {
    fn current() -> Result<Platform> {
        if cfg!(target_os = "macos") {
            Ok(Platform::Launchd)
        } else if cfg!(target_os = "linux") {
            Ok(Platform::Systemd)
        } else {
            bail!("vision install supports macOS (launchd) and Linux (systemd) only")
        }
    }
}

/// One file of the timer.
struct UnitFile {
    path: PathBuf,
    text: String,
}

/// One command that installs or removes the timer.
struct Step {
    args: Vec<String>,
    /// True for a command that fails when there is nothing to undo, such as unloading a timer
    /// that is not loaded.
    may_fail: bool,
}

impl Step {
    fn new(args: &[&str], may_fail: bool) -> Step {
        Step {
            args: args.iter().map(|a| a.to_string()).collect(),
            may_fail,
        }
    }

    fn line(&self) -> String {
        let words: Vec<String> = self.args.iter().map(|a| shell_word(a)).collect();
        let note = if self.may_fail {
            "   # may fail when it is not loaded"
        } else {
            ""
        };
        format!("{}{note}", words.join(" "))
    }

    fn run(&self) -> Result<()> {
        let output = Command::new(&self.args[0])
            .args(&self.args[1..])
            .output()
            .with_context(|| format!("could not run {}", self.args[0]))?;
        if !output.status.success() && !self.may_fail {
            bail!(
                "`{}` failed: {}",
                self.line(),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }
}

/// The timer of one home: its name and its files.
struct Timer {
    platform: Platform,
    name: String,
    files: Vec<PathBuf>,
}

impl Timer {
    fn new(paths: &Paths) -> Result<Timer> {
        let platform = Platform::current()?;
        let name = timer_name(paths, platform);
        let files = match platform {
            Platform::Launchd => vec![user_home()?
                .join("Library/LaunchAgents")
                .join(format!("{name}.plist"))],
            Platform::Systemd => {
                let dir = systemd_user_dir()?;
                vec![
                    dir.join(format!("{name}.service")),
                    dir.join(format!("{name}.timer")),
                ]
            }
        };
        Ok(Timer {
            platform,
            name,
            files,
        })
    }

    fn contents(&self, paths: &Paths) -> Result<Vec<UnitFile>> {
        let exe = std::env::current_exe()
            .and_then(|e| e.canonicalize())
            .context("could not find the path of this vision binary")?;
        let env = TimerEnv {
            exe,
            vision_home: paths.home().to_path_buf(),
            home: std::env::var("HOME").unwrap_or_default(),
            path: std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()),
            error_log: error_log(paths),
        };
        let texts = match self.platform {
            Platform::Launchd => vec![launchd_plist(&self.name, &env)],
            Platform::Systemd => vec![systemd_service(&env), systemd_timer(&self.name)],
        };
        Ok(self
            .files
            .iter()
            .cloned()
            .zip(texts)
            .map(|(path, text)| UnitFile { path, text })
            .collect())
    }

    fn install_steps(&self) -> Vec<Step> {
        match self.platform {
            Platform::Launchd => {
                let domain = launchd_domain();
                let service = format!("{domain}/{}", self.name);
                let plist = self.files[0].to_string_lossy().to_string();
                vec![
                    Step::new(&["launchctl", "bootout", &service], true),
                    Step::new(&["launchctl", "bootstrap", &domain, &plist], false),
                ]
            }
            Platform::Systemd => vec![
                Step::new(&["systemctl", "--user", "daemon-reload"], false),
                Step::new(
                    &[
                        "systemctl",
                        "--user",
                        "enable",
                        "--now",
                        &format!("{}.timer", self.name),
                    ],
                    false,
                ),
            ],
        }
    }

    /// The commands before the files go, and the commands after.
    fn uninstall_steps(&self) -> (Vec<Step>, Vec<Step>) {
        match self.platform {
            Platform::Launchd => {
                let service = format!("{}/{}", launchd_domain(), self.name);
                (
                    vec![Step::new(&["launchctl", "bootout", &service], true)],
                    Vec::new(),
                )
            }
            Platform::Systemd => (
                vec![Step::new(
                    &[
                        "systemctl",
                        "--user",
                        "disable",
                        "--now",
                        &format!("{}.timer", self.name),
                    ],
                    true,
                )],
                vec![Step::new(&["systemctl", "--user", "daemon-reload"], false)],
            ),
        }
    }
}

/// The values that the timer files hold.
struct TimerEnv {
    exe: PathBuf,
    vision_home: PathBuf,
    home: String,
    path: String,
    error_log: PathBuf,
}

/// `vision install [--print]`.
pub fn install(ctx: &Ctx, print: bool) -> Result<()> {
    let timer = Timer::new(&ctx.paths)?;
    let files = timer.contents(&ctx.paths)?;
    let steps = timer.install_steps();
    if print {
        println!("# vision install --print changed nothing. It would write these files and run these commands.");
        for file in &files {
            println!(
                "\n# file: {}\n{}",
                file.path.display(),
                file.text.trim_end()
            );
        }
        println!("\n# commands:");
        for step in &steps {
            println!("{}", step.line());
        }
        return Ok(());
    }
    ctx.require_setup()?;
    for file in &files {
        if let Some(dir) = file.path.parent() {
            ensure_dir(dir)?;
        }
        atomic_write(&file.path, file.text.as_bytes())?;
    }
    for step in &steps {
        step.run()?;
    }
    ctx.log(&format!(
        "install: the timer {} runs `vision tick` each minute",
        timer.name
    ));
    println!(
        "Installed the timer {}: `vision tick` runs each minute. Run `vision doctor` in two minutes to test it.",
        timer.name
    );
    Ok(())
}

/// `vision uninstall [--print]`.
pub fn uninstall(ctx: &Ctx, print: bool) -> Result<()> {
    let timer = Timer::new(&ctx.paths)?;
    let (before, after) = timer.uninstall_steps();
    if print {
        println!("# vision uninstall --print changed nothing. It would run these commands and remove these files.");
        println!("\n# commands:");
        for step in &before {
            println!("{}", step.line());
        }
        println!("\n# files to remove:");
        for path in &timer.files {
            println!("{}", path.display());
        }
        if !after.is_empty() {
            println!("\n# then:");
            for step in &after {
                println!("{}", step.line());
            }
        }
        return Ok(());
    }
    if !timer.files.iter().any(|f| f.exists()) {
        println!(
            "The timer {} is not installed. Nothing changed.",
            timer.name
        );
        return Ok(());
    }
    for step in &before {
        step.run()?;
    }
    for path in &timer.files {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(e).with_context(|| format!("could not remove {}", path.display()))
            }
        }
    }
    for step in &after {
        step.run()?;
    }
    if ctx.paths.log_dir().is_dir() {
        ctx.log(&format!("uninstall: removed the timer {}", timer.name));
    }
    println!(
        "Removed the timer {}. Run `vision tick` by hand, or `vision install` again.",
        timer.name
    );
    Ok(())
}

/// True when the timer file of this home exists.
pub fn is_installed(paths: &Paths) -> bool {
    Timer::new(paths).is_ok_and(|timer| timer.files.iter().all(|f| f.is_file()))
}

/// Where the launchd timer writes the standard error of each tick.
pub fn error_log(paths: &Paths) -> PathBuf {
    paths.log_dir().join("timer.err")
}

/// `local.vision.tick` or `vision-tick` for `~/.vision`. Any other home gets a short hash of its
/// path, so that two homes can each have a timer.
fn timer_name(paths: &Paths, platform: Platform) -> String {
    let suffix = if paths.is_default() {
        String::new()
    } else {
        format!("-{}", &sha256_hex(paths.home().as_os_str().as_bytes())[..8])
    };
    match platform {
        Platform::Launchd => format!("local.vision.tick{suffix}"),
        Platform::Systemd => format!("vision-tick{suffix}"),
    }
}

fn launchd_domain() -> String {
    // SAFETY: getuid has no failure mode and touches no memory.
    let uid = unsafe { libc::getuid() };
    format!("gui/{uid}")
}

fn systemd_user_dir() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => dir,
        _ => user_home()?.join(".config"),
    };
    Ok(base.join("systemd/user"))
}

fn launchd_plist(label: &str, env: &TimerEnv) -> String {
    let x = |text: &str| xml_escape(text);
    let p = |path: &Path| xml_escape(&path.to_string_lossy());
    let home = if env.home.is_empty() {
        String::new()
    } else {
        format!(
            "\n        <key>HOME</key>\n        <string>{}</string>",
            x(&env.home)
        )
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>tick</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>{home_env}</key>
        <string>{vision_home}</string>{home}
        <key>PATH</key>
        <string>{path}</string>
    </dict>
    <key>StartInterval</key>
    <integer>{interval}</integer>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Background</string>
    <key>StandardOutPath</key>
    <string>/dev/null</string>
    <key>StandardErrorPath</key>
    <string>{error_log}</string>
</dict>
</plist>
"#,
        label = x(label),
        exe = p(&env.exe),
        home_env = HOME_ENV,
        vision_home = p(&env.vision_home),
        path = x(&env.path),
        interval = INTERVAL_SECONDS,
        error_log = p(&env.error_log),
    )
}

fn systemd_service(env: &TimerEnv) -> String {
    let mut environment = vec![
        format!("{HOME_ENV}={}", env.vision_home.display()),
        format!("PATH={}", env.path),
    ];
    if !env.home.is_empty() {
        environment.push(format!("HOME={}", env.home));
    }
    let environment: Vec<String> = environment
        .iter()
        .map(|e| format!("Environment={}", systemd_quote(e)))
        .collect();
    format!(
        "[Unit]\nDescription=Vision tick: one pass of the ticket pipeline\n\n\
         [Service]\nType=oneshot\n{}\nExecStart={} tick\nStandardOutput=null\n",
        environment.join("\n"),
        systemd_quote(&env.exe.to_string_lossy()).replace('$', "$$"),
    )
}

fn systemd_timer(name: &str) -> String {
    format!(
        "[Unit]\nDescription=Run the Vision tick each minute\n\n\
         [Timer]\nOnActiveSec=10s\nOnUnitActiveSec={INTERVAL_SECONDS}s\nAccuracySec=1s\nUnit={name}.service\n\n\
         [Install]\nWantedBy=timers.target\n"
    )
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// A double-quoted systemd value: `\` and `"` escaped, and `%` doubled for the specifiers.
fn systemd_quote(text: &str) -> String {
    format!(
        "\"{}\"",
        text.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
    )
}

/// A word that a POSIX shell reads back as the same text.
fn shell_word(text: &str) -> String {
    let plain = !text.is_empty()
        && text.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '/' | '.' | '-' | '_' | ':' | '=' | '@' | '+' | ',')
        });
    if plain {
        text.to_string()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> TimerEnv {
        TimerEnv {
            exe: PathBuf::from("/opt/vision/bin/vision"),
            vision_home: PathBuf::from("/srv/a & b"),
            home: "/home/demo".into(),
            path: "/usr/bin:/bin".into(),
            error_log: PathBuf::from("/srv/a & b/log/timer.err"),
        }
    }

    #[test]
    fn the_plist_runs_tick_each_minute_with_escaped_values() {
        let plist = launchd_plist("local.vision.tick", &env());
        assert!(plist
            .contains("<string>/opt/vision/bin/vision</string>\n        <string>tick</string>"));
        assert!(plist.contains("<integer>60</integer>"));
        assert!(plist.contains("<string>/srv/a &amp; b</string>"));
        assert!(plist.contains("<key>VISION_HOME</key>"));
    }

    #[test]
    fn the_systemd_units_quote_their_values() {
        let mut env = env();
        env.path = "/odd%dir:/usr/bin".into();
        let service = systemd_service(&env);
        assert!(service.contains("Environment=\"VISION_HOME=/srv/a & b\""));
        assert!(service.contains("Environment=\"PATH=/odd%%dir:/usr/bin\""));
        assert!(service.contains("ExecStart=\"/opt/vision/bin/vision\" tick"));
        let timer = systemd_timer("vision-tick");
        assert!(timer.contains("OnUnitActiveSec=60s"));
        assert!(timer.contains("Unit=vision-tick.service"));
    }

    #[test]
    fn a_home_other_than_the_default_gets_its_own_timer_name() {
        let name = timer_name(&Paths::new("/srv/vision-test-home"), Platform::Systemd);
        assert!(name.starts_with("vision-tick-"));
        assert_eq!(name.len(), "vision-tick-".len() + 8);
    }

    #[test]
    fn shell_words_are_quoted_only_when_needed() {
        assert_eq!(
            shell_word("gui/501/local.vision.tick"),
            "gui/501/local.vision.tick"
        );
        assert_eq!(shell_word("/a b/c"), "'/a b/c'");
        assert_eq!(shell_word("it's"), r"'it'\''s'");
    }
}
