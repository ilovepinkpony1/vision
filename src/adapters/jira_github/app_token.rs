//! `vision app-token`: prints an installation token of a GitHub App, and nothing else.
//!
//! It makes a JWT signed with RS256 by the `openssl` program, and asks GitHub for an
//! installation token. The private key comes from a file or from the standard output of a
//! command. A key from a command goes to openssl through a pipe: it never lands in a file, and
//! it is never printed. A command can also print the PEM file as one base64 line, as a keychain
//! item can hold it. Use it as the command of a secret:
//!
//! ```toml
//! FORGE_TOKEN = { cmd = "vision app-token --app-id 123 --installation-id 456 --key-file ~/.keys/app.pem" }
//! ```

use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::time::Duration;

use serde_json::Value;

use super::core::{clean, fail, plain_env, truthy, Error, Res};
use super::http::{check_base, Call, Client};
use super::proc::{self, How};
use crate::cli::AppTokenArgs;

/// The largest private key that the command reads.
const KEY_MAX: usize = 60000;

/// Where the key comes from.
enum Key {
    /// The key itself, from a command.
    Text(Vec<u8>),
    /// A key file, which openssl reads.
    File(String),
}

/// `vision app-token`: prints the token, or `app-token: <why>` on standard error with exit
/// code 1.
pub fn command(args: &AppTokenArgs) -> anyhow::Result<()> {
    match token(args) {
        Ok(value) => {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(format!("{value}\n").as_bytes())?;
            stdout.flush()?;
            Ok(())
        }
        Err(e) => {
            let text = match e {
                Error::Unexpected(text) => format!("unexpected error: {text}"),
                other => other.to_string(),
            };
            eprintln!("app-token: {text}");
            Err(crate::exit::Stop::Reported(text).into())
        }
    }
}

