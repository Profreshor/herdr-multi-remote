//! Remote thin-client launcher over SSH command stdio.

use super::shell_quote;
use base64::Engine as _;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, IsTerminal, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};

use interprocess::local_socket::traits::Listener as _;
use interprocess::local_socket::traits::Stream as _;
use interprocess::local_socket::ListenerNonblockingMode;
use interprocess::TryClone as _;
use serde::Deserialize;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const BRIDGE_ACCEPT_POLL: Duration = Duration::from_millis(50);
const BRIDGE_IO_POLL: Duration = Duration::from_millis(10);
const SSH_CHILD_POLL: Duration = Duration::from_millis(25);
const BRIDGE_SOCKET_PERMISSION_MODE: u32 = 0o600;
const MAX_BRIDGE_STDERR_BYTES: usize = 16 * 1024;
const REMOTE_SERVER_SHUTDOWN_CONFIRM_TIMEOUT: Duration = Duration::from_secs(5);
const REMOTE_SERVER_SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(100);
const CURRENT_PROTOCOL: u32 = crate::protocol::PROTOCOL_VERSION;
const STABLE_UPDATE_MANIFEST_URL: &str =
    "https://github.com/Profreshor/herdr-multi-remote/releases/latest/download/latest.json";
const REMOTE_BINARY_ENV_VAR: &str = "HERDR_REMOTE_BINARY";
const SSH_CONTROL_SOCKET_NAME: &str = "ctl";
pub(crate) const REATTACH_COMMAND_ENV_VAR: &str = "HERDR_REATTACH_COMMAND";

pub(crate) const REMOTE_KEYBINDINGS_ENV_VAR: &str = "HERDR_REMOTE_KEYBINDINGS";

#[derive(Debug)]
struct RemoteCompatibilityError(String);

impl std::fmt::Display for RemoteCompatibilityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for RemoteCompatibilityError {}

fn remote_compatibility_error(message: impl Into<String>) -> io::Error {
    io::Error::other(RemoteCompatibilityError(message.into()))
}

pub(crate) fn is_remote_compatibility_error(error: &io::Error) -> bool {
    error
        .get_ref()
        .and_then(|error| error.downcast_ref::<RemoteCompatibilityError>())
        .is_some()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteKeybindings {
    Local,
    Server,
}

impl RemoteKeybindings {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "local" => Ok(Self::Local),
            "server" => Ok(Self::Server),
            _ => Err("--remote-keybindings must be 'local' or 'server'".to_string()),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Server => "server",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteLaunch {
    pub(crate) target: String,
    pub(crate) keybindings: RemoteKeybindings,
    pub(crate) live_handoff: bool,
}

pub(crate) fn extract_remote_args(
    args: &[String],
) -> Result<(Vec<String>, Option<RemoteLaunch>), String> {
    let mut cleaned = Vec::with_capacity(args.len());
    if let Some(program) = args.first() {
        cleaned.push(program.clone());
    }

    let mut remote_target = None;
    let mut keybindings = RemoteKeybindings::Local;
    let mut keybindings_seen = false;
    let mut live_handoff = false;
    let mut index = 1;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            cleaned.extend_from_slice(&args[index..]);
            break;
        }
        if arg == "--handoff" {
            live_handoff = true;
            index += 1;
            continue;
        }
        if arg == "--remote" {
            if remote_target.is_some() {
                return Err("--remote can only be specified once".to_string());
            }
            let Some(value) = args.get(index + 1) else {
                return Err("missing value for --remote".to_string());
            };
            remote_target = Some(validate_remote_target(value)?.to_owned());
            index += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--remote=") {
            if remote_target.is_some() {
                return Err("--remote can only be specified once".to_string());
            }
            remote_target = Some(validate_remote_target(value)?.to_owned());
            index += 1;
            continue;
        }
        if arg == "--remote-keybindings" {
            if keybindings_seen {
                return Err("--remote-keybindings can only be specified once".to_string());
            }
            let Some(value) = args.get(index + 1) else {
                return Err("missing value for --remote-keybindings".to_string());
            };
            keybindings = RemoteKeybindings::parse(value)?;
            keybindings_seen = true;
            index += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--remote-keybindings=") {
            if keybindings_seen {
                return Err("--remote-keybindings can only be specified once".to_string());
            }
            keybindings = RemoteKeybindings::parse(value)?;
            keybindings_seen = true;
            index += 1;
            continue;
        }

        cleaned.push(arg.clone());
        index += 1;
    }

    let remote = remote_target.map(|target| RemoteLaunch {
        target,
        keybindings,
        live_handoff,
    });
    if remote.is_none() && keybindings_seen {
        return Err("--remote-keybindings requires --remote".to_string());
    }
    if remote.is_none() && live_handoff {
        cleaned.push("--handoff".to_string());
    }

    Ok((cleaned, remote))
}

fn validate_remote_target(target: &str) -> Result<&str, String> {
    if target.is_empty() {
        return Err("missing value for --remote".to_string());
    }
    if target.starts_with('-') {
        return Err("--remote target must not start with '-'".to_string());
    }
    Ok(target)
}

pub(crate) fn run_remote(remote: RemoteLaunch) -> io::Result<()> {
    let session_name = crate::session::active_name()
        .unwrap_or_else(|| crate::session::DEFAULT_SESSION_NAME.to_string());
    let local_socket = local_forward_socket_path(&remote.target, &session_name);
    let program = std::env::args()
        .next()
        .unwrap_or_else(|| "herdr".to_string());
    let reattach_command = reattach_command(
        &program,
        &remote.target,
        &session_name,
        remote.keybindings,
        remote.live_handoff,
    );
    let manage_ssh_config = crate::config::Config::load()
        .config
        .remote
        .manage_ssh_config;
    let remote_ssh = RemoteSsh::new(remote.target.clone(), manage_ssh_config);
    let prepared_remote =
        prepare_remote_herdr(&remote_ssh, &session_name, remote.live_handoff, true)?;
    ensure_remote_server_ready(
        &remote_ssh,
        &prepared_remote.remote_herdr,
        &session_name,
        prepared_remote.stop_after_install_approved,
        remote.live_handoff,
        true,
    )?;

    let _bridge = SshStdioBridge::start(
        remote.target,
        prepared_remote.remote_herdr,
        local_socket.clone(),
        session_name,
        remote_ssh.options(),
    )?;

    run_client_process(&local_socket, &reattach_command, remote.keybindings)
}

pub(crate) struct RemoteClientBridge {
    _bridge: SshStdioBridge,
    _ssh: RemoteSsh,
}

pub(crate) fn connect_remote_client(
    target: String,
    session_name: String,
    manage_ssh_config: bool,
    cancelled: Arc<AtomicBool>,
) -> io::Result<(crate::ipc::LocalStream, RemoteClientBridge)> {
    let local_socket = local_forward_socket_path(&target, &session_name);
    let remote_ssh =
        RemoteSsh::with_cancel(target.clone(), manage_ssh_config, cancelled.clone(), true);
    let prepared_remote = prepare_remote_herdr(&remote_ssh, &session_name, false, false)?;
    ensure_remote_server_ready(
        &remote_ssh,
        &prepared_remote.remote_herdr,
        &session_name,
        prepared_remote.stop_after_install_approved,
        false,
        false,
    )?;
    let bridge = SshStdioBridge::start_with_program(
        target,
        prepared_remote.remote_herdr,
        local_socket.clone(),
        session_name,
        remote_ssh.options(),
        PathBuf::from("ssh"),
        true,
        cancelled,
    )?;
    let stream = crate::ipc::connect_local_stream(&local_socket)?;
    Ok((
        stream,
        RemoteClientBridge {
            _bridge: bridge,
            _ssh: remote_ssh,
        },
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemotePlatform {
    os: &'static str,
    arch: &'static str,
}

impl RemotePlatform {
    fn from_uname(os: &str, arch: &str) -> Option<Self> {
        let os = match os.trim() {
            "Linux" => "linux",
            "Darwin" => "macos",
            "Windows" => "windows",
            _ => return None,
        };
        let arch = match arch.trim().to_ascii_lowercase().as_str() {
            "x86_64" | "amd64" => "x86_64",
            "aarch64" | "arm64" if os != "windows" => "aarch64",
            // Herdr's supported Windows package is x86_64 and runs under the
            // operating system's native emulation on Windows ARM64.
            "aarch64" | "arm64" if os == "windows" => "x86_64",
            _ => return None,
        };
        Some(Self { os, arch })
    }

    fn local() -> Self {
        let os = if cfg!(target_os = "linux") {
            "linux"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else if cfg!(windows) {
            "windows"
        } else {
            "unknown"
        };

        let arch = if cfg!(target_arch = "x86_64") {
            "x86_64"
        } else if cfg!(target_arch = "aarch64") {
            "aarch64"
        } else {
            "unknown"
        };

        Self { os, arch }
    }

    fn asset_key(&self) -> String {
        format!("{}-{}", self.os, self.arch)
    }

    fn is_windows(&self) -> bool {
        self.os == "windows"
    }
}

#[derive(Debug, Clone)]
struct RemoteHerdr {
    install_suffix: String,
    shell_path: String,
    platform: RemotePlatform,
}

impl RemoteHerdr {
    fn for_platform(platform: RemotePlatform) -> Self {
        if platform.is_windows() {
            return Self {
                install_suffix: "AppData/Local/Programs/Herdr/bin/herdr.exe".to_string(),
                shell_path: "(Join-Path $env:LOCALAPPDATA 'Programs\\Herdr\\bin\\herdr.exe')"
                    .to_string(),
                platform,
            };
        }
        let install_suffix = ".local/bin/herdr".to_string();
        let shell_path = format!("\"$HOME/{install_suffix}\"");
        Self {
            install_suffix,
            shell_path,
            platform,
        }
    }

    fn with_shell_path(mut self, shell_path: String) -> Self {
        self.shell_path = shell_path;
        self
    }
}

fn powershell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn powershell_stdio_command(script: &str) -> String {
    let utf16 = script
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
    format!(
        "powershell.exe -NoLogo -NoProfile -NonInteractive -Command \"& ([scriptblock]::Create([Text.Encoding]::Unicode.GetString([Convert]::FromBase64String('{encoded}'))))\""
    )
}

fn powershell_herdr_script(remote_herdr: &RemoteHerdr, arguments: &[&str]) -> String {
    let arguments = arguments
        .iter()
        .map(|argument| powershell_quote(argument))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "$herdr = {path}; if (-not (Test-Path -LiteralPath $herdr -PathType Leaf)) {{ exit 127 }}; & $herdr @({arguments}); exit $LASTEXITCODE",
        path = remote_herdr.shell_path,
    )
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum RemoteAssetRef {
    Url(String),
    Object { url: String, sha256: Option<String> },
}

impl RemoteAssetRef {
    fn url(&self) -> &str {
        match self {
            Self::Url(url) => url,
            Self::Object { url, .. } => url,
        }
    }

    fn sha256(&self) -> Option<&str> {
        match self {
            Self::Url(_) => None,
            Self::Object { sha256, .. } => {
                sha256.as_deref().filter(|value| !value.trim().is_empty())
            }
        }
    }
}

#[derive(Deserialize)]
struct RemoteUpdateManifest {
    version: String,
    protocol: Option<u32>,
    assets: BTreeMap<String, RemoteAssetRef>,
    #[serde(default)]
    sha256: BTreeMap<String, String>,
    #[serde(default, deserialize_with = "deserialize_remote_manifest_releases")]
    releases: BTreeMap<String, RemoteReleaseMetadata>,
}

#[derive(Deserialize)]
struct RemoteReleaseMetadata {
    protocol: Option<u32>,
    #[serde(default)]
    assets: BTreeMap<String, RemoteAssetRef>,
    #[serde(default)]
    sha256: BTreeMap<String, String>,
}

fn deserialize_remote_manifest_releases<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, RemoteReleaseMetadata>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(serde_json::Value::Object(object)) => object
            .into_iter()
            .filter_map(|(version, release)| {
                serde_json::from_value::<RemoteReleaseMetadata>(release)
                    .ok()
                    .map(|metadata| (version, metadata))
            })
            .collect(),
        _ => BTreeMap::new(),
    })
}

impl RemoteUpdateManifest {
    fn release_for_version(&self, version: &str) -> Option<RemoteManifestReleaseRef<'_>> {
        if self.version.trim_start_matches('v') == version {
            return Some(RemoteManifestReleaseRef {
                protocol: self.protocol,
                assets: &self.assets,
                sha256: &self.sha256,
            });
        }

        self.releases.get(version).and_then(|release| {
            (!release.assets.is_empty()).then_some(RemoteManifestReleaseRef {
                protocol: release.protocol,
                assets: &release.assets,
                sha256: &release.sha256,
            })
        })
    }
}

#[derive(Clone, Copy)]
struct RemoteManifestReleaseRef<'a> {
    protocol: Option<u32>,
    assets: &'a BTreeMap<String, RemoteAssetRef>,
    sha256: &'a BTreeMap<String, String>,
}

fn current_version() -> String {
    crate::build_info::version()
}

fn current_channel() -> &'static str {
    crate::build_info::channel()
}

struct InstallSource {
    path: PathBuf,
    temporary_dir: Option<PathBuf>,
}

struct RemoteReleaseAsset {
    url: String,
    sha256: Option<String>,
}

struct PreparedRemoteHerdr {
    remote_herdr: RemoteHerdr,
    stop_after_install_approved: bool,
}

#[derive(Clone)]
struct ManagedSshOptions {
    config_path: PathBuf,
    control_path: Option<PathBuf>,
}

struct ManagedSshConfig {
    options: ManagedSshOptions,
}

