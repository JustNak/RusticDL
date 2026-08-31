use std::path::PathBuf;

/// Parsed command-line options passed by the main app (or a manual repair run).
#[derive(Debug, Clone)]
pub struct UpdaterArgs {
    pub download_url: Option<String>,
    pub installer_path: Option<PathBuf>,
    pub wait_pid: Option<u32>,
    pub app_exe: PathBuf,
    #[allow(dead_code)] // reserved for richer UI copy / logs
    pub from_version: Option<String>,
    pub to_version: Option<String>,
    pub release_page: Option<String>,
    pub expected_size: Option<u64>,
    pub expected_sha256: Option<String>,
    pub wait_timeout_secs: u64,
}

impl UpdaterArgs {
    pub fn parse<I, S>(args: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut download_url = None;
        let mut installer_path = None;
        let mut wait_pid = None;
        let mut app_exe = None;
        let mut from_version = None;
        let mut to_version = None;
        let mut release_page = None;
        let mut expected_size = None;
        let mut expected_sha256 = None;
        let mut wait_timeout_secs = 90_u64;

        let mut iter = args.into_iter().map(|s| s.as_ref().to_string());
        while let Some(arg) = iter.next() {
            let mut next = |flag: &str| {
                iter.next()
                    .ok_or_else(|| format!("Missing value for {flag}"))
            };
            match arg.as_str() {
                "--download-url" => download_url = Some(next("--download-url")?),
                "--installer-path" => {
                    installer_path = Some(PathBuf::from(next("--installer-path")?))
                }
                "--wait-pid" => {
                    let raw = next("--wait-pid")?;
                    wait_pid = Some(
                        raw.parse::<u32>()
                            .map_err(|_| format!("Invalid --wait-pid: {raw}"))?,
                    );
                }
                "--app-exe" => app_exe = Some(PathBuf::from(next("--app-exe")?)),
                "--from-version" => from_version = Some(next("--from-version")?),
                "--to-version" => to_version = Some(next("--to-version")?),
                "--release-page" => release_page = Some(next("--release-page")?),
                "--expected-size" => {
                    let raw = next("--expected-size")?;
                    expected_size = Some(
                        raw.parse::<u64>()
                            .map_err(|_| format!("Invalid --expected-size: {raw}"))?,
                    );
                }
                "--expected-sha256" => expected_sha256 = Some(next("--expected-sha256")?),
                "--wait-timeout-secs" => {
                    let raw = next("--wait-timeout-secs")?;
                    wait_timeout_secs = raw
                        .parse::<u64>()
                        .map_err(|_| format!("Invalid --wait-timeout-secs: {raw}"))?;
                }
                "--help" | "-h" => {
                    return Err(help_text());
                }
                other => return Err(format!("Unknown argument: {other}\n\n{}", help_text())),
            }
        }

        let app_exe =
            app_exe.ok_or_else(|| format!("--app-exe is required.\n\n{}", help_text()))?;

        if download_url.is_none() && installer_path.is_none() {
            return Err(format!(
                "Provide either --download-url or --installer-path.\n\n{}",
                help_text()
            ));
        }
        if download_url.is_some() && installer_path.is_some() {
            return Err("Use only one of --download-url or --installer-path.".into());
        }

        Ok(Self {
            download_url,
            installer_path,
            wait_pid,
            app_exe,
            from_version,
            to_version,
            release_page,
            expected_size,
            expected_sha256,
            wait_timeout_secs,
        })
    }
}

fn help_text() -> String {
    r#"RusticDL Updater

Usage:
  rusticdl-updater.exe --app-exe <path> (--download-url <url> | --installer-path <path>) [options]

Options:
  --download-url <url>       Download NSIS setup from this URL
  --installer-path <path>    Use a local setup binary
  --wait-pid <pid>           Wait for this process to exit before installing
  --app-exe <path>           Path to rusticdl.exe to relaunch
  --from-version <semver>    Current version (UI)
  --to-version <semver>      Target version (UI)
  --release-page <url>       Opened when the update fails
  --expected-size <bytes>    Optional Content-Length hint
  --expected-sha256 <hex>    Required on Linux (SHA-256 of the tarball)
  --wait-timeout-secs <n>    Max seconds to wait for main exit (default 90)
"#
    .trim()
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_download_url_flow() {
        let args = UpdaterArgs::parse([
            "--app-exe",
            r"C:\Apps\RusticDL\rusticdl.exe",
            "--download-url",
            "https://example.com/setup.exe",
            "--wait-pid",
            "1234",
            "--to-version",
            "0.2.1",
            "--expected-size",
            "1024",
        ])
        .unwrap();
        assert_eq!(args.wait_pid, Some(1234));
        assert_eq!(args.to_version.as_deref(), Some("0.2.1"));
        assert_eq!(args.expected_size, Some(1024));
        assert!(args.download_url.is_some());
        assert!(args.installer_path.is_none());
    }

    #[test]
    fn rejects_both_sources() {
        let err = UpdaterArgs::parse([
            "--app-exe",
            "rusticdl.exe",
            "--download-url",
            "https://example.com/a",
            "--installer-path",
            "setup.exe",
        ])
        .unwrap_err();
        assert!(err.contains("only one"));
    }
}
