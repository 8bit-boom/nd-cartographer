//! Finding upstream llama.cpp's `llama-server`. The studio looks for it and runs it; it never
//! downloads or installs one (a downloaded executable is code the person did not choose).
//! When it is missing, [`install_hint`] says how to get it from the official channels.

use super::hw::run_capture;
use serde::Serialize;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Runtime {
    pub path: PathBuf,
    /// The build or version line `--version` printed, when it could be read.
    pub version: Option<String>,
    /// Where it was found: `env`, `settings`, `path` or `known`.
    pub source: &'static str,
}

pub const ENV_VAR: &str = "ND_CARTO_LLAMA_SERVER";

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "llama-server.exe"
    } else {
        "llama-server"
    }
}

fn is_executable(p: &Path) -> bool {
    let Ok(m) = std::fs::metadata(p) else {
        return false;
    };
    if !m.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Places a package manager or installer puts it.
pub fn known_locations() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"]
        .iter()
        .map(|d| Path::new(d).join(exe_name()))
        .collect();
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        v.push(
            Path::new(&local)
                .join("Microsoft")
                .join("WinGet")
                .join("Links")
                .join(exe_name()),
        );
    }
    v
}

/// The search, with every input passed in (the caller reads the environment), in order: the
/// environment variable, the saved setting, `PATH`, the known locations.
pub fn find_in(
    env_path: Option<&str>,
    setting: Option<&str>,
    path_var: Option<OsString>,
    known: &[PathBuf],
) -> Option<(PathBuf, &'static str)> {
    let explicit = |s: Option<&str>| {
        s.map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
    };
    for (p, src) in [(explicit(env_path), "env"), (explicit(setting), "settings")] {
        if let Some(p) = p {
            // An explicit choice is used as given; if it is wrong, say so rather than guess.
            return is_executable(&p).then_some((p, src));
        }
    }
    if let Some(var) = path_var {
        for dir in std::env::split_paths(&var) {
            let p = dir.join(exe_name());
            if is_executable(&p) {
                return Some((p, "path"));
            }
        }
    }
    known
        .iter()
        .find(|p| is_executable(p))
        .map(|p| (p.clone(), "known"))
}

/// The build line from `--version` output: `version: 6789 (abc1234)` (llama.cpp prints it on
/// stderr) or a bare semver such as `1.0.0`.
pub fn parse_version(out: &str) -> Option<String> {
    let line = out.lines().map(str::trim).find(|l| {
        l.to_ascii_lowercase().starts_with("version")
            || l.chars().next().is_some_and(|c| c.is_ascii_digit())
    })?;
    let v = line
        .split_once(':')
        .map(|(_, r)| r.trim())
        .filter(|_| line.to_ascii_lowercase().starts_with("version"))
        .unwrap_or(line);
    (!v.is_empty() && v.chars().any(|c| c.is_ascii_digit())).then(|| v.to_string())
}

/// Ask a program for its version (2 s limit).
pub fn probe_version(path: &Path) -> Option<String> {
    run_capture(path, &["--version"], Duration::from_secs(2)).and_then(|o| parse_version(&o))
}

/// Find the runtime the way the commands do: reads `ND_CARTO_LLAMA_SERVER`, `PATH` and the
/// well-known places. `setting` is the saved `managed.runtime_path`.
pub fn find(setting: Option<&str>) -> Option<Runtime> {
    let env = std::env::var(ENV_VAR).ok();
    let (path, source) = find_in(
        env.as_deref(),
        setting,
        std::env::var_os("PATH"),
        &known_locations(),
    )?;
    let version = probe_version(&path);
    Some(Runtime {
        path,
        version,
        source,
    })
}