impl Drop for ManagedSshConfig {
    fn drop(&mut self) {
        if let Some(dir) = self.options.config_path.parent() {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

struct RemoteSsh {
    target: String,
    managed_config: Option<ManagedSshConfig>,
    cancelled: Arc<AtomicBool>,
    batch_mode: bool,
}

impl RemoteSsh {
    fn new(target: String, manage_ssh_config: bool) -> Self {
        Self::with_cancel(
            target,
            manage_ssh_config,
            Arc::new(AtomicBool::new(false)),
            false,
        )
    }

    fn with_cancel(
        target: String,
        manage_ssh_config: bool,
        cancelled: Arc<AtomicBool>,
        batch_mode: bool,
    ) -> Self {
        let managed_config = if manage_ssh_config {
            write_managed_ssh_config()
                .inspect_err(|err| {
                    tracing::debug!(%err, "could not write managed ssh config; using plain ssh");
                })
                .ok()
        } else {
            None
        };

        Self {
            target,
            managed_config,
            cancelled,
            batch_mode,
        }
    }

    fn target(&self) -> &str {
        &self.target
    }

    fn options(&self) -> Option<&ManagedSshOptions> {
        self.managed_config.as_ref().map(|config| &config.options)
    }

    fn command(&self) -> Command {
        let mut command = self.base_command();
        if self.batch_mode {
            command
                .arg("-o")
                .arg("BatchMode=yes")
                .arg("-o")
                .arg("ConnectTimeout=10");
        }
        command.arg("-T").arg(&self.target);
        command
    }

    fn base_command(&self) -> Command {
        let mut command = Command::new("ssh");
        apply_managed_ssh_options(&mut command, self.options());
        command
    }

    fn sh_output(&self, script: &str) -> io::Result<Output> {
        let mut child = self
            .command()
            .arg("/bin/sh -s")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let script = script.as_bytes().to_vec();
        let write = if let Some(mut stdin) = child.stdin.take() {
            Some(thread::spawn(move || stdin.write_all(&script)))
        } else {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "ssh bootstrap stdin missing",
            ));
        };
        let output = self.wait_with_output(child);
        let write_result = write
            .expect("ssh bootstrap writer exists")
            .join()
            .map_err(|_| io::Error::other("ssh bootstrap writer panicked"))?;
        let output = output?;
        write_result?;
        Ok(output)
    }

    fn user_shell_output(&self, command: &str) -> io::Result<Output> {
        let child = self
            .command()
            .arg(command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        self.wait_with_output(child)
    }

    fn powershell_output(&self, script: &str) -> io::Result<Output> {
        self.user_shell_output(&powershell_stdio_command(script))
    }

    fn herdr_output(&self, remote_herdr: &RemoteHerdr, arguments: &[&str]) -> io::Result<Output> {
        if remote_herdr.platform.is_windows() {
            self.powershell_output(&powershell_herdr_script(remote_herdr, arguments))
        } else {
            let mut command = remote_herdr.shell_path.clone();
            for argument in arguments {
                command.push(' ');
                command.push_str(&shell_quote(argument));
            }
            self.sh_output(&command)
        }
    }

    fn install_herdr(&self, remote_herdr: &RemoteHerdr, source_path: &Path) -> io::Result<()> {
        if remote_herdr.platform.is_windows() {
            return self.install_windows_herdr(remote_herdr, source_path);
        }
        let output = self.sh_output(&remote_install_prepare_script(remote_herdr))?;
        if !output.status.success() {
            return Err(command_failed("remote install preparation failed", &output));
        }
        let (tmp_path, dest_path) = parse_remote_install_paths(&output.stdout)?;

        let mut child = self
            .command()
            .arg(remote_install_stream_command(&tmp_path))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|err| {
                io::Error::new(err.kind(), format!("failed to start ssh install: {err}"))
            })?;

        let mut source = File::open(source_path)?;
        let stdin = child.stdin.take().ok_or_else(|| {
            io::Error::new(io::ErrorKind::BrokenPipe, "ssh install stdin missing")
        })?;
        let copy = thread::spawn(move || {
            let mut stdin = stdin;
            io::copy(&mut source, &mut stdin).map(|_| ())
        });
        let status = self.wait(&mut child);
        let copy_result = copy
            .join()
            .map_err(|_| io::Error::other("ssh install upload worker panicked"))?;
        let status = status?;
        copy_result?;

        if status.success() {
            let output = self.sh_output(&remote_install_commit_script(&tmp_path, &dest_path))?;
            if output.status.success() {
                Ok(())
            } else {
                Err(command_failed("remote install commit failed", &output))
            }
        } else {
            Err(io::Error::other(format!(
                "remote install exited with {status}"
            )))
        }
    }

    fn install_windows_herdr(
        &self,
        remote_herdr: &RemoteHerdr,
        source_path: &Path,
    ) -> io::Result<()> {
        let output = self.powershell_output(windows_install_prepare_script())?;
        if !output.status.success() {
            return Err(command_failed("remote install preparation failed", &output));
        }
        let (package_path, installer_path) = parse_remote_install_paths(&output.stdout)?;
        let temp_dir = package_path
            .rsplit_once('\\')
            .or_else(|| package_path.rsplit_once('/'))
            .map(|(parent, _)| parent.to_owned())
            .ok_or_else(|| io::Error::other("remote installer returned an invalid temp path"))?;

        let result = (|| {
            self.copy_path_to_windows_file(&package_path, source_path)?;
            self.stream_to_windows_file(
                &installer_path,
                io::Cursor::new(include_bytes!("../../distribution/install.ps1")),
            )?;

            let sha256 = crate::checksum::file_sha256(source_path)?;
            let identity = format!("{}-remote-{}", current_version(), &sha256[..12]);
            let install_dir = "(Join-Path $env:LOCALAPPDATA 'Programs\\Herdr\\bin')";
            let script = format!(
                "$ErrorActionPreference = 'Stop'; try {{ & {installer} -InstallDir {install_dir} -Retain 3 -LocalPackagePath {package} -LocalPackageFormat 'zip' -LocalPackageIdentity {identity} -LocalPackageSha256 {sha256}; if ($LASTEXITCODE -ne 0) {{ exit $LASTEXITCODE }} }} finally {{ Remove-Item -LiteralPath {temp_dir} -Recurse -Force -ErrorAction SilentlyContinue }}",
                installer = powershell_quote(&installer_path),
                package = powershell_quote(&package_path),
                identity = powershell_quote(&identity),
                sha256 = powershell_quote(&sha256),
                temp_dir = powershell_quote(&temp_dir),
            );
            let output = self.powershell_output(&script)?;
            if !output.status.success() {
                return Err(command_failed("remote Windows install failed", &output));
            }
            if !remote_binary_exists(self, remote_herdr)? {
                return Err(io::Error::other(
                    "remote Windows installer completed without activating herdr.exe",
                ));
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = self.powershell_output(&format!(
                "Remove-Item -LiteralPath {} -Recurse -Force -ErrorAction SilentlyContinue",
                powershell_quote(&temp_dir)
            ));
        }
        result
    }

    fn copy_path_to_windows_file(&self, remote_path: &str, source_path: &Path) -> io::Result<()> {
        let mut command = Command::new("scp");
        if let Some(options) = self.options() {
            command.arg("-F").arg(&options.config_path);
        }
        command
            .arg("-q")
            .arg("-o")
            .arg("ControlMaster=no")
            .arg("-o")
            .arg("ControlPath=none");
        if self.batch_mode {
            command
                .arg("-o")
                .arg("BatchMode=yes")
                .arg("-o")
                .arg("ConnectTimeout=10");
        }
        let child = command
            .arg(source_path)
            .arg(windows_scp_target(&self.target, remote_path))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let output = self.wait_with_output(child)?;
        if output.status.success() {
            Ok(())
        } else {
            Err(command_failed("remote Windows upload failed", &output))
        }
    }

    fn stream_to_windows_file<R>(&self, remote_path: &str, mut source: R) -> io::Result<()>
    where
        R: io::Read + Send + 'static,
    {
        let script = format!(
            "$output = [IO.File]::Open({path}, [IO.FileMode]::Create, [IO.FileAccess]::Write, [IO.FileShare]::None); try {{ [Console]::OpenStandardInput().CopyTo($output) }} finally {{ $output.Dispose() }}",
            path = powershell_quote(remote_path)
        );
        let mut child = self
            .command()
            .arg(powershell_stdio_command(&script))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "ssh upload stdin missing"))?;
        let write = thread::spawn(move || io::copy(&mut source, &mut stdin).map(|_| ()));
        let output = self.wait_with_output(child);
        let write_result = write
            .join()
            .map_err(|_| io::Error::other("remote Windows upload worker panicked"))?;
        let output = output?;
        write_result?;
        if output.status.success() {
            Ok(())
        } else {
            Err(command_failed("remote Windows upload failed", &output))
        }
    }

    fn wait(&self, child: &mut Child) -> io::Result<ExitStatus> {
        loop {
            if self.cancelled.load(Ordering::Acquire) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "remote SSH operation cancelled",
                ));
            }
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            thread::sleep(SSH_CHILD_POLL);
        }
    }

    fn wait_with_output(&self, mut child: Child) -> io::Result<Output> {
        let stdout = child.stdout.take().map(|mut stdout| {
            thread::spawn(move || {
                let mut bytes = Vec::new();
                stdout.read_to_end(&mut bytes).map(|_| bytes)
            })
        });
        let stderr = child.stderr.take().map(|mut stderr| {
            thread::spawn(move || {
                let mut bytes = Vec::new();
                stderr.read_to_end(&mut bytes).map(|_| bytes)
            })
        });
        let status = self.wait(&mut child);
        let join = |worker: Option<thread::JoinHandle<io::Result<Vec<u8>>>>| {
            worker
                .map(|worker| {
                    worker
                        .join()
                        .map_err(|_| io::Error::other("ssh output reader panicked"))?
                })
                .transpose()
                .map(|bytes| bytes.unwrap_or_default())
        };
        let stdout = join(stdout);
        let stderr = join(stderr);
        Ok(Output {
            status: status?,
            stdout: stdout?,
            stderr: stderr?,
        })
    }
}

fn remote_install_prepare_script(remote_herdr: &RemoteHerdr) -> String {
    format!(
        r#"set -eu
dest="$HOME/{install_suffix}"
dir="${{dest%/*}}"
mkdir -p "$dir"
tmp="${{dest}}.tmp.$$"
printf '%s\0%s\0' "$tmp" "$dest"
"#,
        install_suffix = remote_herdr.install_suffix
    )
}

fn windows_install_prepare_script() -> &'static str {
    r#"$dir = Join-Path ([IO.Path]::GetTempPath()) ('herdr-remote-' + [guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($dir) | Out-Null
$package = Join-Path $dir 'herdr.zip'
$installer = Join-Path $dir 'install.ps1'
[Console]::Out.Write($package + [char]0 + $installer + [char]0)"#
}

fn windows_scp_target(target: &str, remote_path: &str) -> String {
    format!("{target}:{}", remote_path.replace('\\', "/"))
}

fn parse_remote_install_paths(stdout: &[u8]) -> io::Result<(String, String)> {
    let mut parts = stdout.split(|byte| *byte == 0);
    let tmp_path = parts.next().unwrap_or_default();
    let dest_path = parts.next().unwrap_or_default();
    if tmp_path.is_empty() || dest_path.is_empty() {
        return Err(io::Error::other(
            "remote install preparation did not return destination paths",
        ));
    }
    let tmp_path = String::from_utf8(tmp_path.to_vec()).map_err(|err| {
        io::Error::other(format!(
            "remote install temporary path is not valid UTF-8: {err}"
        ))
    })?;
    let dest_path = String::from_utf8(dest_path.to_vec()).map_err(|err| {
        io::Error::other(format!(
            "remote install destination path is not valid UTF-8: {err}"
        ))
    })?;
    Ok((tmp_path, dest_path))
}

fn remote_install_stream_command(tmp_path: &str) -> String {
    format!("tee {}", shell_quote(tmp_path))
}

fn remote_install_commit_script(tmp_path: &str, dest_path: &str) -> String {
    format!(
        "set -eu\nchmod 755 {tmp_path}\nmv {tmp_path} {dest_path}\n",
        tmp_path = shell_quote(tmp_path),
        dest_path = shell_quote(dest_path)
    )
}

impl Drop for RemoteSsh {
    fn drop(&mut self) {
        let Some(_options) = self
            .managed_config
            .as_ref()
            .map(|config| &config.options)
            .filter(|options| options.control_path.is_some())
        else {
            return;
        };

        let Ok(mut child) = self
            .base_command()
            .arg("-O")
            .arg("exit")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg(&self.target)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            return;
        };
        terminate_child_after(&mut child, Duration::from_secs(1));
    }
}

