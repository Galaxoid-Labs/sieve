//! Running Tor ourselves, for people who have not got it running.
//!
//! Requiring a Tor daemon already on the machine is the minority position
//! among privacy wallets — Sparrow ships Tor binaries and starts an internal
//! proxy, Wasabi falls back to a bundled copy, Feather bundles it too — and it
//! is the difference between a privacy feature that is used and one that is
//! admired and left switched off.
//!
//! So the order is: use a proxy that is already listening; otherwise find a
//! `tor` binary and run it ourselves, with its own data directory and a port
//! nobody else chose. `packaging/` puts that binary inside the app, which is
//! what makes this work for someone who has never installed Tor.
//!
//! The child is tied to this process two ways: killed on `stop`, and started
//! with `__OwningControllerProcess`, which makes Tor exit by itself if Sieve
//! dies without getting the chance.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};

use super::Proxy;

/// How long to wait for a first-run bootstrap. Tor on a slow link, building
/// circuits from nothing, is not quick.
const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(120);

/// The Tor we started, if we started one.
static CHILD: Mutex<Option<Child>> = Mutex::new(None);

/// Where a `tor` binary might be, in the order worth trying.
///
/// The bundled copy comes before the system one deliberately: it is the
/// version this release was tested against, and on a Flatpak it is the only
/// one reachable anyway.
pub fn find_binary() -> Option<PathBuf> {
    // An explicit override, and how the tests inject a stand-in.
    if let Ok(path) = std::env::var("SIEVE_TOR") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }

    // Shipped beside the executable: /app/bin/tor in a Flatpak, or the
    // directory `scripts/fetch-tor.sh` unpacks into a development build. The
    // Expert Bundle keeps its libraries with the binary, so it arrives as a
    // directory rather than a bare file — both layouts are looked for.
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        for candidate in [dir.join("tor"), dir.join("tor").join("tor")] {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    // Installed on the machine.
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("tor"))
        .find(|candidate| candidate.is_file())
}

/// Is there a Tor we could run, one way or another?
pub fn available() -> bool {
    super::detect().is_some() || find_binary().is_some()
}

