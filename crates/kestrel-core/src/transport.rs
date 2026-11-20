//! Local transport: a named pipe on Windows, a Unix socket elsewhere (dev only).

pub use imp::*;

#[cfg(windows)]
mod imp {
    use std::fs::File;
    use std::io;
    use std::os::windows::io::FromRawHandle;
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::{
        GetLastError, LocalFree, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ,
        GENERIC_WRITE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, WaitNamedPipeW, PIPE_READMODE_BYTE,
        PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };

    pub const PIPE_NAME: &str = r"\\.\pipe\kestrel-svc";
    /// SYSTEM and Administrators: full control. Logged-on interactive users: read/write.
    const PIPE_SDDL: &str = "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;IU)";

    pub type Stream = File;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    pub struct Listener {
        sd: PSECURITY_DESCRIPTOR,
        first: bool,
    }

    // The security descriptor is only read by CreateNamedPipeW.
    unsafe impl Send for Listener {}

    impl Listener {
        pub fn bind() -> io::Result<Self> {
            let mut sd: PSECURITY_DESCRIPTOR = null_mut();
            let sddl = wide(PIPE_SDDL);
            let ok = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut sd,
                    null_mut(),
                )
            };
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { sd, first: true })
        }

        /// Block until a client connects and return its stream.
        pub fn accept(&mut self) -> io::Result<Stream> {
            let name = wide(PIPE_NAME);
            let sa = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: self.sd,
                bInheritHandle: 0,
            };
            let mut open_mode = PIPE_ACCESS_DUPLEX;
            if self.first {
                // Fails if another process already owns the name (pipe squatting).
                open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
            }
            let h = unsafe {
                CreateNamedPipeW(
                    name.as_ptr(),
                    open_mode,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    PIPE_UNLIMITED_INSTANCES,
                    64 * 1024,
                    64 * 1024,
                    0,
                    &sa,
                )
            };
            if h == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            self.first = false;
            let file = unsafe { File::from_raw_handle(h as _) };
            let ok = unsafe { ConnectNamedPipe(h, null_mut()) };
            if ok == 0 && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED {
                return Err(io::Error::last_os_error());
            }
            Ok(file)
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            unsafe { LocalFree(self.sd as _) };
        }
    }

    pub fn connect() -> io::Result<Stream> {
        let name = wide(PIPE_NAME);
        for _ in 0..3 {
            let h = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    null_mut(),
                    OPEN_EXISTING,
                    0,
                    null_mut(),
                )
            };
            if h != INVALID_HANDLE_VALUE {
                return Ok(unsafe { File::from_raw_handle(h as _) });
            }
            if unsafe { GetLastError() } != ERROR_PIPE_BUSY {
                return Err(io::Error::last_os_error());
            }
            unsafe { WaitNamedPipeW(name.as_ptr(), 1000) };
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "kestrel-svc pipe busy",
        ))
    }
}

#[cfg(unix)]
mod imp {
    use std::io;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;

    pub type Stream = UnixStream;

    pub fn socket_path() -> PathBuf {
        std::env::var_os("KESTREL_SOCKET")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("kestrel-svc.sock"))
    }

    pub struct Listener(UnixListener);

    impl Listener {
        pub fn bind() -> io::Result<Self> {
            let path = socket_path();
            let _ = std::fs::remove_file(&path);
            UnixListener::bind(path).map(Self)
        }

        pub fn accept(&mut self) -> io::Result<Stream> {
            self.0.accept().map(|(s, _)| s)
        }
    }

    pub fn connect() -> io::Result<Stream> {
        UnixStream::connect(socket_path())
    }
}

/// Connect to the service and complete the version handshake.
pub fn client() -> std::io::Result<crate::ipc::Client<Stream>> {
    crate::ipc::Client::handshake(connect()?)
}
