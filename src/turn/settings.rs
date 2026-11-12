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
/// The tool that runs a skill of a plugin. A work turn has it when `claude.plugins` names a
/// folder.
pub const SKILL_TOOL: &str = "Skill";
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

/// The tools of a kind, in the order of section 7.1. With `skills`, the turn loads plugins, and
/// the list ends with the tool that runs a skill.
pub fn tools(kind: TurnKind, skills: bool) -> Vec<String> {
    let mut tools: Vec<String> = READER_TOOLS.iter().map(|t| t.to_string()).collect();
    if kind != TurnKind::Reader {
        tools.extend(WRITE_TOOLS.iter().map(|t| t.to_string()));
    }
    if skills {
        tools.push(SKILL_TOOL.to_string());
    }
    tools
}

/// The tools that the `init` event of a turn must list: the tools of the kind, and the tool
/// of the result schema, because each turn has a schema.
pub fn contract_tools(kind: TurnKind, skills: bool) -> Vec<String> {
    let mut tools = tools(kind, skills);
    tools.push(SCHEMA_TOOL.to_string());
    tools
}

/// The deny rules that keep each folder of `protect` from a change by a file tool: one rule
/// for `Edit` and one for `Write`. `//` starts an absolute path in a rule of Claude Code.
pub fn protect_rules(protect: &[PathBuf]) -> Vec<String> {
    protect
        .iter()
        .flat_map(|dir| {
            let pattern = format!("/{}/**", dir.display().to_string().trim_end_matches('/'));
            [format!("Edit({pattern})"), format!("Write({pattern})")]
        })
        .collect()
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
/// `sandbox.allow_read`, and the git configuration of the worker are allowed. They also get a
/// rule that allows each Bash command, because each command runs in that sandbox. A reader has
/// no Bash tool, so its file only denies the tools that it must never have.
///
/// Each folder of `protect`, such as a plugin folder, gets the deny rules of
/// [`protect_rules`], and the sandbox of a worker and a tester denies writes to it, so that no
/// turn changes a skill. The caller gives such a folder in `add_dirs` too, so that the turn can
/// read it.
pub fn render(
    kind: TurnKind,
    workdir: &Path,
    add_dirs: &[PathBuf],
    sandbox: &Sandbox,
    gitconfig: &Path,
    protect: &[PathBuf],
) -> Result<Value> {
    let protect_rules = protect_rules(protect);
    if !sandboxed(kind) {
        let mut deny: Vec<String> = ["Bash", "WebFetch", "WebSearch"].map(String::from).to_vec();
        deny.extend(protect_rules);
        return Ok(json!({"permissions": {"deny": deny}}));
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
    // The rule that allows each Bash command belongs to the strict sandbox below. Claude Code
    // asks for an approval for some commands also in the sandbox, such as a git command with
    // `-c` or a command with a variable in front of it, and a turn has nobody who can approve.
    // With the rule, such a command runs, and it still runs in the sandbox:
    // `allowUnsandboxedCommands` is false, so a call that asks for no sandbox is ignored. The
    // real test `a_real_worker_turn_stays_in_its_sandbox` shows both. Never keep the rule
    // without the strict sandbox.
    let mut value = json!({
        "permissions": {"allow": ["Bash"]},
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
    });
    if !protect.is_empty() {
        value["permissions"]["deny"] = json!(protect_rules);
        value["sandbox"]["filesystem"]["denyWrite"] = json!(protect
            .iter()
            .map(|d| d.display().to_string())
            .collect::<Vec<_>>());
    }
    Ok(value)
}

/// Adds the rules of [`protect_rules`] to `permissions.deny` of a settings object, each one
/// once. A value that is not an object stays as it is.
fn add_deny_rules(value: &mut Value, rules: &[String]) {
    let Some(object) = value.as_object_mut().filter(|_| !rules.is_empty()) else {
        return;
    };
    let permissions = object.entry("permissions").or_insert_with(|| json!({}));
    let Some(permissions) = permissions.as_object_mut() else {
        return;
    };
    let deny = permissions.entry("deny").or_insert_with(|| json!([]));
    let Some(deny) = deny.as_array_mut() else {
        return;
    };
    for rule in rules {
        if !deny.iter().any(|r| r.as_str() == Some(rule)) {
            deny.push(json!(rule));
        }
    }
}

/// The key of `settings.json` that turns off the lines that Claude Code adds about itself: the
/// co-author trailer of a commit and the line in the text of a pull request. It has only the
/// two keys that each version of Claude Code with the key `attribution` knows, because a
/// version that refuses a settings file ignores the whole file, and the sandbox of the turn is
/// in that file. A turn of Vision is a local session, so it has no session link to hide.
pub fn attribution_off() -> Value {
    json!({"commit": "", "pr": ""})
}

/// Sets the key `attribution` of a settings object for `claude.attribution = false`. With true,
/// the object stays as it is, so the file has no such key.
fn apply_attribution(value: &mut Value, attribution: bool) {
    if attribution {
        return;
    }
    if let Some(object) = value.as_object_mut() {
        object.entry("attribution").or_insert_with(attribution_off);
    }
}

/// The text of `settings.json` for one turn: `$VISION_HOME/sandbox/<kind>.json` when it exists
/// (it replaces the rendered file), else the rendered value. With `attribution` false, each
/// form gets the key of [`attribution_off`]. A file of the home that has its own key
/// `attribution` keeps it. A file of the home also gets the deny rules of the folders of
/// `protect`, so that no turn changes a skill; its sandbox stays as the person wrote it.
pub fn settings_text(
    paths: &Paths,
    kind: TurnKind,
    rendered: &Value,
    attribution: bool,
    protect: &[PathBuf],
) -> Result<String> {
    let custom = paths.sandbox_settings(kind);
    if let Some(text) = read_optional(&custom)? {
        let mut value = serde_json::from_str::<Value>(&text).with_context(|| {
            format!(
                "{} is not valid JSON. Fix it, or remove it to use the settings that Vision renders",
                custom.display()
            )
        })?;
        let add_attribution = !attribution && value.get("attribution").is_none();
        if !value.is_object() || (!add_attribution && protect.is_empty()) {
            return Ok(text);
        }
        apply_attribution(&mut value, attribution);
        add_deny_rules(&mut value, &protect_rules(protect));
        return Ok(format!("{}\n", serde_json::to_string_pretty(&value)?));
    }
    let mut value = rendered.clone();
    apply_attribution(&mut value, attribution);
    Ok(format!("{}\n", serde_json::to_string_pretty(&value)?))
}

/// `$VISION_HOME/worker.gitconfig`: `GIT_CONFIG_GLOBAL` of a worker and a tester, whose sandbox
/// cannot read the git configuration of the person.
pub fn gitconfig_path(paths: &Paths) -> PathBuf {
    paths.home().join("worker.gitconfig")
}

/// The git configuration of a worker: a neutral name and a no-reply address from `user`.
///
/// It also names an empty global ignore file and an empty global attributes file. Without
/// them, git looks for both in the home folder of the person, which the sandbox does not let
/// it read, so each git command of a session prints a warning.
pub fn gitconfig_text(user: &str, git: &crate::config::Git) -> String {
    let (name, email) = identity(user, git);
    format!(
        "# Written by vision for its sessions. A session cannot read the git configuration of the person.\n\
         [user]\n\tname = \"{name}\"\n\temail = {email}\n\
         [commit]\n\tgpgsign = false\n[tag]\n\tgpgsign = false\n\
         [core]\n\texcludesFile = /dev/null\n\tattributesFile = /dev/null\n"
    )
}

/// The name and the address of each commit of a session: `[git]` of the configuration, else
/// the name of `user` with "(vision)" and an address that names nobody.
pub fn identity(user: &str, git: &crate::config::Git) -> (String, String) {
    if !git.name.trim().is_empty() && !git.email.trim().is_empty() {
        return (git.name.trim().to_string(), git.email.trim().to_string());
    }
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
    (
        format!("{name} (vision)"),
        format!("{local}@noreply.invalid"),
    )
}

/// Writes the git configuration of a worker when its text changed.
pub fn write_gitconfig(paths: &Paths, user: &str, git: &crate::config::Git) -> Result<PathBuf> {
    let path = gitconfig_path(paths);
    let text = gitconfig_text(user, git);
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

/// The environment of a session, from the environment of this process. If `USER` or `HOME` is
/// missing, the user database gives it. A system timer can start Vision with a short
/// environment, and Claude Code finds its login under the name of the user: a real session
/// with no `USER` answered "Not logged in".
pub fn session_env_of_process(pass: &[String]) -> Vec<(OsString, OsString)> {
    let mut env = session_env(std::env::vars_os(), pass);
    fill_user(&mut env, system_user());
    env
}

/// Adds `USER` and `HOME` if the environment lacks them.
fn fill_user(env: &mut Vec<(OsString, OsString)>, user: Option<(OsString, OsString)>) {
    let Some((name, home)) = user else {
        return;
    };
    for (key, value) in [("USER", name), ("HOME", home)] {
        if !env.iter().any(|(k, _)| k == key) {
            env.push((OsString::from(key), value));
        }
    }
}

/// The login name and the home folder of the user of this process, from the user database.
fn system_user() -> Option<(OsString, OsString)> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStringExt;

    let mut buffer = vec![0 as libc::c_char; 16 * 1024];
    // SAFETY: a zeroed `passwd` is a valid value for `getpwuid_r` to fill.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: the entry, the buffer, and the result pointer live for the whole call, and the
    // length is the length of the buffer.
    let code = unsafe {
        libc::getpwuid_r(
            libc::geteuid(),
            &mut entry,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut found,
        )
    };
    if code != 0 || found.is_null() || entry.pw_name.is_null() || entry.pw_dir.is_null() {
        return None;
    }
    // SAFETY: both pointers point into `buffer`, which is alive, and each string ends with 0.
    let (name, home) = unsafe {
        (
            CStr::from_ptr(entry.pw_name).to_bytes().to_vec(),
            CStr::from_ptr(entry.pw_dir).to_bytes().to_vec(),
        )
    };
    Some((OsString::from_vec(name), OsString::from_vec(home)))
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
    fn a_short_environment_gets_the_user_and_the_home_folder() {
        let user = Some((OsString::from("ada"), OsString::from("/home/ada")));
        let mut env = os(&[("PATH", "/bin")]);
        fill_user(&mut env, user.clone());
        assert_eq!(
            env,
            os(&[("PATH", "/bin"), ("USER", "ada"), ("HOME", "/home/ada")])
        );
        // A value that is there stays.
        let mut env = os(&[("USER", "bob"), ("HOME", "/h")]);
        fill_user(&mut env, user);
        assert_eq!(env, os(&[("USER", "bob"), ("HOME", "/h")]));
        // The user database of the machine knows the user of this test.
        let (name, home) = system_user().expect("the user of this process");
        assert!(!name.is_empty() && !home.is_empty());
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
            &[],
        )
        .unwrap();
        assert_eq!(
            value,
            json!({"permissions":{"allow":["Bash"]},
                "sandbox":{"enabled":true,"autoAllowBashIfSandboxed":true,"allowUnsandboxedCommands":false,
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
            &[],
        )
        .unwrap();
        // The rule that allows each Bash command is only safe with the strict sandbox: no run
        // outside it, and no start when the sandbox is not there.
        assert_eq!(value["permissions"]["allow"], json!(["Bash"]));
        assert_eq!(value["sandbox"]["enabled"], true);
        assert_eq!(value["sandbox"]["allowUnsandboxedCommands"], false);
        assert_eq!(value["sandbox"]["failIfUnavailable"], true);
        assert!(reader.get("sandbox").is_none());
        assert!(reader["permissions"].get("allow").is_none());
        assert_eq!(tools(TurnKind::Reader, false), READER_TOOLS);
        assert_eq!(tools(TurnKind::Tester, false).len(), 7);
        assert_eq!(permission_mode(TurnKind::Tester), "acceptEdits");
    }

    #[test]
    fn a_plugin_folder_is_readable_and_never_writable() {
        let protect = [
            PathBuf::from("/plugins/one"),
            PathBuf::from("/shared/skills/qa/"),
        ];
        let rules = [
            "Edit(//plugins/one/**)",
            "Write(//plugins/one/**)",
            "Edit(//shared/skills/qa/**)",
            "Write(//shared/skills/qa/**)",
        ];
        assert_eq!(protect_rules(&protect), rules);
        let worker = render(
            TurnKind::Tester,
            Path::new("/clones/one"),
            &protect,
            &Sandbox::default(),
            Path::new("/v/worker.gitconfig"),
            &protect,
        )
        .unwrap();
        assert_eq!(worker["permissions"]["allow"], json!(["Bash"]));
        assert_eq!(worker["permissions"]["deny"], json!(rules));
        assert_eq!(
            worker["sandbox"]["filesystem"],
            json!({"denyRead": ["~/"],
                "allowRead": ["/clones/one", "/plugins/one", "/shared/skills/qa/", "/v/worker.gitconfig"],
                "denyWrite": ["/plugins/one", "/shared/skills/qa/"]})
        );
        let reader = render(
            TurnKind::Reader,
            Path::new("/c"),
            &protect,
            &Sandbox::default(),
            Path::new("/g"),
            &protect,
        )
        .unwrap();
        let mut deny = vec!["Bash", "WebFetch", "WebSearch"];
        deny.extend(rules);
        assert_eq!(reader, json!({"permissions": {"deny": deny}}));
        assert_eq!(tools(TurnKind::Reader, true).last().unwrap(), SKILL_TOOL);
        assert_eq!(
            contract_tools(TurnKind::Worker, true),
            [
                "Read",
                "Grep",
                "Glob",
                "Task",
                "Edit",
                "Write",
                "Bash",
                "Skill",
                "StructuredOutput"
            ]
        );

        // A settings file of the home keeps its sandbox and gets the deny rules.
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        std::fs::create_dir_all(paths.sandbox_dir()).unwrap();
        std::fs::write(
            paths.sandbox_settings(TurnKind::Worker),
            "{\"sandbox\": {\"enabled\": true}, \"permissions\": {\"deny\": [\"WebFetch\", \"Edit(//plugins/one/**)\"]}}",
        )
        .unwrap();
        let text = settings_text(&paths, TurnKind::Worker, &worker, true, &protect).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["sandbox"], json!({"enabled": true}));
        assert_eq!(
            value["permissions"]["deny"],
            json!([
                "WebFetch",
                "Edit(//plugins/one/**)",
                "Write(//plugins/one/**)",
                "Edit(//shared/skills/qa/**)",
                "Write(//shared/skills/qa/**)"
            ])
        );
        std::fs::write(paths.sandbox_settings(TurnKind::Worker), "{}").unwrap();
        let text = settings_text(&paths, TurnKind::Worker, &worker, true, &protect).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["permissions"]["deny"], json!(rules));
    }

    #[test]
    fn a_settings_file_of_the_home_replaces_the_rendered_one() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        let rendered = json!({"permissions": {}});
        let text = settings_text(&paths, TurnKind::Worker, &rendered, true, &[]).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), rendered);
        std::fs::create_dir_all(paths.sandbox_dir()).unwrap();
        std::fs::write(
            paths.sandbox_settings(TurnKind::Worker),
            "{\"sandbox\": {}}",
        )
        .unwrap();
        let text = settings_text(&paths, TurnKind::Worker, &rendered, true, &[]).unwrap();
        assert_eq!(text, "{\"sandbox\": {}}");
        // With attribution off, the file of the home gets the key too.
        let text = settings_text(&paths, TurnKind::Worker, &rendered, false, &[]).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            json!({"sandbox": {}, "attribution": attribution_off()})
        );
        // A file of the home with its own key keeps it.
        std::fs::write(
            paths.sandbox_settings(TurnKind::Worker),
            "{\"attribution\": {\"commit\": \"x\"}}",
        )
        .unwrap();
        let text = settings_text(&paths, TurnKind::Worker, &rendered, false, &[]).unwrap();
        assert_eq!(text, "{\"attribution\": {\"commit\": \"x\"}}");
        std::fs::write(paths.sandbox_settings(TurnKind::Worker), "{").unwrap();
        let err = settings_text(&paths, TurnKind::Worker, &rendered, true, &[]).unwrap_err();
        assert!(err.to_string().contains("is not valid JSON"), "{err}");
    }

    #[test]
    fn attribution_off_adds_the_key_and_on_leaves_none() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path());
        let rendered = json!({"permissions": {"deny": ["Bash"]}});
        let on: Value = serde_json::from_str(
            &settings_text(&paths, TurnKind::Reader, &rendered, true, &[]).unwrap(),
        )
        .unwrap();
        assert!(on.get("attribution").is_none(), "{on}");
        let off: Value = serde_json::from_str(
            &settings_text(&paths, TurnKind::Reader, &rendered, false, &[]).unwrap(),
        )
        .unwrap();
        assert_eq!(off["attribution"], json!({"commit": "", "pr": ""}));
        assert_eq!(off["permissions"], rendered["permissions"]);
    }

    #[test]
    fn the_git_configuration_is_neutral() {
        let text = gitconfig_text("Some One", &crate::config::Git::default());
        assert!(text.contains("name = \"Some One (vision)\""), "{text}");
        assert!(text.contains("email = some-one@noreply.invalid"), "{text}");
        assert!(gitconfig_text("\"<>", &crate::config::Git::default())
            .contains("email = user@noreply.invalid"));
        let bot = crate::config::Git {
            name: "app[bot]".into(),
            email: "7+app[bot]@users.noreply.github.com".into(),
        };
        let text = gitconfig_text("Some One", &bot);
        assert!(text.contains("name = \"app[bot]\""), "{text}");
        assert!(
            text.contains("email = 7+app[bot]@users.noreply.github.com"),
            "{text}"
        );
        assert!(!text.contains("(vision)"), "{text}");
        // No look into the home folder of the person for an ignore file or an attributes file.
        assert!(text.contains("excludesFile = /dev/null"), "{text}");
        assert!(text.contains("attributesFile = /dev/null"), "{text}");
    }
}