/// Get a working Tor proxy, starting one if nothing is listening.
///
/// Blocking, and slow on a first run: call it from a command, never on the
/// main thread. `progress` is handed bootstrap lines as they arrive, so the
/// interface can say something during the thirty seconds this can take.
pub fn ensure(mut progress: impl FnMut(String)) -> Result<Proxy> {
    // Already running — the system service, or Tor Browser. Nothing to start,
    // and nothing of ours to clean up.
    if let Some(proxy) = super::detect() {
        tracing::info!(%proxy, "using the Tor proxy already running");
        return Ok(proxy);
    }

    // One already started by us and still alive.
    if let Some(proxy) = running() {
        return Ok(proxy);
    }

    let binary = find_binary().ok_or_else(|| {
        anyhow!(
            "no Tor found. This build does not ship one, so install it — on Arch, \
             `sudo pacman -S tor` — and try again."
        )
    })?;
    tracing::info!(binary = %binary.display(), "starting Tor");
    progress("Starting Tor".into());

    let dir = crate::wallet::data_root().join("tor");
    std::fs::create_dir_all(&dir)?;
    // Tor refuses to start if anyone else can read its data directory, and it
    // is right to.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }

    // The Expert Bundle ships the libevent and OpenSSL Tor was built against,
    // and the system ones are not necessarily compatible — without this the
    // binary dies on an unresolved symbol before it logs anything. Only set
    // when those libraries are actually sitting beside the binary, so a system
    // Tor is left to the loader.
    let home = binary.parent().map(|dir| dir.to_path_buf());
    let bundled_libraries = home
        .as_ref()
        .map(|dir| dir.join("libevent-2.1.so.7").exists())
        .unwrap_or(false);

    let mut command = Command::new(&binary);
    if bundled_libraries && let Some(dir) = &home {
        command.env("LD_LIBRARY_PATH", dir);
    }
    // Tor complains on every start without these and works anyway; they travel
    // with the bundle, so hand them over when they are there.
    if let Some(dir) = &home {
        let geoip = dir.join("geoip");
        let geoip6 = dir.join("geoip6");
        if geoip.exists() && geoip6.exists() {
            command.arg("--GeoIPFile").arg(&geoip);
            command.arg("--GeoIPv6File").arg(&geoip6);
        }
    }

    let mut child = command
        // Let Tor pick the port and tell us which: choosing one ourselves
        // means racing whatever else on the machine wants it.
        .args(["--SocksPort", "auto"])
        .arg("--DataDirectory")
        .arg(&dir)
        .args(["--Log", "notice stdout"])
        .args(["--ClientOnly", "1"])
        .args(["--AvoidDiskWrites", "1"])
        // If Sieve dies without stopping Tor, Tor stops itself.
        .args(["--__OwningControllerProcess", &std::process::id().to_string()])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| anyhow!("could not start {}: {e}", binary.display()))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("Tor started without a log to read"))?;

    // A watchdog, so a Tor that says nothing at all cannot hang the caller:
    // killing it closes the pipe and ends the read below.
    let watched = child.id();
    std::thread::spawn(move || {
        std::thread::sleep(BOOTSTRAP_TIMEOUT);
        #[cfg(unix)]
        unsafe {
            // Only ever our own child, and harmless if it has already gone.
            libc::kill(watched as i32, libc::SIGTERM);
        }
    });

    *CHILD.lock().unwrap() = Some(child);

    let started = Instant::now();
    let mut port = None;
    for line in BufReader::new(stdout).lines() {
        let line = match line {
            Ok(line) => line,
            Err(e) => {
                stop();
                bail!("lost contact with Tor while it was starting: {e}");
            }
        };
        tracing::debug!(%line, "tor");

        if port.is_none()
            && let Some(found) = parse_socks_port(&line)
        {
            tracing::info!(port = found, "Tor opened its SOCKS port");
            port = Some(found);
        }

        if let Some(percent) = parse_bootstrap(&line) {
            progress(format!("Starting Tor — {percent}%"));
            if percent >= 100 {
                let Some(port) = port else {
                    stop();
                    bail!("Tor finished starting without opening a SOCKS port");
                };
                tracing::info!(seconds = started.elapsed().as_secs(), "Tor is ready");
                return Ok(Proxy::local(port));
            }
        }
    }

    // The pipe closed: Tor exited, or the watchdog ended it.
    stop();
    bail!(
        "Tor stopped before it finished starting. It may be blocked on this network, \
         or another copy may be using the same data directory."
    )
}

/// The proxy of a Tor we started and which is still alive.
fn running() -> Option<Proxy> {
    let mut guard = CHILD.lock().unwrap();
    let child = guard.as_mut()?;
    match child.try_wait() {
        // Still running, but we did not keep the port — the caller re-detects
        // rather than guessing.
        Ok(None) => super::detect(),
        _ => {
            *guard = None;
            None
        }
    }
}

