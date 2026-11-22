//! Windows-only bridge from MCP stdio to the one desktop host for a storage root.
use lotus_session::supervisor::HostConfig;
use rmcp::ServiceExt;
use sha2::{Digest, Sha256};
use std::ffi::{c_void, OsString};
use std::fs::{self, File, TryLockError};
use std::io;
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::{
    Foundation::{
        LocalFree, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, HANDLE, HLOCAL,
    },
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SDDL_REVISION_1,
        },
        GetTokenInformation, TokenLogonSid, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
        TOKEN_GROUPS, TOKEN_QUERY,
    },
    System::Threading::{
        GetCurrentProcess, OpenProcessToken, CREATE_BREAKAWAY_FROM_JOB, DETACHED_PROCESS,
    },
};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_CLIENTS: usize = 64;

pub struct Endpoint {
    root: PathBuf,
    name: String,
    logon_sid: String,
}

impl Endpoint {
    pub fn for_config(config: &HostConfig) -> io::Result<Self> {
        fs::create_dir_all(&config.storage_root)?;
        let root = fs::canonicalize(&config.storage_root)?;
        let logon_sid = logon_sid()?;
        // A client requesting different asset or control permissions must not
        // accidentally inherit those of the desktop already using this root.
        let mut identity = config.clone();
        identity.storage_root = root.clone();
        identity.worker = std::path::absolute(&identity.worker)?;
        for path in identity
            .assets
            .values_mut()
            .chain(identity.asset_directories.values_mut())
            .chain(identity.workers.values_mut())
            .chain(
                identity
                    .source_roots
                    .values_mut()
                    .map(|root| &mut root.path),
            )
            .chain(
                identity
                    .build_profiles
                    .values_mut()
                    .map(|profile| &mut profile.root),
            )
        {
            *path = std::path::absolute(&*path)?;
        }
        let mut hash = Sha256::new();
        hash.update(logon_sid.as_bytes());
        hash.update(serde_json::to_vec(&identity).map_err(io::Error::other)?);
        Ok(Self {
            root,
            name: format!(r"\\.\pipe\lotus-mcp-v1-{:x}", hash.finalize()),
            logon_sid,
        })
    }

    pub fn claim_host(&self) -> io::Result<Option<File>> {
        self.try_lock("desktop-mcp.lock")
    }

    fn try_lock(&self, name: &str) -> io::Result<Option<File>> {
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.root.join(name))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(file)),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(error),
        }
    }

    fn connect(&self) -> io::Result<Option<NamedPipeClient>> {
        match ClientOptions::new().open(&self.name) {
            Ok(pipe) => Ok(Some(pipe)),
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(code) if code == ERROR_FILE_NOT_FOUND.0 as i32 || code == ERROR_PIPE_BUSY.0 as i32
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    pub fn bridge(&self, executable: &Path, arguments: &[OsString]) -> io::Result<()> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let result = runtime.block_on(async {
            let mut pipe = self.connect_or_start(executable, arguments).await?;
            let (mut read, mut write) = tokio::io::split(&mut pipe);
            let mut input = tokio::io::stdin();
            let mut output = tokio::io::stdout();
            tokio::select! {
                result = tokio::io::copy(&mut input, &mut write) => result?,
                result = tokio::io::copy(&mut read, &mut output) => result?,
            };
            Ok(())
        });
        // Stdin uses a blocking read. An exiting desktop must also let the
        // bridge exit when its stdio client is still holding that pipe open.
        runtime.shutdown_background();
        result
    }

    async fn connect_or_start(
        &self,
        executable: &Path,
        arguments: &[OsString],
    ) -> io::Result<NamedPipeClient> {
        let deadline = tokio::time::Instant::now() + STARTUP_TIMEOUT;
        let mut startup_lock = None;
        let mut child = None;
        loop {
            if let Some(pipe) = self.connect()? {
                return Ok(pipe);
            }
            if startup_lock.is_none() {
                startup_lock = self.try_lock("desktop-mcp-start.lock")?;
            }
            if startup_lock.is_some() && child.is_none() && self.claim_host()?.is_some() {
                let log = File::options()
                    .create(true)
                    .append(true)
                    .open(self.root.join("desktop-mcp.log"))?;
                let mut command = Command::new(executable);
                command
                    .args(arguments)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(log);
                child = Some(
                    match command
                        .creation_flags(DETACHED_PROCESS.0 | CREATE_BREAKAWAY_FROM_JOB.0)
                        .spawn()
                    {
                        Err(error)
                            if error.raw_os_error() == Some(ERROR_ACCESS_DENIED.0 as i32) =>
                        {
                            command.creation_flags(DETACHED_PROCESS.0).spawn()?
                        }
                        result => result?,
                    },
                );
            }
            if let Some(process) = &mut child {
                if process.try_wait()?.is_some_and(|status| !status.success()) {
                    return Err(io::Error::other(
                        "LOTUS desktop failed to start; see desktop-mcp.log in the storage root",
                    ));
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(io::Error::new(io::ErrorKind::TimedOut,
                    "cannot connect to the shared LOTUS desktop; check desktop-mcp.log and make sure the running window uses the same host configuration"));
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    }

    pub(crate) fn listen(&self, first: bool) -> io::Result<NamedPipeServer> {
        let sddl: Vec<u16> = format!("D:P(A;;GA;;;{})", self.logon_sid)
            .encode_utf16()
            .chain([0])
            .collect();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: SDDL is terminated and the returned descriptor is valid
        // until freed with LocalFree after CreateNamedPipe copies it.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )?;
            let mut attributes = SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: descriptor.0,
                bInheritHandle: false.into(),
            };
            let result = ServerOptions::new()
                .first_pipe_instance(first)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(
                    &self.name,
                    (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
                );
            LocalFree(Some(HLOCAL(descriptor.0)));
            result
        }
    }

    pub(crate) async fn serve(
        self,
        mut listener: NamedPipeServer,
        server: lotus_mcp::LotusServer,
    ) -> io::Result<()> {
        let mut clients = tokio::task::JoinSet::new();
        loop {
            while clients.len() >= MAX_CLIENTS {
                clients.join_next().await;
            }
            listener.connect().await?;
            // Keep an available instance named while another client connects.
            let next = self.listen(false)?;
            let connection = std::mem::replace(&mut listener, next);
            let server = server.clone();
            clients.spawn(async move {
                if let Ok(service) = server.serve(connection).await {
                    let _ = service.waiting().await;
                }
            });
            while clients.try_join_next().is_some() {}
        }
    }
}

fn logon_sid() -> io::Result<String> {
    let mut token = HANDLE::default();
    let mut bytes = 0;
    let mut sid = PWSTR::null();
    // SAFETY: The token is process-owned. Use an aligned buffer for the
    // returned TOKEN_GROUPS and keep it alive through SID conversion.
    unsafe {
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
        let _token = OwnedHandle::from_raw_handle(token.0);
        let _ = GetTokenInformation(token, TokenLogonSid, None, 0, &mut bytes);
        let mut groups = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
        GetTokenInformation(
            token,
            TokenLogonSid,
            Some(groups.as_mut_ptr().cast::<c_void>()),
            bytes,
            &mut bytes,
        )?;
        let groups = &*groups.as_ptr().cast::<TOKEN_GROUPS>();
        if groups.GroupCount != 1 {
            return Err(io::Error::other("expected one Windows logon SID"));
        }
        ConvertSidToStringSidW(groups.Groups[0].Sid, &mut sid)?;
        let text = sid.to_string();
        LocalFree(Some(HLOCAL(sid.0.cast())));
        text.map_err(io::Error::other)
    }
}
