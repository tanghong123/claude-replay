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

/// #s145: a 401 to the probe proves the server asks for a credential, not WHOSE server it is.
/// Any account may bind a loopback port above 1023, so a process of another user holding the
/// configured port (after a reboot, before native mode starts) would answer the probe and then
/// receive the Monitor's credential. So the credential goes out only on a connection whose
/// SERVER end belongs to this OS user, read for that very connection.
///
/// Until the server accepts, no process holds its end: macOS's `lsof` does not list it, and Linux
/// lists it with inode 0 (and, before 6.17, root's uid — which a root Monitor would take for its
/// own). Either is no owner yet, so the owner is read again until it is this user or `OWNER_WAIT`
/// passes, which only a refusal ever waits out.
pub(super) fn tcp_server_owner(stream: &TcpStream) -> Option<u32> {
    let (Ok(server), Ok(client)) = (stream.peer_addr(), stream.local_addr()) else {
        return None;
    };
    let me = effective_uid()?;
    let deadline = std::time::Instant::now() + OWNER_WAIT;
    poll_owner(
        || tcp_owner::lookup(server, client),
        me,
        deadline,
        std::thread::sleep,
    )
}

/// A real server holds its accepted connection within milliseconds; the bound is for a loaded
/// machine (#s227 saw a handshake take over 2 s), and it is spent in full only on a refusal.
const OWNER_WAIT: Duration = Duration::from_secs(5);

/// Read the owner until it is `me` or the deadline passes, and return the last answer.
fn poll_owner(
    mut lookup: impl FnMut() -> Option<u32>,
    me: u32,
    deadline: std::time::Instant,
    mut sleep: impl FnMut(Duration),
) -> Option<u32> {
    let mut pause = Duration::from_millis(10);
    loop {
        let owner = lookup();
        if owner == Some(me) || std::time::Instant::now() >= deadline {
            return owner;
        }
        sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(250));
    }
}

/// May the credential go to a server whose end of the connection `owner` holds? Only to this
/// OS user's. The refusal names no address and nothing the peer sent.
pub(super) fn credential_may_go_to(owner: Option<u32>) -> Result<(), String> {
    match (owner, effective_uid()) {
        (Some(owner), Some(me)) if owner == me => Ok(()),
        (Some(_), Some(_)) => Err(
            "The Codex server's port belongs to another OS user; the Monitor credential was not sent"
                .into(),
        ),
        _ => Err(
            "Could not verify that the Codex server's port belongs to this OS user; the Monitor credential was not sent"
                .into(),
        ),
    }
}

#[cfg(unix)]
fn effective_uid() -> Option<u32> {
    Some(unsafe { libc::geteuid() })
}

#[cfg(not(unix))]
fn effective_uid() -> Option<u32> {
    None
}

/// Who holds the server end of one loopback TCP connection, per platform. A platform with no
/// way to tell answers `None`, which is a refusal.
mod tcp_owner {
    use std::net::SocketAddr;

    /// Both tables: a dual-stack server (`[::]`) holds a v4 connection's end in `tcp6`, as
    /// `::ffff:127.0.0.1`.
    #[cfg(target_os = "linux")]
    pub(super) fn lookup(server: SocketAddr, client: SocketAddr) -> Option<u32> {
        ["/proc/net/tcp", "/proc/net/tcp6"]
            .iter()
            .find_map(|table| proc_net_owner(&std::fs::read_to_string(table).ok()?, server, client))
    }