fn terminate_child_after(child: &mut Child, timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => break,
            Ok(None) if std::time::Instant::now() < deadline => thread::sleep(SSH_CHILD_POLL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
}

fn apply_managed_ssh_options(command: &mut Command, options: Option<&ManagedSshOptions>) {
    let Some(options) = options else {
        return;
    };

    command.arg("-F").arg(&options.config_path);
    if let Some(control_path) = &options.control_path {
        command
            .arg("-S")
            .arg(control_path)
            .arg("-o")
            .arg("ControlMaster=auto")
            .arg("-o")
            .arg("ControlPersist=yes");
    }
    command
        .arg("-o")
        .arg("ServerAliveInterval=15")
        .arg("-o")
        .arg("ServerAliveCountMax=4");
}

impl InstallSource {
    fn persistent(path: PathBuf) -> Self {
        Self {
            path,
            temporary_dir: None,
        }
    }

    fn temporary(path: PathBuf, temporary_dir: PathBuf) -> Self {
        Self {
            path,
            temporary_dir: Some(temporary_dir),
        }
    }

    fn cleanup(&self) {
        if let Some(dir) = &self.temporary_dir {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

fn prepare_remote_herdr(
    ssh: &RemoteSsh,
    session_name: &str,
    live_handoff_enabled: bool,
    allow_install: bool,
) -> io::Result<PreparedRemoteHerdr> {
    let platform = detect_remote_platform(ssh)?;
    let remote_herdr = RemoteHerdr::for_platform(platform);
    let override_binary = remote_binary_override_path()?;
    let remote_binary_candidates = remote_binary_candidates(ssh, &remote_herdr)?;

    let mut incompatible_binary_found = false;
    if override_binary.is_none() {
        for candidate in &remote_binary_candidates {
            if remote_binary_supports_endpoint(ssh, candidate)? {
                return Ok(PreparedRemoteHerdr {
                    remote_herdr: candidate.clone(),
                    stop_after_install_approved: false,
                });
            }
            incompatible_binary_found = true;
        }
        if remote_binary_supports_endpoint(ssh, &remote_herdr)? {
            return Ok(PreparedRemoteHerdr {
                remote_herdr,
                stop_after_install_approved: false,
            });
        }
        incompatible_binary_found |= remote_binary_exists(ssh, &remote_herdr)?;
    }

    if !allow_install {
        let provision_command = remote_provision_command(ssh.target(), session_name);
        if incompatible_binary_found {
            return Err(remote_compatibility_error(format!(
                "Herdr on {} does not support endpoint generation {}; run `{}` once to provision a compatible version",
                ssh.target(),
                crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION,
                provision_command
            )));
        }
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "no compatible Herdr endpoint is installed on {}; run `{}` once to provision it",
                ssh.target(),
                provision_command
            ),
        ));
    }

    let mut stop_after_install_approved = false;
    if let Some(status_probe_herdr) = remote_binary_candidates.first().or_else(|| {
        remote_binary_exists(ssh, &remote_herdr)
            .ok()
            .and_then(|exists| exists.then_some(&remote_herdr))
    }) {
        stop_after_install_approved = confirm_remote_install_with_running_server(
            ssh,
            status_probe_herdr,
            session_name,
            live_handoff_enabled,
        )?;
    }
    confirm_remote_install(
        ssh.target(),
        &remote_herdr,
        &install_source_description(&remote_herdr.platform, override_binary.as_deref()),
    )?;
    let source = resolve_install_source(&remote_herdr.platform, override_binary)?;
    let install_result = ssh.install_herdr(&remote_herdr, &source.path);
    source.cleanup();
    install_result?;

    if !remote_binary_supports_endpoint(ssh, &remote_herdr)? {
        return Err(remote_compatibility_error(format!(
            "installed remote herdr at {}, but it does not support endpoint generation {}",
            remote_herdr.shell_path,
            crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION
        )));
    }
    warn_if_remote_bin_not_on_path(ssh, &remote_herdr)?;

    Ok(PreparedRemoteHerdr {
        remote_herdr,
        stop_after_install_approved,
    })
}

fn detect_remote_platform(ssh: &RemoteSsh) -> io::Result<RemotePlatform> {
    let unix_output = ssh.user_shell_output("uname -s; uname -m")?;
    if unix_output.status.success() {
        let stdout = String::from_utf8_lossy(&unix_output.stdout);
        let mut lines = stdout.lines();
        if let Some(platform) = RemotePlatform::from_uname(
            lines.next().unwrap_or_default(),
            lines.next().unwrap_or_default(),
        ) {
            return Ok(platform);
        }
    }

    let windows_output = ssh.powershell_output(
        "[Console]::Out.Write('Windows' + [char]10 + $env:PROCESSOR_ARCHITECTURE)",
    )?;
    if !windows_output.status.success() {
        return Err(command_failed(
            "remote platform detection failed",
            &unix_output,
        ));
    }
    let stdout = String::from_utf8_lossy(&windows_output.stdout);
    let mut lines = stdout.lines();
    let os = lines.next().unwrap_or_default();
    let arch = lines.next().unwrap_or_default();
    RemotePlatform::from_uname(os, arch).ok_or_else(|| {
        io::Error::other(format!(
            "unsupported remote platform: {} {}",
            os.trim(),
            arch.trim()
        ))
    })
}

fn remote_binary_candidates(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
) -> io::Result<Vec<RemoteHerdr>> {
    let mut candidates = Vec::new();

    if let Some(path_candidate) = remote_binary_on_path_any(ssh, remote_herdr)? {
        push_if_new_remote_binary_candidate(&mut candidates, path_candidate);
    }

    let output = if remote_herdr.platform.is_windows() {
        ssh.powershell_output(&known_windows_remote_binary_candidate_script())?
    } else {
        ssh.sh_output(&known_remote_binary_candidate_script(
            &remote_herdr.platform,
        ))?
    };
    if !output.status.success() {
        return Err(command_failed("remote binary discovery failed", &output));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for candidate in remote_herdrs_from_path_discovery(remote_herdr, &stdout) {
        push_if_new_remote_binary_candidate(&mut candidates, candidate);
    }

    Ok(candidates)
}

fn known_windows_remote_binary_candidate_script() -> String {
    r#"$paths = @(
    (Join-Path $env:LOCALAPPDATA 'Programs\Herdr\bin\herdr.exe'),
    (Join-Path $env:USERPROFILE '.herdr\remote\current\herdr.exe')
)
foreach ($path in $paths) {
    if (Test-Path -LiteralPath $path -PathType Leaf) {
        [Console]::Out.WriteLine([IO.Path]::GetFullPath($path))
    }
}"#
    .to_string()
}

fn push_if_new_remote_binary_candidate(candidates: &mut Vec<RemoteHerdr>, candidate: RemoteHerdr) {
    if !candidates
        .iter()
        .any(|existing| existing.shell_path == candidate.shell_path)
    {
        candidates.push(candidate);
    }
}

fn known_remote_binary_candidate_script(platform: &RemotePlatform) -> String {
    let mut script = String::from(
        r#"home=${HOME:-}
user=${USER:-}
version="#,
    );
    script.push_str(&shell_quote(&current_version()));
    script.push_str(
        r#"
emit() {
    path=$1
    if [ -n "$path" ] && [ -x "$path" ]; then
        printf '%s\n' "$path"
    fi
}
if [ -n "$home" ]; then
    emit "$home/.local/bin/herdr"
fi
"#,
    );
    if platform.os == "macos" {
        script.push_str(
            r#"    emit "/opt/homebrew/bin/herdr"
    emit "/usr/local/bin/herdr"
"#,
        );
    } else if platform.os == "linux" {
        script.push_str(
            r#"    emit "/home/linuxbrew/.linuxbrew/bin/herdr"
"#,
        );
    }
    script.push_str(
        r#"if [ -n "$home" ]; then
    emit "$home/.local/share/mise/installs/herdr/$version/bin/herdr"
    emit "$home/.local/share/mise/installs/herdr/$version/herdr"
    emit "$home/.nix-profile/bin/herdr"
fi
if [ -n "$user" ]; then
    emit "/etc/profiles/per-user/$user/bin/herdr"
fi
emit "/nix/var/nix/profiles/default/bin/herdr"
emit "/run/current-system/sw/bin/herdr"
"#,
    );

    script
}

fn remote_binary_on_path_any(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
) -> io::Result<Option<RemoteHerdr>> {
    if remote_herdr.platform.is_windows() {
        let output = ssh.powershell_output(
            "$command = Get-Command herdr.exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1; if ($null -ne $command) { [Console]::Out.WriteLine($command.Source) } else { exit 1 }",
        )?;
        return Ok(output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout))
            .as_deref()
            .and_then(|stdout| remote_herdr_from_path_discovery(remote_herdr, stdout)));
    }
    let output = ssh.user_shell_output("command -v herdr")?;
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Some(candidate) = remote_herdr_from_path_discovery(remote_herdr, &stdout) {
            return Ok(Some(candidate));
        }
    }

    // Non-POSIX login shells such as xonsh reject `command -v`; retry through
    // /bin/sh while retaining the login-shell probe for shell-initialized PATHs.
    let output = ssh.sh_output("command -v herdr\n")?;
    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(remote_herdr_from_path_discovery(remote_herdr, &stdout))
}

fn remote_herdrs_from_path_discovery(remote_herdr: &RemoteHerdr, stdout: &str) -> Vec<RemoteHerdr> {
    stdout
        .lines()
        .filter_map(|path| remote_herdr_from_path(remote_herdr, path))
        .collect()
}

fn remote_herdr_from_path_discovery(
    remote_herdr: &RemoteHerdr,
    stdout: &str,
) -> Option<RemoteHerdr> {
    stdout
        .lines()
        .find_map(|path| remote_herdr_from_path(remote_herdr, path))
}

fn remote_herdr_from_path(remote_herdr: &RemoteHerdr, path: &str) -> Option<RemoteHerdr> {
    let path = path.trim();
    if remote_herdr.platform.is_windows() {
        let bytes = path.as_bytes();
        let drive_absolute = bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/');
        if !drive_absolute && !path.starts_with(r"\\") {
            return None;
        }
        return Some(remote_herdr.clone().with_shell_path(powershell_quote(path)));
    }
    if !path.starts_with('/') {
        return None;
    }
    if is_mise_shim_path(path) {
        return None;
    }
    Some(remote_herdr.clone().with_shell_path(shell_quote(path)))
}

fn is_mise_shim_path(path: &str) -> bool {
    path.ends_with("/mise/shims/herdr")
}

fn remote_client_status(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
) -> io::Result<Option<RemoteClientStatusJson>> {
    let output = ssh.herdr_output(remote_herdr, &["status", "client", "--json"])?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(parse_client_status_json(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

fn remote_binary_supports_endpoint(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
) -> io::Result<bool> {
    Ok(remote_client_status(ssh, remote_herdr)?
        .and_then(|status| status.endpoint_protocol_generation)
        == Some(crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION))
}

fn remote_binary_exists(ssh: &RemoteSsh, remote_herdr: &RemoteHerdr) -> io::Result<bool> {
    if remote_herdr.platform.is_windows() {
        return Ok(ssh
            .powershell_output(&format!(
                "if (Test-Path -LiteralPath {path} -PathType Leaf) {{ exit 0 }} else {{ exit 1 }}",
                path = remote_herdr.shell_path
            ))?
            .status
            .success());
    }
    let command = format!("test -x {}", remote_herdr.shell_path);
    Ok(ssh.sh_output(&command)?.status.success())
}

fn remote_binary_override_path() -> io::Result<Option<PathBuf>> {
    let Some(value) = std::env::var_os(REMOTE_BINARY_ENV_VAR) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{REMOTE_BINARY_ENV_VAR} must not be empty"),
        ));
    }

    let path = PathBuf::from(value);
    let metadata = fs::metadata(&path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "failed to inspect {REMOTE_BINARY_ENV_VAR} path {}: {err}",
                path.display()
            ),
        )
    })?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{REMOTE_BINARY_ENV_VAR} path is not a file: {}",
                path.display()
            ),
        ));
    }

    Ok(Some(path))
}

fn install_source_description(platform: &RemotePlatform, override_binary: Option<&Path>) -> String {
    install_source_description_for(
        platform,
        override_binary,
        local_binary_can_seed_remote(platform),
    )
}

fn install_source_description_for(
    platform: &RemotePlatform,
    override_binary: Option<&Path>,
    local_binary_can_seed_remote: bool,
) -> String {
    if let Some(path) = override_binary {
        return format!("{REMOTE_BINARY_ENV_VAR} ({})", path.display());
    }

    if local_binary_can_seed_remote {
        "the current local herdr binary".to_string()
    } else {
        format!(
            "the {} {} asset for {}",
            current_version(),
            current_channel(),
            platform.asset_key()
        )
    }
}

fn resolve_install_source(
    platform: &RemotePlatform,
    override_binary: Option<PathBuf>,
) -> io::Result<InstallSource> {
    if let Some(path) = override_binary {
        if platform.is_windows()
            && !path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{REMOTE_BINARY_ENV_VAR} must point to a packaged Herdr .zip for Windows remotes so the required ConPTY runtime is installed"
                ),
            ));
        }
        return Ok(InstallSource::persistent(path));
    }

    if !platform.is_windows() && *platform == RemotePlatform::local() {
        let path = std::env::current_exe()?;
        if !crate::update::is_package_manager_managed_exe_path(&path) {
            return Ok(InstallSource::persistent(path));
        }
    }

    download_release_asset(platform)
}

fn local_binary_can_seed_remote(platform: &RemotePlatform) -> bool {
    if platform.is_windows() || *platform != RemotePlatform::local() {
        return false;
    }

    std::env::current_exe()
        .map(|path| !crate::update::is_package_manager_managed_exe_path(&path))
        .unwrap_or(false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RemoteServerStatus {
    Running {
        version: Option<String>,
        endpoint_protocol_generation: Option<u32>,
        live_handoff: bool,
        detached_server_daemon: bool,
    },
    NotRunning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteServerRestartReason {
    EndpointProtocolMissing,
    DaemonDetachMissing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteInstallRunningServerPlan {
    KeepRunning,
    LiveHandoff,
    StopRequired(RemoteServerRestartReason),
}

fn ensure_remote_server_ready(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
    session_name: &str,
    stop_after_install_approved: bool,
    live_handoff_enabled: bool,
    interactive: bool,
) -> io::Result<()> {
    let status = remote_server_status(ssh, remote_herdr, session_name)?;
    let RemoteServerStatus::Running {
        version,
        endpoint_protocol_generation,
        live_handoff,
        detached_server_daemon,
    } = status
    else {
        return Ok(());
    };

    let Some(reason) =
        remote_server_restart_reason(endpoint_protocol_generation, detached_server_daemon)
    else {
        return Ok(());
    };

    if live_handoff_enabled && live_handoff {
        match live_handoff_remote_server(ssh, remote_herdr, session_name) {
            Ok(()) => return Ok(()),
            Err(err) => {
                eprintln!("remote live handoff failed: {err}");
                eprintln!("falling back to remote server restart.");
            }
        }
    }

    if stop_after_install_approved {
        stop_remote_server(ssh, remote_herdr, session_name)?;
        return Ok(());
    }

    if !interactive {
        let provision_command = remote_provision_command(ssh.target(), session_name);
        return Err(remote_compatibility_error(format!(
            "remote herdr server on {} needs an interactive update; run `{}` once to provision it",
            ssh.target(),
            provision_command
        )));
    }

    if confirm_remote_server_stop(ssh.target(), version.as_deref(), reason)? {
        stop_remote_server(ssh, remote_herdr, session_name)?;
    }
    Ok(())
}

fn remote_server_restart_reason(
    endpoint_protocol_generation: Option<u32>,
    detached_server_daemon: bool,
) -> Option<RemoteServerRestartReason> {
    if endpoint_protocol_generation != Some(crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION)
    {
        return Some(RemoteServerRestartReason::EndpointProtocolMissing);
    }
    if !detached_server_daemon {
        return Some(RemoteServerRestartReason::DaemonDetachMissing);
    }
    None
}

fn confirm_remote_install_with_running_server(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
    session_name: &str,
    live_handoff_enabled: bool,
) -> io::Result<bool> {
    let target = ssh.target();
    let status = match remote_server_status(ssh, remote_herdr, session_name) {
        Ok(status) => status,
        Err(err) => {
            if !io::stdin().is_terminal() {
                return Err(io::Error::other(format!(
                    "could not inspect the running remote herdr server on {target} before installing: {err}; run from an interactive terminal to approve updating the remote binary"
                )));
            }
            eprintln!(
                "could not inspect the running remote herdr server on {target} before installing: {err}"
            );
            eprint!("continue installing the remote herdr binary? [y/N] ");
            io::stderr().flush()?;

            let mut answer = String::new();
            io::stdin().read_line(&mut answer)?;
            let answer = answer.trim().to_ascii_lowercase();
            if answer != "y" && answer != "yes" {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "remote herdr install cancelled",
                ));
            }
            return Ok(false);
        }
    };
    let RemoteServerStatus::Running {
        version,
        endpoint_protocol_generation,
        live_handoff,
        detached_server_daemon,
    } = &status
    else {
        return Ok(false);
    };
    let plan = remote_install_running_server_plan(
        *endpoint_protocol_generation,
        *detached_server_daemon,
        *live_handoff,
        live_handoff_enabled,
    );

    if plan == RemoteInstallRunningServerPlan::KeepRunning {
        if io::stdin().is_terminal() {
            eprintln!("remote herdr server on {target} is already compatible:");
            eprintln!("  server: v{}", version_label(version.as_deref()));
            eprintln!(
                "Herdr will install {} without stopping the running remote server.",
                current_version()
            );
        }
        return Ok(false);
    }

    if !io::stdin().is_terminal() {
        match plan {
            RemoteInstallRunningServerPlan::LiveHandoff => return Ok(false),
            RemoteInstallRunningServerPlan::StopRequired(_) => {
                return Err(io::Error::other(format!(
                    "remote herdr server on {target} is running v{}; run from an interactive terminal to approve stopping it for the update",
                    version_label(version.as_deref())
                )));
            }
            RemoteInstallRunningServerPlan::KeepRunning => return Ok(false),
        }
    }

    if plan == RemoteInstallRunningServerPlan::LiveHandoff {
        eprintln!("remote herdr server on {target} is currently running:");
        eprintln!("  server: v{}", version_label(version.as_deref()));
        eprintln!(
            "Herdr will install {} and hand off live pane processes to the prepared server.",
            current_version()
        );
        return Ok(false);
    }

    eprintln!("remote herdr server on {target} is currently running:");
    eprintln!("  server: v{}", version_label(version.as_deref()));
    eprintln!(
        "To complete the remote update, Herdr must stop the running remote server after installing."
    );
    eprintln!("This stops active remote pane processes, including shells, dev servers, and tests.");
    eprintln!();
    eprint!(
        "Install {} and stop the remote server now? [y/N] ",
        current_version()
    );
    io::stderr().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    let answer = answer.trim().to_ascii_lowercase();
    if answer != "y" && answer != "yes" {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "remote herdr install cancelled",
        ));
    }

    Ok(true)
}

