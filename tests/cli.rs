//! The real binary: `run` fails cleanly when the port is taken and stops cleanly on Ctrl+C.

use std::{
    net::{SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

const BIN: &str = env!("CARGO_BIN_EXE_smart-claude-router");

/// `run` with an empty config dir, so the user's own config file never leaks into tests.
fn run_cmd(listen: SocketAddr, test: &str) -> Command {
    let config_home: PathBuf =
        std::env::temp_dir().join(format!("scr-cli-{test}-{}", std::process::id()));
    std::fs::create_dir_all(&config_home).unwrap();
    let mut cmd = Command::new(BIN);
    cmd.args(["run", "--listen", &listen.to_string()])
        .env("XDG_CONFIG_HOME", config_home)
        .env_remove("RUST_LOG")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    cmd
}

fn wait_with_timeout(mut child: Child, timeout: Duration) -> Output {
    let deadline = Instant::now() + timeout;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            child.kill().unwrap();
            panic!("process did not exit within {timeout:?}");
        }
        thread::sleep(Duration::from_millis(50));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn port_in_use_exits_with_a_clear_error() {
    let taken = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = taken.local_addr().unwrap();

    let child = run_cmd(addr, "port-in-use").spawn().unwrap();
    let output = wait_with_timeout(child, Duration::from_secs(10));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains(&format!("cannot listen on {addr}")),
        "{stderr}"
    );
    assert!(stderr.contains("already running"), "{stderr}");
}

/// Starts `run` with `args`, waits until it listens, stops it and returns what it printed.
#[cfg(unix)]
fn run_and_stop(test: &str, args: &[&str]) -> String {
    let addr = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let child = run_cmd(addr, test).args(args).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(addr).is_err() {
        assert!(Instant::now() < deadline, "proxy never started listening");
        thread::sleep(Duration::from_millis(50));
    }
    Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    let output = wait_with_timeout(child, Duration::from_secs(10));
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[cfg(unix)]
#[test]
fn startup_says_which_mode_it_runs_in() {
    let route = run_and_stop("mode-route", &[]);
    assert!(route.contains("mode: route, but rules are off"), "{route}");

    let dry = run_and_stop("mode-dry", &["--dry-run"]);
    assert!(dry.contains("mode: observe (dry run)"), "{dry}");
    assert!(dry.contains("nothing to observe"), "{dry}");
}

#[cfg(unix)]
#[test]
fn ctrl_c_stops_cleanly() {
    let addr = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let child = run_cmd(addr, "ctrl-c").spawn().unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(addr).is_err() {
        assert!(Instant::now() < deadline, "proxy never started listening");
        thread::sleep(Duration::from_millis(50));
    }
    let interrupted = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(interrupted.success());

    let output = wait_with_timeout(child, Duration::from_secs(10));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("shutting down") && stderr.contains("stopped"),
        "{stderr}"
    );
}
