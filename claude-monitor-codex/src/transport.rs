//! Native connection policy. Unix permissions and TCP bearer credentials are distinct.

use anyhow::{bail, Context, Result};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Endpoint {
    AuthenticatedWebSocket(String),
    PrivateUnix(PathBuf),
}

impl Endpoint {
    /// Parse trusted host configuration without filesystem access or process creation.
    pub fn parse(value: &str) -> Result<Self> {
        if let Some(path) = value.strip_prefix("unix://") {
            if !Path::new(path).is_absolute() || path.contains(['\0', '\n', '\r', '?', '#']) {
                bail!("Unix Codex endpoint requires an absolute socket path without query or fragment");
            }
            #[cfg(not(unix))]
            bail!("Private Unix Codex connections are not supported on this platform");
            #[cfg(unix)]
            return Ok(Self::PrivateUnix(PathBuf::from(path)));
        }
        let uri: tungstenite::http::Uri =
            value.parse().context("Parse Codex WebSocket endpoint")?;
        if uri.scheme_str() != Some("ws")
            || !matches!(
                uri.host(),
                Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
            )
            || uri.authority().is_some_and(|a| a.as_str().contains('@'))
        {
            bail!("Shared Codex WebSocket endpoint must use ws:// on loopback");
        }
        if uri.path_and_query().is_some_and(|p| p.query().is_some()) {
            bail!("Codex endpoint cannot include query credentials");
        }
        Ok(Self::AuthenticatedWebSocket(value.to_owned()))
    }

    pub fn is_unix(&self) -> bool {
        matches!(self, Self::PrivateUnix(_))
    }

    pub fn socket_path(&self) -> Option<&Path> {
        match self {
            Self::PrivateUnix(path) => Some(path),
            _ => None,
        }
    }
}

/// How long the native server may take to answer the WebSocket handshake, on either transport.
/// It was 2 s, and a loaded machine missed it three times in one day of browser suites ("Private
/// Unix WebSocket handshake failed", #s227) while every rerun alone passed. A handshake happens
/// once per connection; the connect itself keeps its 2 s bound, so a server that is not there is
/// still known at once.
pub(super) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) enum NativeStream {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
}