fn remote_install_running_server_plan(
    endpoint_protocol_generation: Option<u32>,
    detached_server_daemon: bool,
    live_handoff: bool,
    live_handoff_enabled: bool,
) -> RemoteInstallRunningServerPlan {
    let Some(reason) =
        remote_server_restart_reason(endpoint_protocol_generation, detached_server_daemon)
    else {
        return RemoteInstallRunningServerPlan::KeepRunning;
    };

    if live_handoff_enabled && live_handoff {
        return RemoteInstallRunningServerPlan::LiveHandoff;
    }

    RemoteInstallRunningServerPlan::StopRequired(reason)
}

fn remote_server_status(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
    session_name: &str,
) -> io::Result<RemoteServerStatus> {
    let arguments = session_scoped_arguments(session_name, &["status", "server", "--json"]);
    let output = ssh.herdr_output(remote_herdr, &arguments)?;
    if !output.status.success() {
        return Err(command_failed("remote server status failed", &output));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_remote_server_status_json(stdout.trim())
}

fn session_scoped_arguments<'a>(session_name: &'a str, arguments: &[&'a str]) -> Vec<&'a str> {
    let mut scoped = Vec::with_capacity(arguments.len() + 2);
    if session_name != crate::session::DEFAULT_SESSION_NAME {
        scoped.extend(["--session", session_name]);
    }
    scoped.extend_from_slice(arguments);
    scoped
}

#[derive(Debug, Deserialize)]
struct RemoteClientStatusJson {
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    protocol: Option<u32>,
    #[serde(default)]
    endpoint_protocol_generation: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct RemoteServerStatusJson {
    running: bool,
    version: Option<String>,
    capabilities: Option<RemoteServerCapabilitiesJson>,
}

#[derive(Debug, Deserialize)]
struct RemoteServerCapabilitiesJson {
    live_handoff: bool,
    #[serde(default)]
    detached_server_daemon: bool,
    #[serde(default)]
    endpoint_protocol_generation: Option<u32>,
}

fn parse_client_status_json(status: &str) -> Option<RemoteClientStatusJson> {
    status
        .lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<RemoteClientStatusJson>(line).ok())
        .find(|status| {
            status.version.is_some()
                || status.protocol.is_some()
                || status.endpoint_protocol_generation.is_some()
        })
}

fn parse_remote_server_status_json(status: &str) -> io::Result<RemoteServerStatus> {
    let parsed: RemoteServerStatusJson = serde_json::from_str(status).map_err(|err| {
        io::Error::other(format!(
            "could not parse remote server status JSON from `{status}`: {err}"
        ))
    })?;
    if !parsed.running {
        return Ok(RemoteServerStatus::NotRunning);
    }

    let capabilities = parsed.capabilities;

    Ok(RemoteServerStatus::Running {
        version: parsed.version,
        endpoint_protocol_generation: capabilities
            .as_ref()
            .and_then(|capabilities| capabilities.endpoint_protocol_generation),
        live_handoff: capabilities
            .as_ref()
            .is_some_and(|capabilities| capabilities.live_handoff),
        detached_server_daemon: capabilities
            .as_ref()
            .is_some_and(|capabilities| capabilities.detached_server_daemon),
    })
}

fn confirm_remote_server_stop(
    target: &str,
    version: Option<&str>,
    reason: RemoteServerRestartReason,
) -> io::Result<bool> {
    if !io::stdin().is_terminal() {
        if reason == RemoteServerRestartReason::EndpointProtocolMissing {
            return Err(io::Error::other(format!(
                "remote herdr server on {target} needs one final update before this client can attach; run from an interactive terminal to approve updating it"
            )));
        }

        eprintln!(
            "remote herdr server on {target} is still running v{}; it will use {} after it restarts.",
            version_label(version),
            current_version()
        );
        return Ok(false);
    }

    eprintln!("remote herdr server on {target} is currently running:");
    eprintln!("  server: v{}", version_label(version));
    eprintln!("  prepared binary: {}", current_version());
    eprintln!();

    match reason {
        RemoteServerRestartReason::EndpointProtocolMissing => {
            eprintln!(
                "the remote server predates Herdr's stable endpoint protocol and must update before this client can attach."
            );
        }
        RemoteServerRestartReason::DaemonDetachMissing => {
            eprintln!(
                "the remote server was started by a herdr build that may not survive SSH connection loss. restart it so network drops disconnect only this client."
            );
        }
    }

    let prompt = if reason == RemoteServerRestartReason::EndpointProtocolMissing {
        "update the remote server and continue attaching? [Y/n] "
    } else {
        "restart the remote server now? [y/N] "
    };
    eprint!("{prompt}");
    io::stderr().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    let answer = answer.trim().to_ascii_lowercase();
    if answer == "y" || answer == "yes" {
        return Ok(true);
    }
    if answer.is_empty() && reason == RemoteServerRestartReason::EndpointProtocolMissing {
        return Ok(true);
    }
    if reason == RemoteServerRestartReason::EndpointProtocolMissing {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "remote herdr server stop cancelled",
        ));
    }

    Ok(false)
}

fn remote_live_handoff_command(
    remote_herdr: &RemoteHerdr,
    session_name: &str,
    protocol: u32,
    version: &str,
) -> String {
    let session = if session_name == crate::session::DEFAULT_SESSION_NAME {
        String::new()
    } else {
        format!(" --session {}", shell_quote(session_name))
    };
    format!(
        "{}{session} server live-handoff --import-exe {} --expected-protocol {} --expected-version {}",
        remote_herdr.shell_path, remote_herdr.shell_path, protocol, version
    )
}

fn live_handoff_remote_server(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
    session_name: &str,
) -> io::Result<()> {
    let status = remote_client_status(ssh, remote_herdr)?.ok_or_else(|| {
        io::Error::other("could not inspect the prepared remote herdr binary before live handoff")
    })?;
    let protocol = status.protocol.ok_or_else(|| {
        io::Error::other("prepared remote herdr did not report its private protocol")
    })?;
    let version = status
        .version
        .filter(|version| !version.is_empty())
        .ok_or_else(|| io::Error::other("prepared remote herdr did not report its version"))?;
    let protocol_argument = protocol.to_string();
    let output = if remote_herdr.platform.is_windows() {
        let script = format!(
            "$herdr = {path}; & $herdr '--session' {session} 'server' 'live-handoff' '--import-exe' $herdr '--expected-protocol' {protocol} '--expected-version' {version}; exit $LASTEXITCODE",
            path = remote_herdr.shell_path,
            session = powershell_quote(session_name),
            protocol = powershell_quote(&protocol_argument),
            version = powershell_quote(&version),
        );
        ssh.powershell_output(&script)?
    } else {
        let command = remote_live_handoff_command(remote_herdr, session_name, protocol, &version);
        ssh.sh_output(&command)?
    };
    if !output.status.success() {
        return Err(command_failed("remote server live handoff failed", &output));
    }

    eprintln!(
        "handed off the remote herdr server on {}; reconnecting to the prepared server.",
        ssh.target()
    );
    Ok(())
}

fn stop_remote_server(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
    session_name: &str,
) -> io::Result<()> {
    let arguments = session_scoped_arguments(session_name, &["server", "stop"]);
    let output = ssh.herdr_output(remote_herdr, &arguments)?;
    if !output.status.success() {
        return Err(command_failed("remote server stop failed", &output));
    }

    wait_for_remote_server_shutdown(ssh, remote_herdr, session_name)?;
    eprintln!(
        "stopped the remote herdr server on {}; it will restart when the remote client bridge attaches.",
        ssh.target()
    );
    Ok(())
}

fn wait_for_remote_server_shutdown(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
    session_name: &str,
) -> io::Result<()> {
    let deadline = Instant::now() + REMOTE_SERVER_SHUTDOWN_CONFIRM_TIMEOUT;
    loop {
        if remote_server_status(ssh, remote_herdr, session_name)? == RemoteServerStatus::NotRunning
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "shutdown was requested, but the old remote herdr server on {target} is still responding after {} seconds",
                    REMOTE_SERVER_SHUTDOWN_CONFIRM_TIMEOUT.as_secs(),
                    target = ssh.target()
                ),
            ));
        }
        thread::sleep(REMOTE_SERVER_SHUTDOWN_POLL_INTERVAL);
    }
}

fn version_label(version: Option<&str>) -> &str {
    version.unwrap_or("unknown")
}

fn warn_if_remote_bin_not_on_path(ssh: &RemoteSsh, remote_herdr: &RemoteHerdr) -> io::Result<()> {
    // Windows remote installs use a stable absolute package junction, so the
    // current SSH process does not need to observe the installer's PATH update.
    if remote_herdr.platform.is_windows() {
        return Ok(());
    }
    let output = ssh.user_shell_output("command -v herdr")?;
    if output.status.success()
        && remote_shell_resolves_managed_install(&String::from_utf8_lossy(&output.stdout))
    {
        return Ok(());
    }

    eprintln!(
        "herdr: installed remote binary to ~/.local/bin/herdr, but the remote shell does not resolve `herdr` to that path"
    );
    Ok(())
}

fn remote_shell_resolves_managed_install(stdout: &str) -> bool {
    stdout
        .lines()
        .next()
        .map(str::trim)
        .is_some_and(|path| path.ends_with("/.local/bin/herdr"))
}

fn download_release_asset(platform: &RemotePlatform) -> io::Result<InstallSource> {
    let asset_key = platform.asset_key();
    let asset = remote_release_asset(&asset_key)?;

    let dir = private_download_dir(&asset_key)?;
    let path = dir.join("herdr.tmp");
    let status = crate::noninteractive_process::curl_command()
        .args(["-sfL", "--max-time", "120", "-o"])
        .arg(&path)
        .arg(&asset.url)
        .status()
        .map_err(|err| io::Error::new(err.kind(), format!("download failed: {err}")))?;
    if !status.success() {
        let _ = fs::remove_dir_all(&dir);
        return Err(io::Error::other("download failed"));
    }
    if let Some(expected) = &asset.sha256 {
        if let Err(err) = crate::checksum::verify_sha256(&path, expected) {
            let _ = fs::remove_dir_all(&dir);
            return Err(io::Error::new(
                err.kind(),
                format!("downloaded remote asset checksum verification failed: {err}"),
            ));
        }
    }

    Ok(InstallSource::temporary(path, dir))
}

fn fetch_remote_manifest(url: &str) -> io::Result<Vec<u8>> {
    let output = crate::noninteractive_process::curl_command()
        .args([
            "-sfL",
            "--retry",
            "3",
            "--connect-timeout",
            "10",
            "--max-time",
            "20",
            url,
        ])
        .output()
        .map_err(|err| io::Error::new(err.kind(), format!("curl failed: {err}")))?;
    if !output.status.success() {
        return Err(command_failed("failed to fetch update manifest", &output));
    }
    Ok(output.stdout)
}

fn remote_asset_info(asset: &RemoteAssetRef) -> RemoteReleaseAsset {
    RemoteReleaseAsset {
        url: asset.url().to_string(),
        sha256: asset.sha256().map(str::to_string),
    }
}

fn remote_release_asset(asset_key: &str) -> io::Result<RemoteReleaseAsset> {
    if crate::build_info::is_preview() {
        return Err(io::Error::other(format!(
            "the Profreshor/herdr-multi-remote fork does not publish preview remote binaries; set {REMOTE_BINARY_ENV_VAR}=target/release/herdr or install a matching Herdr on the remote host manually"
        )));
    }

    let current_version = current_version();
    let manifest_bytes = fetch_remote_manifest(STABLE_UPDATE_MANIFEST_URL)?;
    let manifest: RemoteUpdateManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|err| io::Error::other(format!("failed to parse update manifest JSON: {err}")))?;
    let release = manifest.release_for_version(&current_version).ok_or_else(|| {
        io::Error::other(format!(
            "release manifest does not include herdr {current_version}; build herdr for {} or install it there manually",
            asset_key
        ))
    })?;
    if let Some(protocol) = release.protocol {
        if protocol != CURRENT_PROTOCOL {
            return Err(io::Error::other(format!(
                "release manifest has herdr {current_version} protocol {protocol}, but this client needs protocol {CURRENT_PROTOCOL}; set {REMOTE_BINARY_ENV_VAR}=target/release/herdr or install a matching herdr on the remote host manually"
            )));
        }
    }
    let asset = release.assets.get(asset_key).ok_or_else(|| {
        io::Error::other(format!(
            "no {asset_key} binary in the release manifest for herdr {current_version}"
        ))
    })?;
    let mut asset = remote_asset_info(asset);
    asset.sha256 = asset
        .sha256
        .or_else(|| release.sha256.get(asset_key).cloned());
    if asset.sha256.is_none() {
        return Err(io::Error::other(format!(
            "release manifest asset {asset_key} is missing a SHA-256 checksum"
        )));
    }
    Ok(asset)
}