    /// As this user, `lsof` lists only this user's processes (measured on macOS 26: it finds
    /// nothing for a socket root holds), so another account's server is never found.
    #[cfg(target_os = "macos")]
    pub(super) fn lookup(server: SocketAddr, client: SocketAddr) -> Option<u32> {
        let host = match client {
            SocketAddr::V4(a) => a.ip().to_string(),
            SocketAddr::V6(a) => format!("[{}]", a.ip()),
        };
        let mut child = std::process::Command::new("/usr/sbin/lsof")
            .args(["-nP", "-w", "-Fpun"])
            .arg(format!("-iTCP@{host}:{}", client.port()))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(5))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }
        let mut out = String::new();
        std::io::Read::read_to_string(child.stdout.as_mut()?, &mut out).ok()?;
        lsof_owner(&out, &format!("{server}->{client}"))
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    pub(super) fn lookup(_server: SocketAddr, _client: SocketAddr) -> Option<u32> {
        None
    }

    /// The `uid` of the `/proc/net/tcp` (or `tcp6`) row whose local end is `server` and whose
    /// remote end is `client`: the server's socket for this connection. A row with inode 0 is
    /// a socket no process holds yet (not accepted, or already closed), which has no owner.
    #[cfg(any(target_os = "linux", test))]
    pub(super) fn proc_net_owner(
        contents: &str,
        server: SocketAddr,
        client: SocketAddr,
    ) -> Option<u32> {
        contents.lines().skip(1).find_map(|line| {
            let mut f = line.split_whitespace();
            let (local, remote) = (proc_addr(f.nth(1)?)?, proc_addr(f.next()?)?);
            if (local, remote) != (server, client) {
                return None;
            }
            // After rem_address: st, tx:rx, tr:tm, retrnsmt, uid, timeout, inode.
            let uid = f.nth(4)?.parse().ok()?;
            (f.nth(1)?.parse::<u64>().ok()? != 0).then_some(uid)
        })
    }

    /// `0100007F:1194` or 32 hex digits and a port: each 32-bit word is printed as the number
    /// its bytes make in memory, in host byte order; the port as big-endian hex. A v4-mapped
    /// address (a dual-stack socket's) reads as the v4 address it maps.
    #[cfg(any(target_os = "linux", test))]
    fn proc_addr(field: &str) -> Option<SocketAddr> {
        let (hex, port) = field.split_once(':')?;
        let port = u16::from_str_radix(port, 16).ok()?;
        let mut bytes = Vec::with_capacity(16);
        for word in 0..hex.len() / 8 {
            let value = u32::from_str_radix(hex.get(word * 8..word * 8 + 8)?, 16).ok()?;
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
        let ip = match (hex.len(), bytes.len()) {
            (8, 4) => std::net::IpAddr::from(<[u8; 4]>::try_from(bytes).ok()?),
            (32, 16) => std::net::IpAddr::from(<[u8; 16]>::try_from(bytes).ok()?),
            _ => return None,
        };
        Some(SocketAddr::new(ip.to_canonical(), port))
    }

    /// The `u` (process user id) of the process whose open file is named exactly `name` in
    /// `lsof -F pun` output: `p<pid>` and `u<uid>` open each process, `f`/`n` each file.
    #[cfg(any(target_os = "macos", test))]
    pub(super) fn lsof_owner(output: &str, name: &str) -> Option<u32> {
        let mut uid = None;
        for line in output.lines() {
            match line.split_at_checked(1) {
                Some(("p", _)) => uid = None,
                Some(("u", value)) => uid = value.parse().ok(),
                Some(("n", value)) if value == name => return uid,
                _ => {}
            }
        }
        None
    }
}

#[cfg(test)]
mod owner_tests {
    use super::tcp_owner::{lsof_owner, proc_net_owner};
    use super::*;
    use std::net::SocketAddr;

    const SERVER: &str = "127.0.0.1:4500";
    const CLIENT: &str = "127.0.0.1:61234";

    fn addr(text: &str) -> SocketAddr {
        text.parse().unwrap()
    }

