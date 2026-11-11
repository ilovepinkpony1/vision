//! `vision app-token`: a JWT signed with RS256 by openssl, and an installation token of a
//! GitHub App.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::jira_github_support::{vision_bin, AdapterTest, Reply};

const TOKEN: &str = "ghs_fakeinstallationtoken0123";

fn unb64(text: &str) -> Vec<u8> {
    vision::util::base64::decode(text).expect("base64url")
}

fn openssl(args: &[&str]) {
    let status = Command::new("openssl")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "openssl {args:?}");
}

fn new_key(path: &Path) {
    openssl(&[
        "genpkey",
        "-algorithm",
        "RSA",
        "-pkeyopt",
        "rsa_keygen_bits:2048",
        "-out",
        &path.display().to_string(),
    ]);
}

/// The header, the claims, and the result of the check of the signature, of each JWT that the
/// fake GitHub got.
type Claims = Arc<Mutex<Vec<(Value, Value, bool)>>>;

struct AppToken {
    t: AdapterTest,
    keys: PathBuf,
    key: PathBuf,
    key_text: String,
    claims: Claims,
}

impl AppToken {
    fn new() -> AppToken {
        let t = AdapterTest::new();
        let keys = t.path("keys");
        std::fs::create_dir_all(&keys).unwrap();
        let key = keys.join("app.pem");
        let public = keys.join("app.pub");
        new_key(&key);
        openssl(&[
            "pkey",
            "-in",
            &key.display().to_string(),
            "-pubout",
            "-out",
            &public.display().to_string(),
        ]);
        let key_text = std::fs::read_to_string(&key).unwrap();
        let claims: Claims = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&claims);
        let folder = keys.clone();
        t.server
            .route("POST", "/app/installations/99/access_tokens", move |req| {
                let auth = req
                    .headers
                    .get("authorization")
                    .cloned()
                    .unwrap_or_default();
                let Some(jwt) = auth.strip_prefix("Bearer ") else {
                    return Reply::json(401, json!({"message": "no JWT"}));
                };
                let parts: Vec<&str> = jwt.split('.').collect();
                let (header, payload, signature) = (parts[0], parts[1], parts[2]);
                let mut seen = seen.lock().unwrap();
                let sig = folder.join(format!("sig-{}.bin", seen.len()));
                std::fs::write(&sig, unb64(signature)).unwrap();
                let mut check = Command::new("openssl")
                    .args(["dgst", "-sha256", "-verify"])
                    .arg(&public)
                    .arg("-signature")
                    .arg(&sig)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap();
                std::io::Write::write_all(
                    &mut check.stdin.take().unwrap(),
                    format!("{header}.{payload}").as_bytes(),
                )
                .unwrap();
                let verified = check.wait().unwrap().success();
                seen.push((
                    serde_json::from_slice(&unb64(header)).unwrap(),
                    serde_json::from_slice(&unb64(payload)).unwrap(),
                    verified,
                ));
                if !verified {
                    return Reply::json(
                        401,
                        json!({"message": "A JSON web token could not be decoded"}),
                    );
                }
                Reply::json(
                    201,
                    json!({"token": TOKEN, "expires_at": "2026-11-10T13:00:00Z"}),
                )
            });
        AppToken {
            t,
            keys,
            key,
            key_text,
            claims,
        }
    }

    /// Runs `vision app-token` with a plain environment, and tests that no part of the key is in
    /// its output.
    fn app_token(&self, args: &[&str]) -> (i32, String, String) {
        let output = Command::new(vision_bin())
            .args([
                "app-token",
                "--app-id",
                "123",
                "--installation-id",
                "99",
                "--api",
                &self.t.server.url,
            ])
            .args(args)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &self.t.home)
            .env("LANG", "C.UTF-8")
            .env("TMPDIR", &self.t.scratch)
            .current_dir(self.t.tmp.path())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&output.stdout).into_owned();
        let err = String::from_utf8_lossy(&output.stderr).into_owned();
        let lines: Vec<&str> = self.key_text.lines().collect();
        for part in &lines[1..lines.len() - 1] {
            assert!(
                !format!("{out}{err}").contains(part),
                "the key is never printed"
            );
        }
        (output.status.code().unwrap_or(-1), out, err)
    }

    fn assert_good_jwt(&self) {
        let claims = self.claims.lock().unwrap();
        let (header, claims, verified) = claims.last().unwrap();
        assert!(
            verified,
            "openssl verifies the RS256 signature with the public key"
        );
        assert_eq!(header, &json!({"alg": "RS256", "typ": "JWT"}));
        assert_eq!(claims["iss"], 123);
        let now = vision::util::time::now().unix_timestamp();
        let iat = claims["iat"].as_i64().unwrap();
        let exp = claims["exp"].as_i64().unwrap();
        assert!(iat <= now);
        assert!(exp > now);
        assert!(exp - iat <= 600);
    }

    fn key_path(&self) -> String {
        self.key.display().to_string()
    }

    fn scratch_names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.t.scratch)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