fn private_download_dir(asset_key: &str) -> io::Result<PathBuf> {
    let base = crate::platform::remote_private_temp_base();
    fs::create_dir_all(&base)?;
    for attempt in 0..100 {
        let dir = base.join(format!(
            "herdr-remote-{}-{}-{attempt}",
            std::process::id(),
            asset_key
        ));
        match crate::platform::create_remote_private_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "failed to create private herdr remote download directory",
    ))
}

fn confirm_remote_install(
    target: &str,
    remote_herdr: &RemoteHerdr,
    source_description: &str,
) -> io::Result<()> {
    if !io::stdin().is_terminal() {
        return Err(io::Error::other(format!(
            "matching remote herdr {} is not installed at {}; run from an interactive terminal to approve installation",
            current_version(),
            remote_herdr.shell_path
        )));
    }

    eprintln!(
        "matching herdr {} is not installed on {target} for {}.",
        current_version(),
        remote_herdr.platform.asset_key()
    );
    eprint!(
        "Install {} to {}? [Y/n] ",
        source_description, remote_herdr.shell_path
    );
    io::stderr().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    let answer = answer.trim().to_ascii_lowercase();
    if answer == "n" || answer == "no" {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "remote herdr installation cancelled",
        ));
    }

    Ok(())
}

fn remote_bridge_command(remote_herdr: &RemoteHerdr, session_name: &str) -> String {
    if remote_herdr.platform.is_windows() {
        let mut arguments = Vec::new();
        if session_name != crate::session::DEFAULT_SESSION_NAME {
            arguments.extend(["--session", session_name]);
        }
        arguments.push("remote-client-bridge");
        return powershell_stdio_command(&powershell_herdr_script(remote_herdr, &arguments));
    }
    let mut command = format!("exec {}", remote_herdr.shell_path);
    if session_name != crate::session::DEFAULT_SESSION_NAME {
        command.push_str(" --session ");
        command.push_str(&shell_quote(session_name));
    }
    command.push_str(" remote-client-bridge");
    command
}

fn reattach_command(
    program: &str,
    target: &str,
    session_name: &str,
    keybindings: RemoteKeybindings,
    live_handoff: bool,
) -> String {
    let program = crate::platform::remote_reattach_program(program);
    let target = crate::platform::remote_reattach_argument(target);
    let mut command = format!("{program} --remote {target}");
    if keybindings != RemoteKeybindings::Local {
        command.push_str(" --remote-keybindings ");
        command.push_str(keybindings.as_str());
    }
    if live_handoff {
        command.push_str(" --handoff");
    }
    if session_name != crate::session::DEFAULT_SESSION_NAME {
        command.push_str(" --session ");
        command.push_str(&crate::platform::remote_reattach_argument(session_name));
    }
    command
}

fn remote_provision_command(target: &str, session_name: &str) -> String {
    let mut command = format!(
        "herdr --remote {}",
        crate::platform::remote_reattach_argument(target)
    );
    if session_name != crate::session::DEFAULT_SESSION_NAME {
        command.push_str(" --session ");
        command.push_str(&crate::platform::remote_reattach_argument(session_name));
    }
    command
}

fn command_failed(context: &str, output: &Output) -> io::Error {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        io::Error::other(format!("{context}: {}", output.status))
    } else {
        io::Error::other(format!("{context}: {stderr}"))
    }
}

struct SshStdioBridge {
    local_socket: PathBuf,
    socket_identity: crate::ipc::SocketFileIdentity,
    should_stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SshStdioBridge {
    fn start(
        target: String,
        remote_herdr: RemoteHerdr,
        local_socket: PathBuf,
        session_name: String,
        ssh_options: Option<&ManagedSshOptions>,
    ) -> io::Result<Self> {
        Self::start_with_program(
            target,
            remote_herdr,
            local_socket,
            session_name,
            ssh_options,
            PathBuf::from("ssh"),
            false,
            Arc::new(AtomicBool::new(false)),
        )
    }

    fn start_with_program(
        target: String,
        remote_herdr: RemoteHerdr,
        local_socket: PathBuf,
        session_name: String,
        ssh_options: Option<&ManagedSshOptions>,
        ssh_program: PathBuf,
        batch_mode: bool,
        cancelled: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        crate::ipc::prepare_socket_path(&local_socket, |path| {
            format!("remote bridge is already listening at {}", path.display())
        })?;
        let listener = crate::ipc::bind_private_local_listener(&local_socket)?;
        let socket_identity = crate::ipc::socket_file_identity(&local_socket)?;
        if let Err(err) =
            crate::ipc::restrict_socket_permissions(&local_socket, BRIDGE_SOCKET_PERMISSION_MODE)
        {
            let _ = crate::ipc::remove_socket_file_if_owned(&local_socket, &socket_identity);
            return Err(err);
        }
        if let Err(err) = listener.set_nonblocking(ListenerNonblockingMode::Accept) {
            let _ = crate::ipc::remove_socket_file_if_owned(&local_socket, &socket_identity);
            return Err(err);
        }

        let should_stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&should_stop);
        let thread_ssh_options = ssh_options.cloned();
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) && !cancelled.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok(stream) => {
                        let stream = match prepare_remote_bridge_stream(stream) {
                            Ok(stream) => stream,
                            Err(err) => {
                                tracing::error!(
                                    error = %err,
                                    "remote bridge failed to prepare client socket"
                                );
                                continue;
                            }
                        };
                        if let Err(err) = bridge_connection(
                            stream,
                            &ssh_program,
                            &target,
                            &remote_herdr,
                            &session_name,
                            thread_ssh_options.as_ref(),
                            &thread_stop,
                            batch_mode,
                            &cancelled,
                        ) {
                            if batch_mode {
                                tracing::warn!(error = %err, "remote fleet bridge failed");
                            } else {
                                eprintln!("herdr: remote bridge failed: {err}");
                            }
                        }
                    }
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(BRIDGE_ACCEPT_POLL);
                    }
                    Err(err) => {
                        if batch_mode {
                            tracing::warn!(error = %err, "remote fleet bridge listener failed");
                        } else {
                            eprintln!("herdr: remote bridge listener failed: {err}");
                        }
                        break;
                    }
                }
            }
        });

        Ok(Self {
            local_socket,
            socket_identity,
            should_stop,
            thread: Some(thread),
        })
    }
}

fn prepare_remote_bridge_stream(
    mut stream: crate::ipc::LocalStream,
) -> io::Result<crate::ipc::LocalStream> {
    crate::ipc::set_local_stream_polling(&mut stream, false)?;
    Ok(stream)
}

impl Drop for SshStdioBridge {
    fn drop(&mut self) {
        self.should_stop.store(true, Ordering::Release);
        #[cfg(unix)]
        let _ = crate::ipc::remove_socket_file_if_owned(&self.local_socket, &self.socket_identity);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        #[cfg(windows)]
        let _ = crate::ipc::remove_socket_file_if_owned(&self.local_socket, &self.socket_identity);
    }
}

fn ssh_config_quote(path: &str) -> String {
    format!("\"{path}\"")
}

fn ssh_config_include_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    if std::path::MAIN_SEPARATOR == '\\' {
        ssh_config_quote(&path.replace('\\', "/"))
    } else {
        ssh_config_quote(&path)
    }
}

/// Builds a temporary ssh config that includes the user's settings and provides
/// keepalive fallbacks for tools that consume the config directly.
fn write_managed_ssh_config() -> io::Result<ManagedSshConfig> {
    let paths = crate::platform::remote_ssh_config_paths();
    let dir = crate::platform::create_remote_ssh_config_dir(SSH_CONTROL_SOCKET_NAME)?;
    let path = dir.join("config");
    let control_path = paths
        .multiplexing
        .then(|| dir.join(SSH_CONTROL_SOCKET_NAME));

    let mut contents = String::new();
    if let Some(user_config) = paths.user_config.filter(|path| path.is_file()) {
        contents.push_str(&format!(
            "Include {}\n",
            ssh_config_include_path(&user_config)
        ));
    }
    if let Some(system_config) = paths.system_config.filter(|path| path.is_file()) {
        contents.push_str(&format!(
            "Include {}\n",
            ssh_config_include_path(&system_config)
        ));
    }
    contents.push_str("Host *\n");
    contents.push_str("  ServerAliveInterval 15\n");
    contents.push_str("  ServerAliveCountMax 4\n");

    let write_result = (|| {
        let mut file = crate::platform::create_remote_ssh_config_file(&path)?;
        file.write_all(contents.as_bytes())
    })();
    if let Err(err) = write_result {
        let _ = fs::remove_dir_all(&dir);
        return Err(err);
    }
    Ok(ManagedSshConfig {
        options: ManagedSshOptions {
            config_path: path,
            control_path,
        },
    })
}

fn bridge_connection(
    stream: crate::ipc::LocalStream,
    ssh_program: &Path,
    target: &str,
    remote_herdr: &RemoteHerdr,
    session_name: &str,
    ssh_options: Option<&ManagedSshOptions>,
    bridge_stop: &Arc<AtomicBool>,
    batch_mode: bool,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    let mut command = Command::new(ssh_program);
    apply_managed_ssh_options(&mut command, ssh_options);
    if batch_mode {
        command
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("ConnectTimeout=10");
    }
    command
        .arg("-T")
        .arg(target)
        .arg(remote_bridge_command(remote_herdr, session_name))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(if batch_mode {
            Stdio::piped()
        } else {
            Stdio::inherit()
        });

    let mut child = command
        .spawn()
        .map_err(|err| io::Error::new(err.kind(), format!("failed to start ssh bridge: {err}")))?;
    let stderr = child
        .stderr
        .take()
        .map(|mut stderr| thread::spawn(move || read_bounded_tail(&mut stderr)));
    let mut child_stdin = match child.stdin.take() {
        Some(stdin) => stdin,
        None => return terminate_bridge_child(child, "ssh bridge stdin missing"),
    };
    let mut child_stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => return terminate_bridge_child(child, "ssh bridge stdout missing"),
    };
    let stream_to_child = match stream.try_clone() {
        Ok(stream) => stream,
        Err(err) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(err);
        }
    };
    if let Err(err) = stream.set_nonblocking(true) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(err);
    }
    let mut child_to_stream = stream;

    let connection_stop = Arc::new(AtomicBool::new(false));
    let upload_stop = Arc::new(AtomicBool::new(false));
    let upload_failed = Arc::new(AtomicBool::new(false));
    let download_done = Arc::new(AtomicBool::new(false));
    let client_closed = Arc::new(AtomicBool::new(false));
    let upload_cancel = Arc::clone(&upload_stop);
    let upload_bridge_stop = Arc::clone(bridge_stop);
    let upload_failed_worker = Arc::clone(&upload_failed);
    let upload_client_closed = Arc::clone(&client_closed);
    let upload = thread::spawn(move || {
        let result = copy_local_stream_to_writer(
            stream_to_child,
            &mut child_stdin,
            &upload_cancel,
            &upload_bridge_stop,
            &upload_client_closed,
        );
        upload_failed_worker.store(result.is_err(), Ordering::Release);
        result
    });
    let download_stop = Arc::clone(&connection_stop);
    let download_bridge_stop = Arc::clone(bridge_stop);
    let download_done_worker = Arc::clone(&download_done);
    let download_upload_stop = Arc::clone(&upload_stop);
    let download = thread::spawn(move || {
        let result = copy_reader_to_local_stream(
            &mut child_stdout,
            &mut child_to_stream,
            &download_stop,
            &download_bridge_stop,
        );
        download_done_worker.store(true, Ordering::Release);
        download_upload_stop.store(true, Ordering::Release);
        result
    });

    let mut stopped_at = None;
    let (status_result, child_exited) = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                upload_stop.store(true, Ordering::Release);
                break (Ok(status), true);
            }
            Ok(None) => {}
            Err(err) => {
                connection_stop.store(true, Ordering::Release);
                upload_stop.store(true, Ordering::Release);
                let _ = child.kill();
                let _ = child.wait();
                break (Err(err), false);
            }
        }
        if bridge_stop.load(Ordering::Acquire) || cancelled.load(Ordering::Acquire) {
            connection_stop.store(true, Ordering::Release);
            upload_stop.store(true, Ordering::Release);
            let _ = child.kill();
            break (child.wait(), false);
        }
        if client_closed.load(Ordering::Acquire)
            || upload_failed.load(Ordering::Acquire)
            || download_done.load(Ordering::Acquire)
        {
            upload_stop.store(true, Ordering::Release);
            let stopped_at = stopped_at.get_or_insert_with(Instant::now);
            if stopped_at.elapsed() >= Duration::from_millis(250) {
                connection_stop.store(true, Ordering::Release);
                let _ = child.kill();
                break (child.wait(), false);
            }
        }
        thread::sleep(BRIDGE_ACCEPT_POLL);
    };
    upload_stop.store(true, Ordering::Release);
    if !child_exited {
        connection_stop.store(true, Ordering::Release);
    }
    let upload_result = upload
        .join()
        .map_err(|_| io::Error::other("remote bridge upload worker panicked"))?;
    let download_result = download
        .join()
        .map_err(|_| io::Error::other("remote bridge download worker panicked"))?;
    let status = status_result?;
    let stderr = stderr
        .map(|stderr| {
            stderr
                .join()
                .map_err(|_| io::Error::other("remote bridge stderr worker panicked"))?
        })
        .transpose()?
        .unwrap_or_default();

    let stopping = bridge_stop.load(Ordering::Acquire) || cancelled.load(Ordering::Acquire);
    let client_closed = client_closed.load(Ordering::Acquire);
    if !stopping && !client_closed {
        upload_result.map_err(|err| {
            io::Error::new(err.kind(), format!("remote bridge upload failed: {err}"))
        })?;
        download_result.map_err(|err| {
            io::Error::new(err.kind(), format!("remote bridge download failed: {err}"))
        })?;
    }

    if status.success() || stopping || client_closed {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&stderr);
        let detail = stderr.trim();
        Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            if detail.is_empty() {
                format!("ssh bridge exited with {status}")
            } else {
                format!("ssh bridge exited with {status}: {detail}")
            },
        ))
    }
}

fn read_bounded_tail(reader: &mut impl io::Read) -> io::Result<Vec<u8>> {
    let mut tail = Vec::with_capacity(MAX_BRIDGE_STDERR_BYTES);
    let mut buffer = [0; 4096];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(tail);
        }
        let overflow = tail
            .len()
            .saturating_add(read)
            .saturating_sub(MAX_BRIDGE_STDERR_BYTES);
        if overflow > 0 {
            tail.drain(..overflow.min(tail.len()));
        }
        tail.extend_from_slice(&buffer[..read]);
    }
}

