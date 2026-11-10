//! What a session may do (sections 7.1 and 7.3): the tools and the permission mode of each
//! kind, the rendered `settings.json` with its sandbox, the git configuration of a worker,
//! and the allow list of the environment.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::config::Sandbox;
use crate::paths::{expand_tilde, Paths};
use crate::turn::TurnKind;
use crate::util::fs::{atomic_write, read_optional};

/// The tools of a reader (section 7.1).
pub const READER_TOOLS: [&str; 4] = ["Read", "Grep", "Glob", "Task"];
/// The tools that a worker and a tester have in addition to the reader tools.
pub const WRITE_TOOLS: [&str; 3] = ["Edit", "Write", "Bash"];
/// The tool that Claude Code adds by itself when a turn has a result schema (`--json-schema`).
/// The `init` event lists it next to the tools of `--tools`. A real session showed this.
pub const SCHEMA_TOOL: &str = "StructuredOutput";

/// The names of the environment that a session gets (section 7.3), next to `claude.env_pass`.
/// `LC_*` matches each name that starts with `LC_`.
pub const ALLOWED_ENV: [&str; 10] = [
    "PATH",
    "HOME",
    "USER",
    "SHELL",
    "LANG",
    "LC_*",
    "TERM",
    "TMPDIR",
    "CLAUDE_CONFIG_DIR",
    "GIT_CONFIG_GLOBAL",
];

/// The tools of a kind, in the order of section 7.1.
pub fn tools(kind: TurnKind) -> Vec<String> {
    let mut tools: Vec<String> = READER_TOOLS.iter().map(|t| t.to_string()).collect();
    if kind != TurnKind::Reader {
        tools.extend(WRITE_TOOLS.iter().map(|t| t.to_string()));
    }
    tools
}

/// The tools that the `init` event of a turn must list: the tools of the kind, and the tool
/// of the result schema, because each turn has a schema.
pub fn contract_tools(kind: TurnKind) -> Vec<String> {
    let mut tools = tools(kind);
    tools.push(SCHEMA_TOOL.to_string());
    tools
}

/// `--permission-mode`: `dontAsk` for a reader, `acceptEdits` for the others.
pub fn permission_mode(kind: TurnKind) -> &'static str {
    match kind {
        TurnKind::Reader => "dontAsk",
        TurnKind::Worker | TurnKind::Tester => "acceptEdits",
    }
}

/// True when the kind runs Bash, so that its session needs the sandbox and git.
pub fn sandboxed(kind: TurnKind) -> bool {
    kind != TurnKind::Reader
}

/// The `settings.json` of a session. A worker and a tester get the sandbox: reads of the home
/// folder are denied, and the working folder, the extra folders, the folders of
/// `sandbox.allow_read`, and the git configuration of the worker are allowed. A reader has no
/// Bash tool, so its file only denies the tools that it must never have.
pub fn render(
    kind: TurnKind,
    workdir: &Path,
    add_dirs: &[PathBuf],
    sandbox: &Sandbox,
    gitconfig: &Path,
) -> Result<Value> {
    if !sandboxed(kind) {
        return Ok(json!({"permissions": {"deny": ["Bash", "WebFetch", "WebSearch"]}}));
    }
    let mut allow_read = vec![workdir.display().to_string()];
    for dir in add_dirs {
        allow_read.push(dir.display().to_string());
    }
    for entry in &sandbox.allow_read {
        let path = expand_tilde(entry)
            .with_context(|| format!("could not read sandbox.allow_read entry `{entry}`"))?;
        allow_read.push(path.display().to_string());
    }
    allow_read.push(gitconfig.display().to_string());
    Ok(json!({
        "sandbox": {
            "enabled": true,
            "autoAllowBashIfSandboxed": true,
            "allowUnsandboxedCommands": false,
            "failIfUnavailable": true,
            "filesystem": {
                "denyRead": ["~/"],
                "allowRead": allow_read,
            },
            "network": {
                "allowedDomains": sandbox.allow_domains,
                "allowLocalBinding": true,
                "allowUnixSockets": sandbox.allow_unix_sockets,
            }
        }
    }))
}

/// The text of `settings.json` for one turn: `$VISION_HOME/sandbox/<kind>.json` when it exists
/// (it replaces the rendered file), else the rendered value.
pub fn settings_text(paths: &Paths, kind: TurnKind, rendered: &Value) -> Result<String> {
    let custom = paths.sandbox_settings(kind);
    if let Some(text) = read_optional(&custom)? {
        serde_json::from_str::<Value>(&text).with_context(|| {
            format!(
                "{} is not valid JSON. Fix it, or remove it to use the settings that Vision renders",
                custom.display()
            )
        })?;
        return Ok(text);
    }
    Ok(format!("{}\n", serde_json::to_string_pretty(rendered)?))
}

/// `$VISION_HOME/worker.gitconfig`: `GIT_CONFIG_GLOBAL` of a worker and a tester, whose sandbox
/// cannot read the git configuration of the person.
pub fn gitconfig_path(paths: &Paths) -> PathBuf {
    paths.home().join("worker.gitconfig")
}

