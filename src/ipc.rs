//! Line-based Unix socket between `cosmic-ws-isolate switch` and the daemon.

use anyhow::{Context, Result, bail};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::{
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
    path::PathBuf,
    process::Command,
    sync::mpsc,
    thread,
    time::Duration,
};
use tracing::{debug, warn};

/// One socket per compositor, so a nested test compositor never talks to the real session's daemon.
pub fn socket_path() -> Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".into());
    let display = display.rsplit('/').next().unwrap_or("wayland-0").to_owned();
    Ok(PathBuf::from(runtime).join(format!("cosmic-ws-isolate-{display}.sock")))
}

pub struct Request {
    pub line: String,
    pub reply: mpsc::Sender<String>,
}

/// Binds the socket and forwards each request line to `forward`, waiting for its reply.
pub fn serve(forward: impl Fn(Request) -> bool + Send + 'static) -> Result<()> {
    let path = socket_path()?;
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            bail!("another daemon is already listening on {}", path.display());
        }
        std::fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            if let Err(err) = handle(stream, &forward) {
                debug!("ipc client error: {err:#}");
            }
        }
    });
    Ok(())
}

fn handle(stream: UnixStream, forward: &impl Fn(Request) -> bool) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    stream.set_write_timeout(Some(Duration::from_millis(500)))?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    let (tx, rx) = mpsc::channel();
    let answer = if forward(Request {
        line: line.trim().to_owned(),
        reply: tx,
    }) {
        rx.recv_timeout(Duration::from_secs(1))
            .unwrap_or_else(|_| "err timeout".into())
    } else {
        "err daemon shutting down".into()
    };
    (&stream).write_all(answer.as_bytes())?;
    (&stream).write_all(b"\n")?;
    Ok(())
}

pub fn request(line: &str) -> Result<String> {
    let path = socket_path()?;
    let stream = UnixStream::connect(&path).with_context(|| format!("connecting to {}", path.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_millis(500)))?;
    (&stream).write_all(format!("{line}\n").as_bytes())?;
    let mut answer = String::new();
    BufReader::new(&stream).read_line(&mut answer)?;
    if answer.is_empty() {
        bail!("daemon closed the connection");
    }
    Ok(answer.trim_end().to_owned())
}

/// Used by the Alt+Tab binding. If the daemon can't be reached, hand over to the stock
/// COSMIC switcher so Alt+Tab keeps working.
pub fn switch(dir: &str) -> Result<()> {
    match request(&format!("switch {dir}")) {
        Ok(answer) if answer.starts_with("ok") => Ok(()),
        Ok(answer) => {
            debug!("daemon: {answer}");
            Ok(())
        }
        Err(err) => {
            warn!("daemon unavailable ({err:#}); falling back to cosmic-launcher");
            let arg = if dir == "prev" { "shift-alt-tab" } else { "alt-tab" };
            // exec keeps XDG_ACTIVATION_TOKEN and friends from cosmic-comp.
            let err = Command::new("cosmic-launcher").arg(arg).exec();
            Err(err).context("running cosmic-launcher")
        }
    }
}