fn terminate_bridge_child(mut child: std::process::Child, message: &'static str) -> io::Result<()> {
    let _ = child.kill();
    let _ = child.wait();
    Err(io::Error::new(io::ErrorKind::BrokenPipe, message))
}

fn copy_reader_to_local_stream<R: io::Read>(
    reader: &mut R,
    stream: &mut crate::ipc::LocalStream,
    connection_stop: &AtomicBool,
    bridge_stop: &AtomicBool,
) -> io::Result<u64> {
    let mut buffer = [0_u8; 16 * 1024];
    let mut total = 0;

    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => return Ok(total),
            Ok(read) => read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        let mut written = 0;
        while written < read {
            if connection_stop.load(Ordering::Acquire) || bridge_stop.load(Ordering::Acquire) {
                return Ok(total);
            }
            let chunk_len = (read - written).min(4 * 1024);
            match stream.write(&buffer[written..written + chunk_len]) {
                Ok(0) => thread::sleep(BRIDGE_IO_POLL),
                Ok(count) => written += count,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(BRIDGE_IO_POLL);
                }
                Err(err) => return Err(err),
            }
        }
        stream.flush()?;
        total += read as u64;
    }
}

fn copy_local_stream_to_writer<W: io::Write>(
    mut stream: crate::ipc::LocalStream,
    writer: &mut W,
    connection_stop: &AtomicBool,
    bridge_stop: &AtomicBool,
    client_closed: &AtomicBool,
) -> io::Result<u64> {
    let mut buffer = [0_u8; 16 * 1024];
    let mut total = 0;

    while !connection_stop.load(Ordering::Acquire) && !bridge_stop.load(Ordering::Acquire) {
        match crate::ipc::poll_local_stream_read_count(&mut stream, &mut buffer)? {
            crate::ipc::LocalStreamReadCount::Data(read) => {
                writer.write_all(&buffer[..read])?;
                writer.flush()?;
                total += read as u64;
            }
            crate::ipc::LocalStreamReadCount::Pending => thread::sleep(BRIDGE_IO_POLL),
            crate::ipc::LocalStreamReadCount::Closed => {
                client_closed.store(true, Ordering::Release);
                break;
            }
        }
    }

    Ok(total)
}

fn run_client_process(
    local_socket: &Path,
    reattach_command: &str,
    keybindings: RemoteKeybindings,
) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let status = Command::new(exe)
        .arg("client")
        .env(
            crate::server::socket_paths::CLIENT_SOCKET_PATH_ENV_VAR,
            local_socket,
        )
        .env(REATTACH_COMMAND_ENV_VAR, reattach_command)
        .env(REMOTE_KEYBINDINGS_ENV_VAR, keybindings.as_str())
        .env_remove(crate::api::SOCKET_PATH_ENV_VAR)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            format!("remote client exited with {status}"),
        ))
    }
}

fn local_forward_socket_path(target: &str, session_name: &str) -> PathBuf {
    static NEXT_BRIDGE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let attempt = NEXT_BRIDGE.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let target_clean = sanitize_path_component(target);
    let session_clean = sanitize_path_component(session_name);
    let readable_name = format!("herdr-remote-{pid}-{attempt}-{target_clean}-{session_clean}.sock");
    let target_prefix: String = target_clean.chars().take(8).collect();
    let hash = short_socket_hash(target, session_name);
    let short_name = format!("herdr-r-{pid}-{attempt}-{target_prefix}-{hash}.sock");
    crate::platform::remote_bridge_endpoint_path(&readable_name, &short_name)
}

#[cfg(all(test, unix))]
fn fits_unix_socket_path(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;

    path.as_os_str().as_bytes().len() <= 103
}