/// How to get `llama-server`, for this OS (the official channels only; nothing is run for you).
pub fn install_hint(os: &str) -> String {
    match os {
        "macos" => "Install llama.cpp with Homebrew:\n  brew install llama.cpp".into(),
        "windows" => "Install llama.cpp with winget (then open a new terminal):\n  winget install llama.cpp\n\
             Or download a release zip from https://github.com/ggml-org/llama.cpp/releases (pick the CUDA or Vulkan build for your GPU) and set ND_CARTO_LLAMA_SERVER to its llama-server.exe."
            .into(),
        _ => "Install llama.cpp with one of:\n  brew install llama.cpp\n  conda install -c conda-forge llama.cpp   (CUDA and Vulkan builds)\n\
             Or download a release archive from https://github.com/ggml-org/llama.cpp/releases and set ND_CARTO_LLAMA_SERVER to its llama-server.\n\
             Or use the Docker profile: docker compose --profile ai-cpu up -d (docs/DOCKER.md)."
            .into(),
    }
}

/// The hint for the OS this build runs on.
pub fn install_hint_here() -> String {
    install_hint(std::env::consts::OS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_read_from_llama_cpp_and_semver_output() {
        assert_eq!(
            parse_version("version: 6789 (abc1234)\nbuilt with cc for x86_64\n").as_deref(),
            Some("6789 (abc1234)")
        );
        assert_eq!(parse_version("1.0.0\n").as_deref(), Some("1.0.0"));
        assert_eq!(parse_version("error: nope"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn hints_name_the_right_installer_per_os() {
        assert!(install_hint("macos").contains("brew install llama.cpp"));
        assert!(install_hint("windows").contains("winget install llama.cpp"));
        let l = install_hint("linux");
        assert!(l.contains("brew install llama.cpp") && l.contains("conda install"));
        assert!(l.contains("ai-cpu"));
        // No hint downloads or runs anything by itself.
        assert!(!l.contains("curl") && !l.contains("| sh"));
    }

    #[cfg(unix)]
    mod unix {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        fn fake(dir: &Path, name: &str, body: &str) -> PathBuf {
            let p = dir.join(name);
            std::fs::write(&p, body).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        }

        fn tmp(tag: &str) -> PathBuf {
            let d = std::env::temp_dir().join(format!("ndcart-rt-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            d
        }

        #[test]
        fn the_environment_variable_wins_and_its_version_is_read() {
            let d = tmp("env");
            let p = fake(
                &d,
                "llama-server",
                "#!/bin/sh\necho 'version: 42 (deadbee)' >&2\n",
            );
            let (found, src) = find_in(p.to_str(), Some("/nope"), None, &[]).unwrap();
            assert_eq!((found.as_path(), src), (p.as_path(), "env"));
            assert_eq!(probe_version(&p).as_deref(), Some("42 (deadbee)"));
            // A wrong explicit choice is not silently replaced by a search.
            assert!(find_in(
                Some("/definitely/missing"),
                None,
                Some(d.clone().into_os_string()),
                &[]
            )
            .is_none());
            let _ = std::fs::remove_dir_all(&d);
        }

        #[test]
        fn then_the_setting_then_path_then_known_places() {
            let d = tmp("order");
            let on_path = fake(&d, "llama-server", "#!/bin/sh\n");
            let other = tmp("order2");
            let known = fake(&other, "llama-server", "#!/bin/sh\n");
            let (p, src) = find_in(None, on_path.to_str(), None, &[]).unwrap();
            assert_eq!((p, src), (on_path.clone(), "settings"));
            let (p, src) = find_in(
                None,
                None,
                Some(d.clone().into_os_string()),
                std::slice::from_ref(&known),
            )
            .unwrap();
            assert_eq!((p, src), (on_path.clone(), "path"));
            let (p, src) = find_in(
                None,
                None,
                None,
                &[PathBuf::from("/no/such"), known.clone()],
            )
            .unwrap();
            assert_eq!((p, src), (known, "known"));
            // A file without the executable bit is not a runtime.
            std::fs::set_permissions(&on_path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(find_in(None, None, Some(d.clone().into_os_string()), &[]).is_none());
            let _ = std::fs::remove_dir_all(&d);
            let _ = std::fs::remove_dir_all(&other);
        }

        #[test]
        fn a_missing_runtime_is_none_so_the_hint_is_shown() {
            assert!(find_in(None, None, Some("/no/such/dir".into()), &[]).is_none());
        }
    }
}