    /// #s145: the row that names the SERVER's end of this connection decides, never the
    /// client's own row, the listener's, or another connection to the same port; and a row no
    /// process holds yet (inode 0, root's uid before Linux 6.17) is no owner. The hex is a
    /// little-endian host's.
    #[cfg(target_endian = "little")]
    #[test]
    fn proc_net_names_the_owner_of_the_servers_end_of_this_connection() {
        let v4 = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
           0: 0100007F:1194 00000000:0000 0A 00000000:00000000 00:00000000 00000000   501        0 1 1\n\
           1: 0100007F:EF32 0100007F:1194 01 00000000:00000000 00:00000000 00000000   502        0 2 1\n\
           2: 0100007F:1194 0100007F:EF33 01 00000000:00000000 00:00000000 00000000   503        0 3 1\n\
           3: 0100007F:1194 0100007F:EF32 01 00000000:00000000 00:00000000 00000000   504        0 4 1\n";
        assert_eq!(proc_net_owner(v4, addr(SERVER), addr(CLIENT)), Some(504));
        assert_eq!(
            proc_net_owner(v4, addr(SERVER), addr("127.0.0.1:1")),
            None,
            "no row for this connection is no owner"
        );
        let v6 = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
           0: 00000000000000000000000001000000:1194 00000000000000000000000001000000:EF32 01 00000000:00000000 00:00000000 00000000   505        0 5 1\n";
        assert_eq!(
            proc_net_owner(v6, addr("[::1]:4500"), addr("[::1]:61234")),
            Some(505)
        );
        assert_eq!(proc_net_owner("", addr(SERVER), addr(CLIENT)), None);
        let unaccepted = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
           0: 0100007F:1194 0100007F:EF32 01 00000000:00000000 00:00000000 00000000     0        0 0 1\n";
        assert_eq!(
            proc_net_owner(unaccepted, addr(SERVER), addr(CLIENT)),
            None,
            "a socket no process holds yet is no owner, not root's"
        );
        let dual_stack = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
           0: 0000000000000000FFFF00000100007F:1194 0000000000000000FFFF00000100007F:EF32 01 00000000:00000000 00:00000000 00000000   506        0 6 1\n";
        assert_eq!(
            proc_net_owner(dual_stack, addr(SERVER), addr(CLIENT)),
            Some(506),
            "a dual-stack server holds a v4 connection's end as ::ffff:127.0.0.1"
        );
    }

    /// The process that holds a file named exactly `<server>-><client>` decides; this process's
    /// own end of the connection (`<client>-><server>`) is not the server.
    #[test]
    fn lsof_names_the_process_holding_the_servers_end() {
        let out = "p100\nu501\nf7\nn127.0.0.1:61234->127.0.0.1:4500\np200\nu502\nf9\nn127.0.0.1:4500->127.0.0.1:61234\n";
        let want = format!("{}->{}", addr(SERVER), addr(CLIENT));
        assert_eq!(lsof_owner(out, &want), Some(502));
        assert_eq!(
            lsof_owner("p100\nu501\nf7\nn127.0.0.1:61234->127.0.0.1:4500\n", &want),
            None
        );
        assert_eq!(lsof_owner("", &want), None);
        let v6 = format!("{}->{}", addr("[::1]:4500"), addr("[::1]:61234"));
        assert_eq!(
            lsof_owner("p3\nu9\nf4\nn[::1]:4500->[::1]:61234\n", &v6),
            Some(9)
        );
    }

    #[test]
    fn the_owner_is_read_again_until_it_is_this_user_or_time_runs_out() {
        let now = std::time::Instant::now();
        let mut answers = vec![None, Some(0), Some(7)].into_iter();
        let mut sleeps = 0;
        assert_eq!(
            poll_owner(
                || answers.next().flatten(),
                7,
                now + OWNER_WAIT,
                |_| sleeps += 1
            ),
            Some(7)
        );
        assert_eq!(
            sleeps, 2,
            "a socket not yet held, then one read as root, then ours"
        );
        assert_eq!(
            poll_owner(|| Some(0), 7, now, |_| panic!("no wait past the deadline")),
            Some(0)
        );
    }

    #[cfg(unix)]
    #[test]
    fn only_this_users_server_may_receive_the_credential() {
        let me = effective_uid().unwrap();
        assert!(credential_may_go_to(Some(me)).is_ok());
        let foreign = credential_may_go_to(Some(me + 1)).unwrap_err();
        assert!(foreign.contains("another OS user"), "{foreign}");
        let unknown = credential_may_go_to(None).unwrap_err();
        assert!(unknown.contains("Could not verify"), "{unknown}");
    }

    /// The real lookup on this machine: a connection nobody has accepted has no owner yet, even
    /// though this user's listener is waiting for it.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_connection_nobody_holds_yet_has_no_owner() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, local) = (client.peer_addr().unwrap(), client.local_addr().unwrap());
        assert_eq!(tcp_owner::lookup(server, local), None);
        drop(listener);
    }

    /// The real lookup on this machine: a listener of this test process is this user's.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn a_server_of_this_user_is_found_as_this_user() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_held, _) = listener.accept().unwrap();
        assert_eq!(tcp_server_owner(&client), effective_uid());
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