/// Stop the Tor we started. Does nothing to one we merely borrowed.
pub fn stop() {
    let mut guard = CHILD.lock().unwrap();
    if let Some(mut child) = guard.take() {
        tracing::info!("stopping the Tor we started");
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// The port from Tor's "opened a listener" notice.
///
/// Its own function because the wording has changed between Tor versions and
/// this is the only thing standing between us and connecting to nothing.
fn parse_socks_port(line: &str) -> Option<u16> {
    if !line.contains("Opened Socks listener") {
        return None;
    }
    // "... on 127.0.0.1:39423" — and on a unix socket there is no port at all,
    // which is why this returns an Option rather than guessing.
    let address = line.rsplit(" on ").next()?;
    address.rsplit(':').next()?.trim().parse().ok()
}

/// The percentage from a bootstrap notice.
fn parse_bootstrap(line: &str) -> Option<u8> {
    let after = line.split("Bootstrapped ").nth(1)?;
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_socks_port_is_read_from_tors_notice() {
        let line = "Aug 29 20:14:02.000 [notice] Opened Socks listener connection \
                    (ready) on 127.0.0.1:39423";
        assert_eq!(parse_socks_port(line), Some(39423));

        // The older wording, still worth reading.
        assert_eq!(
            parse_socks_port("[notice] Opened Socks listener on 127.0.0.1:9050"),
            Some(9050)
        );

        // Not a listener line, and a listener with no port.
        assert_eq!(parse_socks_port("[notice] Bootstrapped 5%"), None);
        assert_eq!(
            parse_socks_port("[notice] Opened Socks listener on /run/tor/socks"),
            None
        );
    }

    #[test]
    fn bootstrap_progress_is_read() {
        assert_eq!(parse_bootstrap("[notice] Bootstrapped 0% (starting)"), Some(0));
        assert_eq!(parse_bootstrap("[notice] Bootstrapped 45% (requesting_descriptors)"), Some(45));
        assert_eq!(parse_bootstrap("[notice] Bootstrapped 100% (done): Done"), Some(100));
        assert_eq!(parse_bootstrap("[notice] Opened Socks listener"), None);
    }

    /// A stand-in for `tor` that logs what Tor logs, so the starting sequence
    /// is exercised without a Tor daemon: the port is read, progress is
    /// reported, and the proxy comes back pointing at what it announced.
    #[test]
    fn a_binary_that_behaves_like_tor_is_driven_to_ready() {
        let dir = std::env::temp_dir().join(format!("sieve-tor-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("tor");
        std::fs::write(
            &fake,
            "#!/bin/sh\n\
             echo '[notice] Opened Socks listener connection (ready) on 127.0.0.1:19051'\n\
             echo '[notice] Bootstrapped 10% (conn_done)'\n\
             echo '[notice] Bootstrapped 100% (done): Done'\n\
             sleep 30\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        // SAFETY: single-threaded test, and the variable is read by this test
        // only. Restored below.
        unsafe { std::env::set_var("SIEVE_TOR", &fake) };
        assert_eq!(find_binary().as_deref(), Some(fake.as_path()));

        let mut seen = Vec::new();
        let proxy = ensure(|message| seen.push(message)).unwrap();
        assert_eq!(proxy, Proxy::local(19051));
        assert!(seen.iter().any(|m| m.contains("10%")), "{seen:?}");
        assert!(seen.iter().any(|m| m.contains("100%")), "{seen:?}");

        stop();
        unsafe { std::env::remove_var("SIEVE_TOR") };
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod live {
    //! Against a real Tor, when there is one to run.
    //!
    //! Ignored by default: it starts Tor, waits for a bootstrap, and needs
    //! a working network. Run it with
    //! `cargo test -- --ignored --nocapture tor_actually_starts`.

    use super::*;

    #[test]
    #[ignore = "starts Tor and talks to the network"]
    fn tor_actually_starts_and_answers_as_tor() {
        // The test binary lives in target/debug/deps, so the bundle beside the
        // *app* binary is named directly rather than discovered.
        let bundled = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/debug/tor/tor");
        if bundled.is_file() {
            // SAFETY: single-threaded test.
            unsafe { std::env::set_var("SIEVE_TOR", &bundled) };
        }

        let binary = find_binary().expect("no Tor to start — run scripts/fetch-tor.sh");
        println!("using {}", binary.display());

        let proxy = ensure(|message| println!("{message}")).expect("Tor did not start");
        println!("proxy at {proxy}");

        // Not merely listening: answering RESOLVE, which only Tor does.
        let address = crate::tor::check(proxy).expect("the proxy is not Tor");
        println!("resolved example.com through Tor to {address}");

        // And the thing the node depends on: seeds, found through Tor.
        let seeds = crate::tor::resolve_seeds(proxy, "bitcoin", 2);
        println!("seeds through Tor: {seeds:?}");
        assert!(!seeds.is_empty(), "no Bitcoin seeds resolved through Tor");

        stop();
    }
}