impl NativeStream {
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        match self {
            Self::Tcp(s) => s.set_read_timeout(timeout),
            #[cfg(unix)]
            Self::Unix(s) => s.set_read_timeout(timeout),
        }
    }
}
impl Read for NativeStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(s) => s.read(bytes),
            #[cfg(unix)]
            Self::Unix(s) => s.read(bytes),
        }
    }
}
impl Write for NativeStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(s) => s.write(bytes),
            #[cfg(unix)]
            Self::Unix(s) => s.write(bytes),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Tcp(s) => s.flush(),
            #[cfg(unix)]
            Self::Unix(s) => s.flush(),
        }
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::fs::{self, Metadata};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, MetadataExt},
        net::UnixStream,
    };

    fn uid() -> u32 {
        unsafe { libc::geteuid() }
    }

    // Ancestors may be root-owned (including sticky /tmp), but other users must
    // not be able to replace a path component. Canonicalize before checking them.
    fn protected_ancestors(path: &Path) -> Result<()> {
        for parent in path.ancestors() {
            let m = fs::symlink_metadata(parent)?;
            if !m.is_dir()
                || (m.uid() != uid() && m.uid() != 0)
                || (m.mode() & 0o022 != 0 && !(m.uid() == 0 && m.mode() & 0o1000 != 0))
            {
                bail!(
                    "Unix socket directory is replaceable by another user: {}",
                    parent.display()
                );
            }
        }
        Ok(())
    }

    struct VerifiedSocket {
        path: PathBuf,
        metadata: Metadata,
    }

    fn inspect(path: &Path) -> Result<VerifiedSocket> {
        let parent = path
            .parent()
            .context("Unix socket has no parent")?
            .canonicalize()?;
        protected_ancestors(&parent)?;
        let advertised = fs::symlink_metadata(path)
            .with_context(|| format!("Inspect Unix endpoint {}", path.display()))?;
        if advertised.uid() != uid()
            || (!advertised.file_type().is_socket() && !advertised.file_type().is_symlink())
        {
            bail!("Unix endpoint must be a socket or socket alias owned by this user");
        }
        // Codex's own rendezvous alias resolves to a short, private physical path.
        let physical = path
            .canonicalize()
            .with_context(|| format!(
                "Resolve private Codex socket {}. If this is a stale link, restart the Codex daemon or App that owns it from a terminal, or select a different private socket with --codex-server-url. Monitor leaves the entry unchanged",
                path.display()
            ))?;
        let parent = physical
            .parent()
            .context("Unix socket has no physical parent")?;
        protected_ancestors(parent)?;
        let directory = fs::metadata(parent)?;
        let metadata = fs::symlink_metadata(&physical)?;
        if directory.uid() != uid() || directory.mode() & 0o077 != 0 {
            bail!("Unix socket directory must be owner-only (0700)");
        }
        if !metadata.file_type().is_socket()
            || metadata.uid() != uid()
            || metadata.mode() & 0o077 != 0
        {
            bail!("Unix socket must be an owner-only socket (0600)");
        }
        Ok(VerifiedSocket {
            path: physical,
            metadata,
        })
    }

    /// Refuse stale or invalid entries before startup; never unlink or chmod them.
    pub fn check_start_path(path: &Path) -> Result<()> {
        match fs::symlink_metadata(path) {
            Ok(_) => {
                inspect(path)?;
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }

    fn connect_timeout(path: &Path) -> io::Result<UnixStream> {
        let bytes = path.as_os_str().as_bytes();
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        if bytes.len() >= address.sun_path.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Resolved Unix socket path is too long",
            ));
        }
        address.sun_family = libc::AF_UNIX as _;
        for (out, byte) in address.sun_path.iter_mut().zip(bytes) {
            *out = *byte as _;
        }
        let length = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1)
            as libc::socklen_t;
        #[cfg(any(
            target_os = "macos",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd"
        ))]
        {
            address.sun_len = length as _;
        }
        let raw = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // Own the descriptor before any fallible setup; never leak it on timeout.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0
            || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } < 0
        {
            return Err(io::Error::last_os_error());
        }
        if unsafe {
            libc::connect(
                fd.as_raw_fd(),
                (&address as *const libc::sockaddr_un).cast(),
                length,
            )
        } < 0
        {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINPROGRESS) {
                return Err(error);
            }
            let mut poll = libc::pollfd {
                fd: fd.as_raw_fd(),
                events: libc::POLLOUT,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut poll, 1, 2000) };
            if ready < 0 {
                return Err(io::Error::last_os_error());
            }
            if ready == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Unix socket connect timed out",
                ));
            }
            let mut error: libc::c_int = 0;
            let mut size = std::mem::size_of_val(&error) as libc::socklen_t;
            if unsafe {
                libc::getsockopt(
                    fd.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_ERROR,
                    (&mut error as *mut libc::c_int).cast(),
                    &mut size,
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
            if error != 0 {
                return Err(io::Error::from_raw_os_error(error));
            }
        }
        let stream = UnixStream::from(fd);
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(super::HANDSHAKE_TIMEOUT))?;
        stream.set_write_timeout(Some(super::HANDSHAKE_TIMEOUT))?;
        Ok(stream)
    }

    fn verify_peer(stream: &UnixStream) -> Result<()> {
        #[cfg(any(
            target_os = "macos",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd"
        ))]
        let peer = {
            let (mut user, mut group) = (0, 0);
            if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut user, &mut group) } < 0 {
                return Err(io::Error::last_os_error().into());
            }
            user
        };
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let peer = {
            let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
            let mut size = std::mem::size_of_val(&credentials) as libc::socklen_t;
            if unsafe {
                libc::getsockopt(
                    stream.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_PEERCRED,
                    (&mut credentials as *mut libc::ucred).cast(),
                    &mut size,
                )
            } < 0
            {
                return Err(io::Error::last_os_error().into());
            }
            credentials.uid
        };
        #[cfg(not(any(
            target_os = "macos",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "linux",
            target_os = "android"
        )))]
        let peer = {
            bail!("Unix peer identity checks are unsupported on this platform");
        };
        if peer != uid() {
            bail!("Codex Unix peer belongs to a different OS user");
        }
        Ok(())
    }

    pub fn connect(path: &Path) -> Result<NativeStream> {
        let before = inspect(path)?;
        let stream = connect_timeout(&before.path).context("Connect private Codex socket")?;
        verify_peer(&stream)?;
        let after = inspect(path)?;
        if before.path != after.path
            || before.metadata.dev() != after.metadata.dev()
            || before.metadata.ino() != after.metadata.ino()
        {
            bail!("Codex Unix socket changed during connection; retry");
        }
        Ok(NativeStream::Unix(stream))
    }
}