fn token(args: &AppTokenArgs) -> Res<String> {
    let api = check_base(&args.api, "--api")?;
    let key = read_key(args)?;
    let now = crate::util::time::now().unix_timestamp();
    let header = crate::util::base64::encode_url(br#"{"alg":"RS256","typ":"JWT"}"#);
    let issuer = if !args.app_id.is_empty() && args.app_id.bytes().all(|b| b.is_ascii_digit()) {
        args.app_id.trim_start_matches('0').to_string()
    } else {
        serde_json::to_string(&args.app_id).unwrap_or_default()
    };
    let issuer = if issuer.is_empty() {
        "0".into()
    } else {
        issuer
    };
    let claims = format!(
        "{{\"iat\":{},\"exp\":{},\"iss\":{issuer}}}",
        now - 60,
        now + 540
    );
    let payload = crate::util::base64::encode_url(claims.as_bytes());
    let signing_input = format!("{header}.{payload}");
    let signature = sign(signing_input.as_bytes(), &key, args.timeout)?;
    let jwt = format!(
        "{signing_input}.{}",
        crate::util::base64::encode_url(&signature)
    );
    let client = Client {
        service: "github".into(),
        base: api,
        headers: vec![
            ("Authorization".into(), format!("Bearer {jwt}")),
            ("Accept".into(), "application/vnd.github+json".into()),
            ("X-GitHub-Api-Version".into(), "2022-11-28".into()),
            ("User-Agent".into(), "vision-jira-github-adapter".into()),
        ],
        timeout: args.timeout,
    };
    let data = client
        .call(
            "POST",
            &format!("/app/installations/{}/access_tokens", args.installation_id),
            "make an installation token",
            Call {
                ok: &[201],
                ..Call::default()
            },
        )?
        .json()?;
    match truthy(Some(&data)).and_then(|d| d.get("token")) {
        Some(Value::String(value)) if !value.is_empty() => Ok(value.clone()),
        _ => fail("GitHub gave no token"),
    }
}

fn read_key(args: &AppTokenArgs) -> Res<Key> {
    if let Some(file) = args.key_file.as_deref().filter(|f| !f.is_empty()) {
        let path =
            crate::paths::expand_tilde(file).unwrap_or_else(|_| std::path::PathBuf::from(file));
        if !path.is_file() {
            return fail(format!("the key file {} does not exist", path.display()));
        }
        return Ok(Key::File(path.display().to_string()));
    }
    let script = args.key_command.as_deref().unwrap_or("");
    let mut command = Command::new("sh");
    command.arg("-c").arg(script).env_clear().envs(plain_env());
    let output = proc::run(
        command,
        None,
        Some(Duration::from_secs_f64(args.timeout)),
        How::default(),
    )
    .map_err(|e| Error::Unexpected(format!("OSError: {e}")))?;
    if output.timed_out {
        return fail(format!(
            "the key command did not end in {} s",
            super::core::whole(args.timeout)
        ));
    }
    let code = output.code.unwrap_or(-1);
    if code != 0 || output.stdout.trim_ascii().is_empty() {
        return fail(format!(
            "the key command failed with exit code {code} or printed nothing"
        ));
    }
    Ok(Key::Text(pem_of(output.stdout)))
}

/// The PEM text of a key: the text itself when it has a PEM header, or the text decoded from
/// base64 once when that gives a PEM header. Each other text stays as it is, and openssl names
/// the problem.
pub fn pem_of(text: Vec<u8>) -> Vec<u8> {
    const HEADER: &[u8] = b"-----BEGIN";
    let has_header = |bytes: &[u8]| bytes.windows(HEADER.len()).any(|w| w == HEADER);
    if has_header(&text) {
        return text;
    }
    let decoded = std::str::from_utf8(&text)
        .ok()
        .and_then(crate::util::base64::decode);
    match decoded {
        Some(pem) if has_header(&pem) => pem,
        _ => text,
    }
}

/// The RS256 signature of `data` by openssl. A key in memory goes through a pipe.
fn sign(data: &[u8], key: &Key, timeout: f64) -> Res<Vec<u8>> {
    let mut command = Command::new("openssl");
    command
        .args(["dgst", "-sha256", "-sign"])
        .env_clear()
        .envs(plain_env());
    let mut reader = None;
    match key {
        Key::File(path) => {
            command.arg(path);
        }
        Key::Text(text) => {
            if text.len() > KEY_MAX {
                return fail("the private key is too large");
            }
            let (pipe_reader, mut pipe_writer) = std::io::pipe()
                .map_err(|_| Error::Failure("could not run openssl: OSError".into()))?;
            pipe_writer
                .write_all(text)
                .map_err(|_| Error::Failure("could not run openssl: OSError".into()))?;
            drop(pipe_writer);
            let fd = pipe_reader.as_raw_fd();
            command.arg(format!("/dev/fd/{fd}"));
            // SAFETY: fcntl only clears the close-on-exec mark of the read end of the pipe in
            // the new child, so that openssl can read the key from it. It is safe to call
            // between fork and exec.
            unsafe {
                command.pre_exec(move || {
                    if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            reader = Some(pipe_reader);
        }
    }
    let output = proc::run(
        command,
        Some(data.to_vec()),
        Some(Duration::from_secs_f64(timeout)),
        How::default(),
    );
    drop(reader);
    let output = match output {
        Ok(output) if !output.timed_out => output,
        Ok(_) => return fail("could not run openssl: TimeoutExpired"),
        Err(e) => {
            let name = if e.kind() == std::io::ErrorKind::NotFound {
                "FileNotFoundError"
            } else {
                "OSError"
            };
            return fail(format!("could not run openssl: {name}"));
        }
    };
    if output.code != Some(0) || output.stdout.is_empty() {
        // The text of openssl names the problem; it never holds the key.
        return fail(format!(
            "openssl could not sign with the private key: {}",
            clean(&output.stderr_text(), 200)
        ));
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_in_base64_is_decoded_once() {
        let pem = b"-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n".to_vec();
        assert_eq!(pem_of(pem.clone()), pem);
        let line = crate::util::base64::encode(&pem);
        assert_eq!(pem_of(format!("{line}\n").into_bytes()), pem);
        // Twice encoded: decoded once only, so it stays as it is.
        let twice = crate::util::base64::encode(line.as_bytes()).into_bytes();
        assert_eq!(pem_of(twice.clone()), twice);
        assert_eq!(pem_of(b"not a key".to_vec()), b"not a key");
    }
}
