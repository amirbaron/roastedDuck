//! Run environment-sensitive provider cases in isolated child processes. The
//! parent never mutates process-global environment while other tests are running.
use std::fs;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use roastedduck_aws::{Request, ResolveError, resolve};

#[test]
fn credential_provider_contract() {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("roastedduck-aws-{}-{nonce}", std::process::id()));
    fs::create_dir(&root).unwrap();
    fs::write(root.join("config"), concat!(
        "[default]\nregion=default-region\n",
        "[profile fixture]\nregion=profile-region\n",
        "[profile process]\ncredential_process=/usr/bin/printf ",
        "'{\"Version\":1,\"AccessKeyId\":\"process-key\",\"SecretAccessKey\":\"process-secret\",\"SessionToken\":\"process-token\"}'\n",
    )).unwrap();
    fs::write(
        root.join("credentials"),
        concat!(
            "[default]\naws_access_key_id=default-key\naws_secret_access_key=default-secret\n",
            "[fixture]\naws_access_key_id=profile-key\naws_secret_access_key=profile-secret\n",
        ),
    )
    .unwrap();
    let cases = [
        "env-first",
        "config-first",
        "explicit-profile",
        "default-chain",
        "missing-later-profile",
        "missing-default-profile",
        "validation-none",
        "process",
        "empty-region-precedence",
        "legacy-profile",
        "config-error-before-unknown",
        "unknown-before-config",
    ];
    let executable = std::env::current_exe().unwrap();
    for case in cases {
        let mut command = Command::new(&executable);
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("AWS_") {
                command.env_remove(key);
            }
        }
        let output = command
            .args(["--exact", "credential_probe", "--nocapture"])
            .env("ROASTEDDUCK_AWS_CASE", case)
            .env("AWS_ACCESS_KEY_ID", "environment-key")
            .env("AWS_SECRET_ACCESS_KEY", "environment-secret")
            .env("AWS_SESSION_TOKEN", "environment-token")
            .env("AWS_CONFIG_FILE", root.join("config"))
            .env("AWS_SHARED_CREDENTIALS_FILE", root.join("credentials"))
            .env("AWS_EC2_METADATA_DISABLED", "true")
            .env("AWS_PROFILE", "default")
            .env("AWS_REGION", "")
            .env("AWS_DEFAULT_REGION", "ignored-region")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{case}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    println!("validated provider cases: {}", cases.len());
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[allow(
    clippy::manual_assert_eq,
    reason = "a failing assertion must not print credential material"
)]
fn credential_probe() {
    let Ok(case) = std::env::var("ROASTEDDUCK_AWS_CASE") else {
        return;
    };
    // Refuse to resolve a developer's real environment when invoked manually.
    assert!(std::env::var("AWS_ACCESS_KEY_ID").is_ok_and(|key| key == "environment-key"));
    assert!(std::env::var("AWS_SECRET_ACCESS_KEY").is_ok_and(|key| key == "environment-secret"));
    let mut request = Request::default();
    let expected = match case.as_str() {
        "env-first" => {
            request.chain = "env;config".into();
            request.profile = Some("fixture".into());
            "environment-key"
        }
        "config-first" => {
            request.chain = "config;env".into();
            request.profile = Some("fixture".into());
            "profile-key"
        }
        "explicit-profile" => {
            request.profile = Some("fixture".into());
            "profile-key"
        }
        "default-chain" | "empty-region-precedence" => "environment-key",
        "process" => {
            request.chain = "process".into();
            request.profile = Some("process".into());
            "process-key"
        }
        "legacy-profile" => {
            request.legacy = true;
            request.profile = Some("fixture".into());
            "profile-key"
        }
        "config-error-before-unknown" | "unknown-before-config" => {
            request.profile = Some("missing".into());
            request.chain = if case == "config-error-before-unknown" {
                "config;unknown"
            } else {
                "unknown;config"
            }
            .into();
            let error = resolve(&request).unwrap_err();
            if case == "config-error-before-unknown" {
                assert!(matches!(error, ResolveError::Configuration(_)));
            } else {
                assert!(matches!(error, ResolveError::InvalidInput(_)));
            }
            return;
        }
        "missing-later-profile" | "missing-default-profile" | "validation-none" => {
            request.profile = Some("missing".into());
            if case == "missing-later-profile" {
                request.chain = "env;config".into();
            }
            request.require_credentials = case != "validation-none";
            let resolution = resolve(&request);
            if case == "validation-none" {
                let resolution = resolution.unwrap();
                assert!(resolution.credentials.is_none());
                assert_eq!(resolution.effective_chain, "config");
            } else {
                assert!(matches!(resolution, Err(ResolveError::Configuration(message))
                    if message.contains("no profile 'missing' found")));
            }
            return;
        }
        _ => panic!("unknown fixture case"),
    };
    let resolution = resolve(&request).unwrap();
    if case == "empty-region-precedence" {
        assert_eq!(resolution.region, "default-region");
    }
    let credentials = resolution.credentials.unwrap();
    assert!(credentials.access_key == expected);
    let debug = format!("{credentials:?}");
    assert!(!debug.contains(&credentials.access_key));
    assert!(!debug.contains(&credentials.secret_key));
    if let Some(token) = &credentials.session_token {
        assert!(!debug.contains(token));
    }
}