/// The git configuration of a worker: a neutral name and a no-reply address from `user`.
pub fn gitconfig_text(user: &str) -> String {
    let local: String = user
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let local = local.trim_matches(['-', '.']);
    let local = if local.is_empty() { "user" } else { local };
    let name: String = user
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '"' | '\\' | '<' | '>'))
        .collect();
    let name = if name.trim().is_empty() {
        "user"
    } else {
        name.trim()
    };
    format!(
        "# Written by vision for its sessions. A session cannot read the git configuration of the person.\n\
         [user]\n\tname = \"{name} (vision)\"\n\temail = {local}@noreply.invalid\n\
         [commit]\n\tgpgsign = false\n[tag]\n\tgpgsign = false\n"
    )
}

/// Writes the git configuration of a worker when its text changed.
pub fn write_gitconfig(paths: &Paths, user: &str) -> Result<PathBuf> {
    let path = gitconfig_path(paths);
    let text = gitconfig_text(user);
    if read_optional(&path)?.as_deref() != Some(text.as_str()) {
        atomic_write(&path, text.as_bytes())?;
    }
    Ok(path)
}

/// The environment of a session: each variable whose name is on the allow list or in
/// `pass`. Every other name is dropped, so that a token of the executor never reaches a session.
pub fn session_env<I>(vars: I, pass: &[String]) -> Vec<(OsString, OsString)>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    vars.into_iter()
        .filter(|(name, _)| name.to_str().is_some_and(|name| allowed(name, pass)))
        .collect()
}

fn allowed(name: &str, pass: &[String]) -> bool {
    ALLOWED_ENV
        .iter()
        .any(|allowed| match allowed.strip_suffix('*') {
            Some(prefix) => name.starts_with(prefix),
            None => *allowed == name,
        })
        || pass.iter().any(|p| p == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v)))
            .collect()
    }

    #[test]
    fn the_environment_is_an_allow_list() {
        let vars = os(&[
            ("PATH", "/bin"),
            ("HOME", "/h"),
            ("LC_ALL", "C"),
            ("FORGE_TOKEN", "secret"),
            ("GH_TOKEN", "secret"),
            ("EXTRA", "x"),
            ("LCX", "no"),
        ]);
        let env = session_env(vars, &["EXTRA".to_string()]);
        let names: Vec<_> = env.iter().map(|(k, _)| k.to_str().unwrap()).collect();
        assert_eq!(names, ["PATH", "HOME", "LC_ALL", "EXTRA"]);
    }

    #[test]
    fn a_worker_gets_the_sandbox_of_the_known_shape() {
        let sandbox = Sandbox {
            allow_read: vec!["/opt/tool chain".into()],
            ..Sandbox::default()
        };
        let value = render(
            TurnKind::Worker,
            Path::new("/clones/one"),
            &[],
            &sandbox,
            Path::new("/v/worker.gitconfig"),
        )
        .unwrap();
        assert_eq!(
            value,
            json!({"sandbox":{"enabled":true,"autoAllowBashIfSandboxed":true,"allowUnsandboxedCommands":false,
                "failIfUnavailable":true,"filesystem":{"denyRead":["~/"],
                "allowRead":["/clones/one","/opt/tool chain","/v/worker.gitconfig"]},
                "network":{"allowedDomains":["localhost","127.0.0.1"],"allowLocalBinding":true,"allowUnixSockets":[]}}})
        );
        let reader = render(
            TurnKind::Reader,
            Path::new("/c"),
            &[],
            &sandbox,
            Path::new("/g"),
        )
        .unwrap();
        assert!(reader.get("sandbox").is_none());
        assert_eq!(tools(TurnKind::Reader), READER_TOOLS);
        assert_eq!(tools(TurnKind::Tester).len(), 7);
        assert_eq!(permission_mode(TurnKind::Tester), "acceptEdits");
    }

    #[test]
    fn a_settings_file_of_the_home_replaces_the_rendered_one() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        let rendered = json!({"permissions": {}});
        let text = settings_text(&paths, TurnKind::Worker, &rendered).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), rendered);
        std::fs::create_dir_all(paths.sandbox_dir()).unwrap();
        std::fs::write(
            paths.sandbox_settings(TurnKind::Worker),
            "{\"sandbox\": {}}",
        )
        .unwrap();
        let text = settings_text(&paths, TurnKind::Worker, &rendered).unwrap();
        assert_eq!(text, "{\"sandbox\": {}}");
        std::fs::write(paths.sandbox_settings(TurnKind::Worker), "{").unwrap();
        let err = settings_text(&paths, TurnKind::Worker, &rendered).unwrap_err();
        assert!(err.to_string().contains("is not valid JSON"), "{err}");
    }

    #[test]
    fn the_git_configuration_is_neutral() {
        let text = gitconfig_text("Some One");
        assert!(text.contains("name = \"Some One (vision)\""), "{text}");
        assert!(text.contains("email = some-one@noreply.invalid"), "{text}");
        assert!(gitconfig_text("\"<>").contains("email = user@noreply.invalid"));
    }
}