fn short_socket_hash(target: &str, session: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    target.hash(&mut hasher);
    0u8.hash(&mut hasher);
    session.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn sanitize_path_component(input: &str) -> String {
    let sanitized: String = input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect();

    sanitized.trim_matches('-').chars().take(32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_stderr_retains_only_the_bounded_tail() {
        let input = (0..MAX_BRIDGE_STDERR_BYTES + 37)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let mut reader = input.as_slice();

        let tail = read_bounded_tail(&mut reader).unwrap();

        assert_eq!(tail, input[input.len() - MAX_BRIDGE_STDERR_BYTES..]);
    }

    #[cfg(unix)]
    #[test]
    fn bridge_socket_is_user_only() {
        use std::os::unix::fs::PermissionsExt;

        let socket = std::env::temp_dir().join(format!(
            "herdr-bridge-permissions-test-{}.sock",
            std::process::id()
        ));
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let bridge = SshStdioBridge::start(
            "example".to_string(),
            remote_herdr,
            socket.clone(),
            "default".to_string(),
            None,
        )
        .expect("start bridge listener");

        let mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, BRIDGE_SOCKET_PERMISSION_MODE);

        drop(bridge);
        let _ = std::fs::remove_file(socket);
    }

    #[cfg(unix)]
    #[test]
    fn accepted_bridge_stream_is_reset_to_blocking() {
        use std::os::fd::AsRawFd as _;

        fn is_nonblocking(stream: &crate::ipc::LocalStream) -> bool {
            let fd = match stream {
                crate::ipc::LocalStream::UdSocket(stream) => stream.inner().as_raw_fd(),
            };
            // SAFETY: F_GETFL only reads flags from the live descriptor owned by `stream`.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            assert!(flags >= 0, "fcntl(F_GETFL): {}", io::Error::last_os_error());
            flags & libc::O_NONBLOCK != 0
        }

        let socket = std::env::temp_dir().join(format!(
            "herdr-bridge-blocking-test-{}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&socket);
        let listener = crate::ipc::bind_private_local_listener(&socket).expect("bind listener");
        let client = crate::ipc::connect_local_stream(&socket).expect("connect client");
        let mut server = listener.accept().expect("accept client");

        crate::ipc::set_local_stream_polling(&mut server, true)
            .expect("force the macOS accepted-stream state");
        assert!(is_nonblocking(&server));
        let server = prepare_remote_bridge_stream(server).expect("prepare bridge stream");
        assert!(!is_nonblocking(&server));

        drop(server);
        drop(client);
        drop(listener);
        let _ = std::fs::remove_file(socket);
    }

    #[cfg(unix)]
    #[test]
    fn bridge_drop_cancels_an_active_ssh_child() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "herdr-active-bridge-drop-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ssh = dir.join("ssh");
        let started = dir.join("started");
        let socket = dir.join("bridge.sock");
        std::fs::write(
            &ssh,
            format!(
                "#!/bin/sh\nprintf ready > '{}'\nwhile read line; do :; done\n",
                started.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();

        let cancelled = Arc::new(AtomicBool::new(false));
        let bridge = SshStdioBridge::start_with_program(
            "example".to_string(),
            RemoteHerdr::for_platform(RemotePlatform {
                os: "linux",
                arch: "x86_64",
            }),
            socket.clone(),
            "default".to_string(),
            None,
            ssh,
            false,
            cancelled.clone(),
        )
        .unwrap();
        let _client = crate::ipc::connect_local_stream(&socket).unwrap();
        for _ in 0..100 {
            if started.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(started.exists(), "fake ssh child did not start");

        let drop_started = Instant::now();
        drop(bridge);
        assert!(drop_started.elapsed() < Duration::from_secs(1));
        assert!(
            !cancelled.load(Ordering::Acquire),
            "bridge teardown must allow retries"
        );
        assert!(!socket.exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn cancelling_a_connector_unblocks_a_pending_bridge_handshake() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "herdr-cancel-handshake-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let ssh = dir.join("ssh");
        let started = dir.join("started");
        let socket = dir.join("bridge.sock");
        std::fs::write(
            &ssh,
            format!(
                "#!/bin/sh\nprintf ready > '{}'\nwhile read line; do :; done\n",
                started.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let bridge = SshStdioBridge::start_with_program(
            "example".into(),
            RemoteHerdr::for_platform(RemotePlatform::local()),
            socket.clone(),
            "default".into(),
            None,
            ssh,
            true,
            cancelled.clone(),
        )
        .unwrap();
        let mut client = crate::ipc::connect_local_stream(&socket).unwrap();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let reader = thread::spawn(move || {
            let welcome: Result<crate::protocol::ServerMessage, _> =
                crate::protocol::read_message(&mut client, crate::protocol::MAX_FRAME_SIZE);
            done_tx.send(welcome.is_err()).unwrap();
        });
        for _ in 0..100 {
            if started.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let child_started = started.exists();
        cancelled.store(true, Ordering::Release);
        let result = done_rx.recv_timeout(Duration::from_secs(1));
        // Always release the reader, including when the cancellation regression fails.
        drop(bridge);
        reader.join().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        assert!(child_started, "fake ssh child did not start");
        assert_eq!(
            result,
            Ok(true),
            "cancellation must close the pending welcome read"
        );
    }

    #[cfg(unix)]
    #[test]
    fn ssh_cleanup_kills_a_hung_child_after_the_deadline() {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "sleep 10"])
            .spawn()
            .unwrap();
        let started = Instant::now();

        terminate_child_after(&mut child, Duration::from_millis(50));

        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(child.try_wait().unwrap().is_some());
    }

    #[cfg(windows)]
    #[test]
    fn windows_bridge_drop_while_waiting_for_client_is_bounded() {
        let socket = local_forward_socket_path("drop-test", "default");
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let bridge = SshStdioBridge::start(
            "example".to_string(),
            remote_herdr,
            socket.clone(),
            "default".to_string(),
            None,
        )
        .expect("start bridge listener");
        let started = Instant::now();

        drop(bridge);

        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(!socket.exists());
    }

    #[cfg(unix)]
    #[test]
    fn managed_ssh_config_includes_user_config_then_fallback() {
        use std::os::unix::fs::PermissionsExt;

        let managed_config = write_managed_ssh_config().expect("write managed config");
        let path = managed_config.options.config_path.clone();
        let control_path = managed_config
            .options
            .control_path
            .clone()
            .expect("Unix managed config has a control path");
        let contents = std::fs::read_to_string(&path).expect("read keepalive config");

        // herdr's fallback transport settings are present...
        assert!(
            contents.contains("Host *"),
            "config should add a Host * fallback block: {contents}"
        );
        assert!(
            contents.contains("ServerAliveInterval 15"),
            "config should set the keepalive interval: {contents}"
        );
        assert!(
            contents.contains("ServerAliveCountMax 4"),
            "config should set the keepalive count: {contents}"
        );
        assert!(!contents.contains("ControlMaster"));
        assert!(!contents.contains("ControlPersist"));
        assert!(!contents.contains("ControlPath"));
        // ...and any user config is Included (quoted) before the fallback.
        // Managed Herdr ssh commands enforce their liveness bound with `-o`.
        if let Some(home) = std::env::var_os("HOME") {
            let user_config = PathBuf::from(home).join(".ssh").join("config");
            if user_config.is_file() {
                let include = format!(
                    "Include {}",
                    ssh_config_quote(&user_config.to_string_lossy())
                );
                let include_at = contents.find(&include).expect("user config Included");
                let fallback_at = contents.find("Host *").expect("fallback present");
                assert!(
                    include_at < fallback_at,
                    "user config must be Included before herdr's fallback: {contents}"
                );
            }
        }

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, BRIDGE_SOCKET_PERMISSION_MODE,
            "keepalive config must be user-only"
        );
        // The config lives in a private 0700 dir, not a predictable temp path.
        let dir = path.parent().expect("config has a parent dir");
        let dir_mode = std::fs::metadata(dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "ssh config dir must be user-only");
        assert!(
            fits_unix_socket_path(&control_path),
            "control socket path must fit portable Unix socket limits"
        );

        drop(managed_config);
    }

    #[test]
    fn ssh_config_quote_wraps_path_with_spaces() {
        assert_eq!(
            ssh_config_quote("/home/a b/.ssh/config"),
            "\"/home/a b/.ssh/config\""
        );
    }

    #[cfg(unix)]
    #[test]
    fn remote_ssh_command_uses_managed_config_when_present() {
        let managed_config = write_managed_ssh_config().expect("write managed config");
        let config_path = managed_config.options.config_path.clone();
        let control_path = managed_config
            .options
            .control_path
            .clone()
            .expect("Unix managed config has a control path");
        let ssh = RemoteSsh {
            target: "example".to_string(),
            managed_config: Some(managed_config),
            cancelled: Arc::new(AtomicBool::new(false)),
            batch_mode: false,
        };

        let command = ssh.command();
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            args,
            vec![
                "-F".to_string(),
                config_path.to_string_lossy().into_owned(),
                "-S".to_string(),
                control_path.to_string_lossy().into_owned(),
                "-o".to_string(),
                "ControlMaster=auto".to_string(),
                "-o".to_string(),
                "ControlPersist=yes".to_string(),
                "-o".to_string(),
                "ServerAliveInterval=15".to_string(),
                "-o".to_string(),
                "ServerAliveCountMax=4".to_string(),
                "-T".to_string(),
                "example".to_string(),
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_managed_ssh_config_uses_keepalives_without_control_socket() {
        let managed_config = write_managed_ssh_config().expect("write managed config");
        let config_path = managed_config.options.config_path.clone();
        assert!(managed_config.options.control_path.is_none());
        let contents = std::fs::read_to_string(&config_path).expect("read managed config");
        assert!(contents.contains("ServerAliveInterval 15"));
        assert!(contents.contains("ServerAliveCountMax 4"));

        let ssh = RemoteSsh {
            target: "example".to_string(),
            managed_config: Some(managed_config),
            cancelled: Arc::new(AtomicBool::new(false)),
            batch_mode: false,
        };
        let args = ssh
            .command()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            vec![
                "-F".to_string(),
                config_path.to_string_lossy().into_owned(),
                "-o".to_string(),
                "ServerAliveInterval=15".to_string(),
                "-o".to_string(),
                "ServerAliveCountMax=4".to_string(),
                "-T".to_string(),
                "example".to_string(),
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_ssh_config_include_uses_forward_slashes() {
        assert_eq!(
            ssh_config_include_path(Path::new(r"C:\Users\A B\.ssh\config")),
            r#""C:/Users/A B/.ssh/config""#
        );
    }

    #[test]
    fn remote_ssh_command_is_plain_without_managed_config() {
        let ssh = RemoteSsh {
            target: "example".to_string(),
            managed_config: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            batch_mode: false,
        };

        let command = ssh.command();
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(args, vec!["-T".to_string(), "example".to_string()]);
    }

    #[test]
    fn remote_install_stream_command_avoids_shell_c_wrapper() {
        let command = remote_install_stream_command("/home/a b/.local/bin/herdr.tmp.123");

        assert_eq!(command, "tee '/home/a b/.local/bin/herdr.tmp.123'");
    }

    #[test]
    fn remote_install_prepare_and_commit_scripts_quote_paths() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let prepare = remote_install_prepare_script(&remote_herdr);

        assert!(prepare.contains("mkdir -p \"$dir\""));
        assert!(prepare.contains("printf '%s\\0%s\\0' \"$tmp\" \"$dest\""));
        assert_eq!(
            parse_remote_install_paths(b"/home/a b/herdr.tmp.42\0/home/a b/herdr\0").unwrap(),
            (
                "/home/a b/herdr.tmp.42".to_string(),
                "/home/a b/herdr".to_string()
            )
        );
        assert_eq!(
            parse_remote_install_paths(b"/home/a b\n/herdr.tmp.42\0/home/a b\n/herdr\0").unwrap(),
            (
                "/home/a b\n/herdr.tmp.42".to_string(),
                "/home/a b\n/herdr".to_string()
            )
        );
        assert_eq!(
            remote_install_commit_script("/home/a b/herdr.tmp.42", "/home/a b/herdr"),
            "set -eu\nchmod 755 '/home/a b/herdr.tmp.42'\nmv '/home/a b/herdr.tmp.42' '/home/a b/herdr'\n"
        );
    }

    #[test]
    fn extract_remote_args_removes_space_form() {
        let args = vec![
            "herdr".into(),
            "--remote".into(),
            "dev".into(),
            "--help".into(),
        ];
        let (cleaned, remote) = extract_remote_args(&args).unwrap();
        assert_eq!(cleaned, vec!["herdr", "--help"]);
        let remote = remote.unwrap();
        assert_eq!(remote.target, "dev");
        assert_eq!(remote.keybindings, RemoteKeybindings::Local);
    }

    #[test]
    fn extract_remote_args_removes_equals_form() {
        let args = vec!["herdr".into(), "--remote=user@host".into()];
        let (cleaned, remote) = extract_remote_args(&args).unwrap();
        assert_eq!(cleaned, vec!["herdr"]);
        let remote = remote.unwrap();
        assert_eq!(remote.target, "user@host");
        assert_eq!(remote.keybindings, RemoteKeybindings::Local);
    }

    #[test]
    fn extract_remote_args_accepts_remote_keybindings_server() {
        let args = vec![
            "herdr".into(),
            "--remote".into(),
            "dev".into(),
            "--remote-keybindings=server".into(),
        ];
        let (cleaned, remote) = extract_remote_args(&args).unwrap();
        assert_eq!(cleaned, vec!["herdr"]);
        let remote = remote.unwrap();
        assert_eq!(remote.target, "dev");
        assert_eq!(remote.keybindings, RemoteKeybindings::Server);
    }

    #[test]
    fn extract_remote_args_accepts_remote_keybindings_space_form() {
        let args = vec![
            "herdr".into(),
            "--remote=dev".into(),
            "--remote-keybindings".into(),
            "server".into(),
        ];
        let (cleaned, remote) = extract_remote_args(&args).unwrap();
        assert_eq!(cleaned, vec!["herdr"]);
        assert_eq!(remote.unwrap().keybindings, RemoteKeybindings::Server);
    }

    #[test]
    fn extract_remote_args_accepts_explicit_handoff() {
        let args = vec!["herdr".into(), "--remote=dev".into(), "--handoff".into()];

        let (cleaned, remote) = extract_remote_args(&args).unwrap();

        assert_eq!(cleaned, vec!["herdr"]);
        let remote = remote.unwrap();
        assert_eq!(remote.target, "dev");
        assert!(remote.live_handoff);
    }

    #[test]
    fn extract_remote_args_preserves_child_remote_options_after_separator() {
        let args = vec![
            "herdr".into(),
            "agent".into(),
            "start".into(),
            "repro".into(),
            "--".into(),
            "child".into(),
            "--remote".into(),
            "dev".into(),
            "--remote-keybindings=server".into(),
            "--handoff".into(),
        ];

        let (cleaned, remote) = extract_remote_args(&args).unwrap();

        assert_eq!(cleaned, args);
        assert!(remote.is_none());
    }

    #[test]
    fn extract_remote_args_preserves_handoff_without_remote() {
        let args = vec!["herdr".into(), "update".into(), "--handoff".into()];

        let (cleaned, remote) = extract_remote_args(&args).unwrap();

        assert_eq!(cleaned, args);
        assert!(remote.is_none());
    }

    #[test]
    fn extract_remote_args_rejects_remote_keybindings_without_remote() {
        let args = vec!["herdr".into(), "--remote-keybindings=server".into()];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "--remote-keybindings requires --remote");
    }

    #[test]
    fn extract_remote_args_rejects_duplicate_remote_keybindings() {
        let args = vec![
            "herdr".into(),
            "--remote=dev".into(),
            "--remote-keybindings=local".into(),
            "--remote-keybindings=server".into(),
        ];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "--remote-keybindings can only be specified once");
    }

    #[test]
    fn extract_remote_args_requires_value() {
        let args = vec!["herdr".into(), "--remote".into()];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "missing value for --remote");
    }

    #[test]
    fn extract_remote_args_rejects_empty_value() {
        let args = vec!["herdr".into(), "--remote=".into()];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "missing value for --remote");
    }

    #[test]
    fn extract_remote_args_rejects_duplicate_values() {
        let args = vec![
            "herdr".into(),
            "--remote=dev".into(),
            "--remote=prod".into(),
        ];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "--remote can only be specified once");
    }

    #[test]
    fn extract_remote_args_rejects_option_like_target() {
        let args = vec!["herdr".into(), "--remote".into(), "-oProxyCommand=x".into()];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "--remote target must not start with '-'");
    }

    #[test]
    fn sanitize_path_component_removes_shell_sensitive_chars() {
        assert_eq!(sanitize_path_component("user@host:22"), "user-host-22");
    }

    #[test]
    fn remote_platform_maps_uname_values() {
        assert_eq!(
            RemotePlatform::from_uname("Linux", "amd64")
                .unwrap()
                .asset_key(),
            "linux-x86_64"
        );
        assert_eq!(
            RemotePlatform::from_uname("Darwin", "arm64")
                .unwrap()
                .asset_key(),
            "macos-aarch64"
        );
        assert_eq!(
            RemotePlatform::from_uname("Windows", "AMD64")
                .unwrap()
                .asset_key(),
            "windows-x86_64"
        );
        assert_eq!(
            RemotePlatform::from_uname("Windows", "ARM64")
                .unwrap()
                .asset_key(),
            "windows-x86_64"
        );
        assert!(RemotePlatform::from_uname("FreeBSD", "x86_64").is_none());
    }

    fn decoded_powershell_script(command: &str) -> String {
        let encoded = command
            .split_once("FromBase64String('")
            .and_then(|(_, rest)| rest.split_once("')"))
            .map(|(encoded, _)| encoded)
            .expect("encoded PowerShell command");
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("base64");
        let utf16 = bytes
            .chunks_exact(2)
            .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
            .collect::<Vec<_>>();
        String::from_utf16(&utf16).expect("UTF-16LE script")
    }

    #[test]
    fn powershell_stdio_wrapper_keeps_script_and_stdin_available() {
        let script = "$path = 'C:\\Users\\O''Brien\\herdr.exe'; & $path";
        let command = powershell_stdio_command(script);

        assert_eq!(decoded_powershell_script(&command), script);
        assert!(!command.contains("-EncodedCommand"));
    }

    #[test]
    fn windows_remote_path_and_bridge_command_are_shell_safe() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "windows",
            arch: "x86_64",
        });
        let remote_herdr = remote_herdr_from_path_discovery(
            &remote_herdr,
            "C:\\Users\\O'Brien\\Herdr Bin\\herdr.exe\r\n",
        )
        .expect("Windows path binary");
        assert_eq!(
            remote_herdr.shell_path,
            "'C:\\Users\\O''Brien\\Herdr Bin\\herdr.exe'"
        );

        let script = decoded_powershell_script(&remote_bridge_command(&remote_herdr, "work"));
        assert!(script.contains("$herdr = 'C:\\Users\\O''Brien\\Herdr Bin\\herdr.exe'"));
        assert!(script.contains("@('--session','work','remote-client-bridge')"));
    }

    #[test]
    fn windows_remote_install_requires_the_packaged_zip() {
        let platform = RemotePlatform {
            os: "windows",
            arch: "x86_64",
        };
        let error = resolve_install_source(&platform, Some(PathBuf::from("herdr.exe")))
            .err()
            .expect("bare Windows executable must be rejected");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("ConPTY runtime"));
        assert!(windows_install_prepare_script().contains("[guid]::NewGuid"));
        assert_eq!(
            windows_scp_target("dev", "C:\\Temp\\herdr.zip"),
            "dev:C:/Temp/herdr.zip"
        );
    }

    #[cfg(unix)]
    #[test]
    fn reattach_command_includes_remote_and_session() {
        assert_eq!(
            reattach_command(
                "target/release/herdr",
                "user@host",
                "work",
                RemoteKeybindings::Local,
                false,
            ),
            "target/release/herdr --remote user@host --session work"
        );
        assert_eq!(
            reattach_command(
                "herdr",
                "host name",
                crate::session::DEFAULT_SESSION_NAME,
                RemoteKeybindings::Local,
                false,
            ),
            "herdr --remote 'host name'"
        );
        assert_eq!(
            reattach_command(
                "herdr",
                "host",
                crate::session::DEFAULT_SESSION_NAME,
                RemoteKeybindings::Server,
                false,
            ),
            "herdr --remote host --remote-keybindings server"
        );
        assert_eq!(
            reattach_command(
                "herdr",
                "host",
                crate::session::DEFAULT_SESSION_NAME,
                RemoteKeybindings::Local,
                true,
            ),
            "herdr --remote host --handoff"
        );
    }

    #[cfg(unix)]
    #[test]
    fn remote_provision_command_includes_non_default_session() {
        assert_eq!(
            remote_provision_command("host name", "work name"),
            "herdr --remote 'host name' --session 'work name'"
        );
        assert_eq!(
            remote_provision_command("host", crate::session::DEFAULT_SESSION_NAME),
            "herdr --remote host"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_reattach_command_uses_current_executable() {
        let executable = std::env::current_exe().expect("current test executable");
        assert_eq!(
            reattach_command(
                r"C:\Program Files\Herdr\herdr.exe",
                "host'name",
                "work'name",
                RemoteKeybindings::Local,
                false,
            ),
            format!(
                "& '{}' --remote 'host''name' --session 'work''name'",
                executable.display().to_string().replace('\'', "''")
            )
        );
        assert_eq!(
            remote_provision_command("host'name", "work'name"),
            "herdr --remote 'host''name' --session 'work''name'"
        );
    }

    #[test]
    fn remote_bridge_command_uses_installed_binary() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        assert_eq!(
            remote_bridge_command(&remote_herdr, crate::session::DEFAULT_SESSION_NAME),
            "exec \"$HOME/.local/bin/herdr\" remote-client-bridge"
        );
    }

    #[test]
    fn remote_path_discovery_uses_path_binary() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let remote_herdr = remote_herdr_from_path_discovery(&remote_herdr, "/usr/bin/herdr\n")
            .expect("path binary");

        assert_eq!(
            remote_bridge_command(&remote_herdr, crate::session::DEFAULT_SESSION_NAME),
            "exec /usr/bin/herdr remote-client-bridge"
        );
    }

    #[test]
    fn remote_path_discovery_quotes_discovered_binary() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let remote_herdr =
            remote_herdr_from_path_discovery(&remote_herdr, "/opt/herdr bin/herdr\n")
                .expect("path binary");

        assert_eq!(
            remote_bridge_command(&remote_herdr, crate::session::DEFAULT_SESSION_NAME),
            "exec '/opt/herdr bin/herdr' remote-client-bridge"
        );
    }

    #[test]
    fn remote_path_discovery_uses_macos_path_binary() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "macos",
            arch: "aarch64",
        });
        let remote_herdr =
            remote_herdr_from_path_discovery(&remote_herdr, "/opt/homebrew/bin/herdr\n")
                .expect("path binary");

        assert_eq!(
            remote_bridge_command(&remote_herdr, crate::session::DEFAULT_SESSION_NAME),
            "exec /opt/homebrew/bin/herdr remote-client-bridge"
        );
        assert_eq!(remote_herdr.platform.asset_key(), "macos-aarch64");
    }

    #[test]
    fn remote_path_discovery_reads_multiple_absolute_paths() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let candidates = remote_herdrs_from_path_discovery(
            &remote_herdr,
            "/usr/bin/herdr\nbin/herdr\n /opt/herdr bin/herdr\n",
        );

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].shell_path, "/usr/bin/herdr");
        assert_eq!(candidates[1].shell_path, "'/opt/herdr bin/herdr'");
    }

    #[test]
    fn remote_path_discovery_ignores_mise_shims() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let candidates = remote_herdrs_from_path_discovery(
            &remote_herdr,
            "/home/can/.local/share/mise/shims/herdr\n/home/can/.local/share/mise/installs/herdr/0.7.1/bin/herdr\n",
        );

        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].shell_path,
            "/home/can/.local/share/mise/installs/herdr/0.7.1/bin/herdr"
        );
    }

    #[test]
    fn known_remote_binary_candidate_script_includes_mise_and_nix_paths() {
        let script = known_remote_binary_candidate_script(&RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });

        assert!(script.contains("emit \"$home/.local/bin/herdr\""));
        assert!(!script.contains("mise/shims/herdr"));
        assert!(script.contains(&format!("version={}", shell_quote(&current_version()))));
        assert!(
            script.contains("emit \"$home/.local/share/mise/installs/herdr/$version/bin/herdr\"")
        );
        assert!(script.contains("emit \"$home/.local/share/mise/installs/herdr/$version/herdr\""));
        assert!(script.contains("emit \"$home/.nix-profile/bin/herdr\""));
        assert!(script.contains("emit \"/etc/profiles/per-user/$user/bin/herdr\""));
        assert!(script.contains("emit \"/run/current-system/sw/bin/herdr\""));
        assert!(script.contains("emit \"/home/linuxbrew/.linuxbrew/bin/herdr\""));
        assert!(!script.contains("emit \"/opt/homebrew/bin/herdr\""));
    }

    #[test]
    fn known_remote_binary_candidate_script_includes_macos_homebrew_paths() {
        let script = known_remote_binary_candidate_script(&RemotePlatform {
            os: "macos",
            arch: "aarch64",
        });

        assert!(script.contains("emit \"/opt/homebrew/bin/herdr\""));
        assert!(script.contains("emit \"/usr/local/bin/herdr\""));
        assert!(!script.contains("emit \"/home/linuxbrew/.linuxbrew/bin/herdr\""));
    }

    #[test]
    fn remote_path_discovery_quotes_single_quotes_in_discovered_binary() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let remote_herdr =
            remote_herdr_from_path_discovery(&remote_herdr, "/opt/herdr's/bin/herdr\n")
                .expect("path binary");

        assert_eq!(
            remote_bridge_command(&remote_herdr, crate::session::DEFAULT_SESSION_NAME),
            "exec '/opt/herdr'\\''s/bin/herdr' remote-client-bridge"
        );
    }

    #[test]
    fn remote_path_discovery_ignores_relative_paths() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let remote_herdr = remote_herdr_from_path_discovery(&remote_herdr, "bin/herdr\n");

        assert!(remote_herdr.is_none());
    }

    #[test]
    fn remote_path_discovery_ignores_empty_output() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let remote_herdr = remote_herdr_from_path_discovery(&remote_herdr, "\n");

        assert!(remote_herdr.is_none());
    }

    #[test]
    fn remote_shell_path_warning_accepts_managed_install() {
        assert!(remote_shell_resolves_managed_install(
            "/home/can/.local/bin/herdr\n"
        ));
        assert!(remote_shell_resolves_managed_install(
            "/Users/can/.local/bin/herdr\n"
        ));
        assert!(!remote_shell_resolves_managed_install(
            "/usr/local/bin/herdr\n"
        ));
        assert!(!remote_shell_resolves_managed_install(""));
    }

    #[test]
    fn parse_client_status_json_reads_last_json_record() {
        let status = parse_client_status_json(
            "wrapper output\n{\"version\":\"0.8.0\",\"protocol\":20,\"endpoint_protocol_generation\":1}\n{\"wrapper\":true}\n",
        )
        .unwrap();
        assert_eq!(status.version.as_deref(), Some("0.8.0"));
        assert_eq!(status.protocol, Some(20));
        assert_eq!(status.endpoint_protocol_generation, Some(1));
        assert!(
            parse_client_status_json(r#"{"endpoint_protocol_generation":"unknown"}"#).is_none()
        );
    }

    #[test]
    fn parse_remote_server_status_json_reads_running_server() {
        assert_eq!(
            parse_remote_server_status_json(
                r#"{"status":"running","running":true,"version":"0.6.0","protocol":8,"capabilities":{"live_handoff":true,"detached_server_daemon":true,"endpoint_protocol_generation":1}}"#
            )
            .unwrap(),
            RemoteServerStatus::Running {
                version: Some("0.6.0".into()),
                endpoint_protocol_generation: Some(1),
                live_handoff: true,
                detached_server_daemon: true
            }
        );
    }

    #[test]
    fn parse_remote_server_status_json_treats_missing_capability_as_old_server() {
        assert_eq!(
            parse_remote_server_status_json(
                r#"{"status":"running","running":true,"version":"0.6.0","protocol":8}"#
            )
            .unwrap(),
            RemoteServerStatus::Running {
                version: Some("0.6.0".into()),
                endpoint_protocol_generation: None,
                live_handoff: false,
                detached_server_daemon: false
            }
        );
    }

    #[test]
    fn parse_remote_server_status_json_reads_stopped_server() {
        assert_eq!(
            parse_remote_server_status_json(
                r#"{"status":"not_running","running":false,"version":null,"protocol":null}"#
            )
            .unwrap(),
            RemoteServerStatus::NotRunning
        );
    }

    #[test]
    fn remote_update_manifest_uses_root_assets_for_latest_version() {
        let manifest: RemoteUpdateManifest = serde_json::from_str(
            r#"{
                "version": "1.2.3",
                "assets": {
                    "linux-x86_64": "https://example.com/latest"
                },
                "sha256": {
                    "linux-x86_64": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                },
                "releases": {
                    "1.2.3": {
                        "assets": {
                            "linux-x86_64": "https://example.com/archive"
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        let release = manifest.release_for_version("1.2.3").unwrap();
        assert_eq!(
            release.assets.get("linux-x86_64").map(RemoteAssetRef::url),
            Some("https://example.com/latest")
        );
        assert_eq!(
            release.sha256.get("linux-x86_64").map(String::as_str),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
    }

    #[test]
    fn remote_update_manifest_reads_archived_release_assets() {
        let manifest: RemoteUpdateManifest = serde_json::from_str(
            r#"{
                "version": "1.2.4",
                "assets": {
                    "linux-x86_64": "https://example.com/latest"
                },
                "releases": {
                    "1.2.3": {
                        "notes": "ignored",
                        "assets": {
                            "linux-x86_64": "https://example.com/archive"
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            manifest
                .release_for_version("1.2.3")
                .and_then(|release| release.assets.get("linux-x86_64"))
                .map(RemoteAssetRef::url),
            Some("https://example.com/archive")
        );
    }

    #[test]
    fn remote_update_manifest_uses_archived_release_protocol() {
        let manifest: RemoteUpdateManifest = serde_json::from_str(
            r#"{
                "version": "1.2.4",
                "protocol": 42,
                "assets": {
                    "linux-x86_64": "https://example.com/latest"
                },
                "releases": {
                    "1.2.3": {
                        "notes": "ignored",
                        "protocol": 41,
                        "assets": {
                            "linux-x86_64": "https://example.com/archive"
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            manifest
                .release_for_version("1.2.3")
                .and_then(|release| release.protocol),
            Some(41)
        );
    }

    #[test]
    fn remote_update_manifest_does_not_inherit_latest_protocol_for_archived_assets() {
        let manifest: RemoteUpdateManifest = serde_json::from_str(
            r#"{
                "version": "1.2.4",
                "protocol": 42,
                "assets": {
                    "linux-x86_64": "https://example.com/latest"
                },
                "releases": {
                    "1.2.3": {
                        "notes": "ignored",
                        "assets": {
                            "linux-x86_64": "https://example.com/archive"
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            manifest
                .release_for_version("1.2.3")
                .and_then(|release| release.protocol),
            None
        );
    }

    #[test]
    fn remote_server_restart_reason_requires_one_update_for_pre_floor_server() {
        assert_eq!(
            remote_server_restart_reason(None, true),
            Some(RemoteServerRestartReason::EndpointProtocolMissing)
        );
    }

    #[test]
    fn remote_server_restart_reason_allows_compatible_server() {
        assert_eq!(
            remote_server_restart_reason(
                Some(crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION),
                true
            ),
            None
        );
    }

    #[test]
    fn remote_server_restart_reason_requires_restart_for_old_daemon() {
        assert_eq!(
            remote_server_restart_reason(
                Some(crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION),
                false
            ),
            Some(RemoteServerRestartReason::DaemonDetachMissing)
        );
    }

    #[test]
    fn remote_install_plan_keeps_compatible_running_server() {
        assert_eq!(
            remote_install_running_server_plan(
                Some(crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION),
                true,
                false,
                false
            ),
            RemoteInstallRunningServerPlan::KeepRunning
        );
    }

    #[test]
    fn remote_install_plan_requires_stop_for_old_daemon() {
        assert_eq!(
            remote_install_running_server_plan(
                Some(crate::protocol::endpoint::ENDPOINT_PROTOCOL_GENERATION),
                false,
                false,
                false
            ),
            RemoteInstallRunningServerPlan::StopRequired(
                RemoteServerRestartReason::DaemonDetachMissing
            )
        );
    }

    #[test]
    fn remote_install_plan_requires_stop_for_pre_floor_server() {
        assert_eq!(
            remote_install_running_server_plan(None, true, false, false),
            RemoteInstallRunningServerPlan::StopRequired(
                RemoteServerRestartReason::EndpointProtocolMissing
            )
        );
    }

    #[test]
    fn remote_install_plan_uses_live_handoff_for_pre_floor_server() {
        assert_eq!(
            remote_install_running_server_plan(None, true, true, true),
            RemoteInstallRunningServerPlan::LiveHandoff
        );
    }

    #[test]
    fn remote_live_handoff_uses_prepared_binary_identity() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let command = remote_live_handoff_command(
            &remote_herdr,
            crate::session::DEFAULT_SESSION_NAME,
            19,
            "0.7.9",
        );
        assert!(command.contains("--expected-protocol 19"));
        assert!(command.contains("--expected-version 0.7.9"));
        assert!(!command.contains("--session"));
        assert!(!command.contains(&format!(
            "--expected-protocol {CURRENT_PROTOCOL} --expected-version {}",
            current_version()
        )));
    }

    #[test]
    fn remote_lifecycle_commands_scope_named_sessions() {
        assert_eq!(
            session_scoped_arguments("fleet", &["status", "server", "--json"]),
            ["--session", "fleet", "status", "server", "--json"]
        );

        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let command = remote_live_handoff_command(&remote_herdr, "fleet", 22, "0.8.2");
        assert!(command.contains("--session fleet server live-handoff"));
    }

    #[test]
    fn install_source_description_uses_override_binary() {
        let platform = RemotePlatform {
            os: "linux",
            arch: "aarch64",
        };
        assert_eq!(
            install_source_description_for(&platform, Some(Path::new("/tmp/herdr-aarch64")), false),
            "HERDR_REMOTE_BINARY (/tmp/herdr-aarch64)"
        );
    }

    #[test]
    fn install_source_description_uses_local_binary_when_allowed() {
        let platform = RemotePlatform::local();

        assert_eq!(
            install_source_description_for(&platform, None, true),
            "the current local herdr binary"
        );
    }

    #[test]
    fn install_source_description_uses_release_asset_when_local_binary_cannot_seed_remote() {
        let platform = RemotePlatform::local();

        assert_eq!(
            install_source_description_for(&platform, None, false),
            format!(
                "the {} {} asset for {}",
                current_version(),
                current_channel(),
                platform.asset_key()
            )
        );
    }

    #[test]
    fn resolve_install_source_uses_override_binary_without_temporary_cleanup() {
        let platform = RemotePlatform {
            os: "linux",
            arch: "aarch64",
        };
        let source = resolve_install_source(&platform, Some(PathBuf::from("/tmp/herdr-aarch64")))
            .expect("override source");
        assert_eq!(source.path, PathBuf::from("/tmp/herdr-aarch64"));
        assert!(source.temporary_dir.is_none());
    }

    #[cfg(windows)]
    #[test]
    fn windows_local_forward_endpoint_uses_private_state_dir() {
        let path = local_forward_socket_path("user@example.com", "work");
        assert!(path.starts_with(crate::platform::remote_private_temp_base()));
        assert!(path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("herdr-r-")));
    }

    fn remote_env_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    #[cfg(unix)]
    fn socket_path_byte_len(path: &Path) -> usize {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().len()
    }

    #[cfg(unix)]
    #[test]
    fn local_forward_socket_path_uses_readable_name_when_it_fits() {
        let _guard = remote_env_lock().lock().unwrap();
        // Short target + session leave plenty of room — keep the human-
        // readable form so the socket path stays grep-friendly.
        let path = local_forward_socket_path("dev", "default");
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        assert!(
            filename.starts_with("herdr-remote-"),
            "expected readable name, got {filename}"
        );
        assert!(filename.contains("-dev-default."), "got {filename}");
        assert!(
            fits_unix_socket_path(&path),
            "socket path too long: {} ({} bytes)",
            path.display(),
            socket_path_byte_len(&path)
        );
    }

    #[test]
    fn remote_bridge_paths_distinguish_sanitized_targets_and_attempts() {
        let _guard = remote_env_lock().lock().unwrap();
        let targets = ["dev@box", "dev-box"];
        assert_ne!(
            local_forward_socket_path(targets[0], "default"),
            local_forward_socket_path(targets[1], "default"),
        );
        assert_ne!(
            local_forward_socket_path("box", "default"),
            local_forward_socket_path("box", "default"),
            "each registry entry and connection attempt owns its endpoint",
        );
    }

    #[cfg(unix)]
    #[test]
    fn local_forward_socket_path_fits_in_sun_path() {
        let _guard = remote_env_lock().lock().unwrap();
        // Worst case for the readable form: macOS-style 49-char TMPDIR +
        // max-length sanitized components. Should fall back to the hashed
        // short name, which fits under TMPDIR.
        let target = "longish-host.example.com";
        let session = "a-fairly-long-session-name-here";
        let path = local_forward_socket_path(target, session);
        assert!(
            fits_unix_socket_path(&path),
            "socket path too long for sun_path: {} ({} bytes)",
            path.display(),
            socket_path_byte_len(&path)
        );
    }

    #[cfg(unix)]
    #[test]
    fn local_forward_socket_path_falls_back_to_tmp_when_dir_is_long() {
        let _guard = remote_env_lock().lock().unwrap();
        // Force a TMPDIR long enough that even the hashed short name cannot
        // fit inside it. The fallback should drop to /tmp.
        let prior = std::env::var_os("TMPDIR");
        let long_dir = std::env::temp_dir().join("a".repeat(80));
        let _ = fs::create_dir_all(&long_dir);
        std::env::set_var("TMPDIR", &long_dir);

        let path = local_forward_socket_path("longish-host.example.com", "default");
        let fits = fits_unix_socket_path(&path);
        let parent = path.parent().map(Path::to_path_buf);
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        match prior {
            Some(v) => std::env::set_var("TMPDIR", v),
            None => std::env::remove_var("TMPDIR"),
        }
        let _ = fs::remove_dir_all(&long_dir);

        assert!(fits, "fallback path still overflows: {}", path.display());
        assert_eq!(parent.as_deref(), Some(Path::new("/tmp")));
        assert!(
            filename.starts_with("herdr-r-"),
            "expected hashed fallback, got {filename}"
        );
    }

    #[test]
    fn install_source_cleanup_removes_temporary_directory() {
        let dir = std::env::temp_dir().join(format!(
            "herdr-install-source-cleanup-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir(&dir).expect("create temp dir");
        let path = dir.join("herdr.tmp");
        fs::write(&path, b"test").expect("write temp file");

        InstallSource::temporary(path, dir.clone()).cleanup();

        assert!(!dir.exists());
    }

    #[test]
    fn remote_compatibility_errors_keep_their_classification() {
        let error = remote_compatibility_error("endpoint generation mismatch");

        assert!(is_remote_compatibility_error(&error));
        assert_eq!(error.to_string(), "endpoint generation mismatch");
    }
}