#[test]
fn app_token_a_key_file() {
    let a = AppToken::new();
    let result = a.app_token(&["--key-file", &a.key_path()]);
    assert_eq!(result, (0, format!("{TOKEN}\n"), String::new()));
    a.assert_good_jwt();
}

#[test]
fn app_token_a_key_command_leaves_no_file() {
    let a = AppToken::new();
    let before = a.scratch_names();
    let result = a.app_token(&["--key-command", &format!("cat '{}'", a.key_path())]);
    assert_eq!(result, (0, format!("{TOKEN}\n"), String::new()));
    a.assert_good_jwt();
    assert_eq!(a.scratch_names(), before, "the key never lands in a file");
    let calls =
        a.t.server
            .calls(Some("POST"), Some("/app/installations/99/access_tokens"));
    let request = calls.last().unwrap();
    assert_eq!(request.headers["accept"], "application/vnd.github+json");
    assert_eq!(request.headers["x-github-api-version"], "2022-11-28");
}

#[test]
fn app_token_a_key_command_can_print_the_key_as_one_base64_line() {
    let a = AppToken::new();
    // A keychain item can hold the PEM file as one base64 line.
    let line = a.keys.join("app.b64");
    std::fs::write(
        &line,
        format!("{}\n", vision::util::base64::encode(a.key_text.as_bytes())),
    )
    .unwrap();
    let before = a.scratch_names();
    let result = a.app_token(&["--key-command", &format!("cat '{}'", line.display())]);
    assert_eq!(result, (0, format!("{TOKEN}\n"), String::new()));
    a.assert_good_jwt();
    assert_eq!(a.scratch_names(), before, "the key never lands in a file");
}

#[test]
fn app_token_failures_name_the_problem_and_never_the_key() {
    let a = AppToken::new();
    let (code, out, err) = a.app_token(&["--key-command", "exit 4"]);
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(
        err.contains("the key command failed with exit code 4"),
        "{err}"
    );
    let bad = a.keys.join("bad.pem");
    std::fs::write(
        &bad,
        "-----BEGIN PRIVATE KEY-----\nnot a key\n-----END PRIVATE KEY-----\n",
    )
    .unwrap();
    let (code, out, err) = a.app_token(&["--key-file", &bad.display().to_string()]);
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(err.contains("openssl could not sign"), "{err}");
    let other = a.keys.join("other.pem");
    new_key(&other);
    let (code, out, err) = a.app_token(&["--key-file", &other.display().to_string()]);
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(
        err.contains(
            "make an installation token (POST /app/installations/99/access_tokens) answered HTTP 401"
        ),
        "{err}"
    );
    assert!(!err.contains("Bearer"), "{err}");
    let (code, _out, err) = a.app_token(&[
        "--key-file",
        &a.key_path(),
        "--api",
        "http://api.example.com",
    ]);
    assert_eq!(code, 1);
    assert!(err.contains("https://"), "{err}");
    let missing = a.keys.join("missing.pem");
    let (code, _out, err) = a.app_token(&["--key-file", &missing.display().to_string()]);
    assert_eq!(code, 1);
    assert!(err.contains("does not exist"), "{err}");
}
