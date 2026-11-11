//! A per-user file lock owns an ephemeral loopback activation endpoint.
//! Windows releases the lock after a crash; no stale socket removal is needed.
use anyhow::{Context as _, Result, bail};
use directories::ProjectDirs;
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_millis(250);

pub enum Instance {
    Primary(Arc<InstanceLifecycle>),
    Secondary,
}

pub struct InstanceLifecycle {
    // Keep the file locked until the listener has stopped. Never unlink it:
    // another process could otherwise lock a different file at the same path.
    _lock: Option<File>,
    activations: async_channel::Receiver<()>,
    shutdown: Arc<AtomicBool>,
    listener_thread: Option<JoinHandle<()>>,
}

impl InstanceLifecycle {
    #[cfg(test)]
    pub(crate) fn isolated() -> Arc<Self> {
        let (_, activations) = async_channel::bounded(1);
        Arc::new(Self {
            _lock: None,
            activations,
            shutdown: Arc::new(AtomicBool::new(false)),
            listener_thread: None,
        })
    }

    pub fn acquire() -> Result<Instance> {
        let dirs = ProjectDirs::from("dev", "twoducks", "cadence")
            .context("could not determine the Sonder cache directory")?;
        Self::acquire_at(dirs.cache_dir().join("activation.lock"))
    }

    fn acquire_at(path: PathBuf) -> Result<Instance> {
        fs::create_dir_all(path.parent().context("missing lifecycle directory")?)?;
        let endpoint_path = path.with_extension("endpoint");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .context("could not open lifecycle lock")?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                // A second process can start before the primary publishes its
                // endpoint. Retry boundedly without stealing the live lock.
                let deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    if request_activation(&endpoint_path).is_ok() {
                        return Ok(Instance::Secondary);
                    }
                    if Instant::now() >= deadline {
                        bail!("Sonder is running but its activation endpoint did not respond");
                    }
                    thread::sleep(Duration::from_millis(25));
                }
            }
            Err(TryLockError::Error(error)) => return Err(error.into()),
        }
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let secret = oauth2::CsrfToken::new_random().secret().clone();
        // Windows byte-range locks prevent other handles reading the locked
        // file. Publish discovery data separately while holding ownership.
        fs::write(&endpoint_path, format!("{port} {secret}\n"))?;
        let (sender, activations) = async_channel::bounded(1);
        let shutdown = Arc::new(AtomicBool::new(false));
        let stop = shutdown.clone();
        let listener_thread = thread::Builder::new()
            .name("cadence-activation".into())
            .spawn(move || {
                let expected = format!("activate {secret}\n").into_bytes();
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let _ = stream.set_read_timeout(Some(TIMEOUT));
                            let _ = stream.set_write_timeout(Some(TIMEOUT));
                            let mut message = vec![0; expected.len()];
                            if stream.read_exact(&mut message).is_ok() && message == expected {
                                let _ = sender.try_send(());
                                let _ = stream.write_all(b"ok\n");
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(50));
                        }
                        Err(_) => break,
                    }
                }
            })?;
        Ok(Instance::Primary(Arc::new(Self {
            _lock: Some(lock),
            activations,
            shutdown,
            listener_thread: Some(listener_thread),
        })))
    }

    pub fn take_activation(&self) -> bool {
        let mut activated = false;
        while self.activations.try_recv().is_ok() {
            activated = true;
        }
        activated
    }

    pub fn activation_receiver(&self) -> async_channel::Receiver<()> {
        self.activations.clone()
    }
}

fn request_activation(path: &std::path::Path) -> Result<()> {
    let mut endpoint = String::new();
    File::open(path)?.take(256).read_to_string(&mut endpoint)?;
    let (port, secret) = endpoint
        .trim()
        .split_once(' ')
        .context("activation endpoint not ready")?;
    let address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port.parse()?));
    let mut stream = TcpStream::connect_timeout(&address, TIMEOUT)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    stream.write_all(format!("activate {secret}\n").as_bytes())?;
    let mut ack = [0; 3];
    stream.read_exact(&mut ack)?;
    anyhow::ensure!(&ack == b"ok\n", "invalid activation acknowledgment");
    Ok(())
}

impl Drop for InstanceLifecycle {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(thread) = self.listener_thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_instance_activates_primary_and_lock_is_released_on_exit() {
        let path = std::env::temp_dir().join(format!(
            "cadence-windows-lifecycle-{}.lock",
            std::process::id()
        ));
        fs::write(&path, "stale endpoint").unwrap();
        let Instance::Primary(primary) = InstanceLifecycle::acquire_at(path.clone()).unwrap()
        else {
            panic!("primary expected")
        };
        assert!(matches!(
            InstanceLifecycle::acquire_at(path.clone()).unwrap(),
            Instance::Secondary
        ));
        assert!(primary.take_activation());
        drop(primary);
        let Instance::Primary(restarted) = InstanceLifecycle::acquire_at(path.clone()).unwrap()
        else {
            panic!("lock was not released")
        };
        drop(restarted);
        fs::remove_file(path.with_extension("endpoint")).unwrap();
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn unauthenticated_connections_do_not_activate_the_app() {
        let path =
            std::env::temp_dir().join(format!("cadence-windows-auth-{}.lock", std::process::id()));
        let Instance::Primary(primary) = InstanceLifecycle::acquire_at(path.clone()).unwrap()
        else {
            panic!("primary expected")
        };
        let endpoint = fs::read_to_string(path.with_extension("endpoint")).unwrap();
        let (port, secret) = endpoint.trim().split_once(' ').unwrap();
        let mut stream =
            TcpStream::connect((Ipv4Addr::LOCALHOST, port.parse::<u16>().unwrap())).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        stream
            .write_all(format!("activate {}\n", "!".repeat(secret.len())).as_bytes())
            .unwrap();
        assert!(stream.read_exact(&mut [0; 3]).is_err());
        assert!(!primary.take_activation());
        assert!(matches!(
            InstanceLifecycle::acquire_at(path.clone()).unwrap(),
            Instance::Secondary
        ));
        drop(primary);
        fs::remove_file(path.with_extension("endpoint")).unwrap();
        fs::remove_file(path).unwrap();
    }
}
