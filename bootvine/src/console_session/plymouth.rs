//! Plymouth owns the display until its Escape hook requests this viewer.
use std::{
    fs, io,
    os::unix::{fs::PermissionsExt, net::UnixDatagram},
    path::PathBuf,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const SOCKET: &str = "/run/bootvine/plymouth.sock";

pub struct Plymouth {
    socket: UnixDatagram,
    path: PathBuf,
    pub visible: bool,
}

pub fn command(action: &str) -> bool {
    let program =
        std::env::var_os("BOOTVINE_PLYMOUTH_COMMAND").unwrap_or_else(|| "plymouth".into());
    let Ok(mut child) = Command::new(program)
        .arg(action)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

impl Plymouth {
    pub fn open() -> io::Result<Self> {
        fs::create_dir_all("/run/bootvine")?;
        fs::set_permissions("/run/bootvine", fs::Permissions::from_mode(0o700))?;
        let path = PathBuf::from(SOCKET);
        // A killed viewer can leave a name, but never a listening socket.
        if path.exists() {
            let probe = UnixDatagram::unbound()?;
            if probe.connect(&path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "viewer already running",
                ));
            }
            fs::remove_file(&path)?;
        }
        let socket = UnixDatagram::bind(&path)?;
        socket.set_nonblocking(true)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        Ok(Self {
            socket,
            path,
            visible: false,
        })
    }

    pub fn requested(&self) -> bool {
        let mut data = [0; 16];
        matches!(self.socket.recv(&mut data), Ok(4)) && &data[..4] == b"show"
    }

    pub fn show(&mut self) -> bool {
        if !command("deactivate") {
            return false;
        }
        self.visible = true;
        true
    }

    pub fn hide(&mut self) -> bool {
        if self.visible && !command("reactivate") {
            return false;
        }
        self.visible = false;
        true
    }
}

impl Drop for Plymouth {
    fn drop(&mut self) {
        let _ = self.hide();
        let _ = fs::remove_file(&self.path);
    }
}