#[cfg(unix)]
pub(super) use unix::{check_start_path, connect as connect_unix};

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::{
        fs::{symlink, DirBuilderExt, PermissionsExt},
        net::UnixListener,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = PathBuf::from("/tmp").join(format!(
                "am-uds-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
        fn bind(&self) -> (PathBuf, UnixListener) {
            let path = self.0.join("rpc.sock");
            let listener = UnixListener::bind(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            (path, listener)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn endpoint_policy_keeps_unix_and_bearer_websocket_distinct() {
        assert!(matches!(
            Endpoint::parse("unix:///tmp/private/rpc.sock").unwrap(),
            Endpoint::PrivateUnix(_)
        ));
        assert!(matches!(
            Endpoint::parse("ws://127.0.0.1:4500").unwrap(),
            Endpoint::AuthenticatedWebSocket(_)
        ));
        for address in [
            "unix://",
            "unix://relative",
            "unix:///tmp/rpc.sock?token=x",
            "unix:///tmp/rpc.sock#x",
            "ws://user@localhost:4500",
            "ws://example.com:4500",
            "ws://localhost:4500?token=x",
        ] {
            assert!(Endpoint::parse(address).is_err(), "{address}");
        }
    }

    #[test]
    fn two_clients_connect_through_native_style_alias_without_tcp_or_token() {
        let fixture = Fixture::new();
        let (path, listener) = fixture.bind();
        let alias = fixture.0.join("advertised.sock");
        symlink(&path, &alias).unwrap();
        let mut first = connect_unix(&alias).unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        let mut second = connect_unix(&alias).unwrap();
        let (mut other, _) = listener.accept().unwrap();
        first.write_all(b"a").unwrap();
        second.write_all(b"b").unwrap();
        let mut byte = [0];
        peer.read_exact(&mut byte).unwrap();
        assert_eq!(&byte, b"a");
        other.read_exact(&mut byte).unwrap();
        assert_eq!(&byte, b"b");
        assert!(!fixture.0.join("codex-server.token").exists());
    }

    #[test]
    fn reconnect_rechecks_permissions_and_does_not_repair_foreign_state() {
        let fixture = Fixture::new();
        let (path, listener) = fixture.bind();
        drop(connect_unix(&path).unwrap());
        drop(listener.accept().unwrap());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(connect_unix(&path)
            .err()
            .unwrap()
            .to_string()
            .contains("owner-only socket"));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o666
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(connect_unix(&path)
            .err()
            .unwrap()
            .to_string()
            .contains("owner-only (0700)"));
        fs::set_permissions(&fixture.0, fs::Permissions::from_mode(0o700)).unwrap();
        drop(connect_unix(&path).unwrap());
        drop(listener.accept().unwrap());
        drop(listener);
        fs::remove_file(&path).unwrap();
        fs::write(&path, b"replacement").unwrap();
        assert!(connect_unix(&path).is_err());
        assert!(check_start_path(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"replacement");
    }

    #[test]
    fn alias_cannot_hide_public_physical_directory_or_dangling_target() {
        let advertised = Fixture::new();
        let physical = Fixture::new();
        let (path, _listener) = physical.bind();
        let alias = advertised.0.join("alias.sock");
        symlink(&path, &alias).unwrap();
        fs::set_permissions(&physical.0, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(connect_unix(&alias).is_err());
        fs::set_permissions(&physical.0, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(connect_unix(&alias).is_err());
        let error = format!("{:#}", check_start_path(&alias).unwrap_err());
        assert!(error.contains(&alias.display().to_string()));
        assert!(error.contains("restart the Codex daemon or App"));
        assert!(error.contains("--codex-server-url"));
        assert!(fs::symlink_metadata(&alias)
            .unwrap()
            .file_type()
            .is_symlink());
    }
}
