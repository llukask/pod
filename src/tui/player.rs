//! Audio playback via mpv's JSON IPC protocol.
//!
//! Spawns an mpv subprocess with `--input-ipc-server` pointing at a Unix
//! socket, then sends commands and polls playback state over that socket.
//! This is Linux-specific for now.

use std::path::PathBuf;
use std::process::Stdio;

use anyhow::Context;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};

/// State reported back to the TUI on each poll.
#[derive(Debug, Clone)]
pub struct PlaybackState {
    pub position_secs: i32,
    pub duration_secs: i32,
    pub paused: bool,
    /// true when mpv has exited or the file ended.
    pub finished: bool,
}

pub struct Player {
    process: Child,
    socket_path: PathBuf,
}

impl Player {
    /// Launch mpv playing the given audio URL. The player starts in the
    /// background and can be controlled via IPC.
    pub async fn start(audio_url: &str, start_position: i32) -> anyhow::Result<Self> {
        let socket_path = std::env::temp_dir().join(format!("pod-mpv-{}.sock", std::process::id()));

        // Remove stale socket if it exists.
        let _ = std::fs::remove_file(&socket_path);

        let mut process = Command::new("mpv")
            .arg("--no-video")
            .arg("--no-terminal")
            .arg(format!("--input-ipc-server={}", socket_path.display()))
            .arg(format!("--start=+{}", start_position))
            .arg("--")
            .arg(audio_url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| anyhow::anyhow!("failed to launch mpv (is it installed?): {}", e))?;

        // Wait briefly for mpv to create the socket.
        for _ in 0..20 {
            if socket_path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        if !socket_path.exists() {
            let _ = process.kill().await;
            anyhow::bail!("mpv did not create IPC socket at {}", socket_path.display());
        }

        Ok(Self {
            process,
            socket_path,
        })
    }

    /// Send a JSON IPC command and read the response.
    async fn ipc_command(&self, command: &[serde_json::Value]) -> anyhow::Result<serde_json::Value> {
        let mut stream = UnixStream::connect(&self.socket_path)
            .await
            .context("connect to mpv IPC socket")?;

        let msg = serde_json::json!({ "command": command });
        let mut bytes = serde_json::to_vec(&msg).context("serialize IPC command")?;
        bytes.push(b'\n');
        stream.write_all(&bytes).await.context("write to mpv IPC")?;

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.context("read mpv IPC response")?;

        let resp: serde_json::Value =
            serde_json::from_str(&line).context("parse mpv IPC response")?;
        Ok(resp)
    }

    /// Get a property from mpv. Returns None if the property doesn't exist
    /// or mpv has exited.
    async fn get_property(&self, name: &str) -> Option<serde_json::Value> {
        let resp = self
            .ipc_command(&[
                serde_json::json!("get_property"),
                serde_json::json!(name),
            ])
            .await
            .ok()?;

        if resp.get("error")?.as_str()? == "success" {
            resp.get("data").cloned()
        } else {
            None
        }
    }

    /// Poll the current playback state.
    pub async fn poll_state(&mut self) -> PlaybackState {
        // Check if the process has exited.
        if let Ok(Some(_)) = self.process.try_wait() {
            return PlaybackState {
                position_secs: 0,
                duration_secs: 0,
                paused: false,
                finished: true,
            };
        }

        let position = self
            .get_property("time-pos")
            .await
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);

        let duration = self
            .get_property("duration")
            .await
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);

        let paused = self
            .get_property("pause")
            .await
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        PlaybackState {
            position_secs: position as i32,
            duration_secs: duration as i32,
            paused,
            finished: false,
        }
    }

    pub async fn toggle_pause(&self) -> anyhow::Result<()> {
        self.ipc_command(&[
            serde_json::json!("cycle"),
            serde_json::json!("pause"),
        ])
        .await
        .context("toggle pause")?;
        Ok(())
    }

    pub async fn seek(&self, offset_secs: i32) -> anyhow::Result<()> {
        self.ipc_command(&[
            serde_json::json!("seek"),
            serde_json::json!(offset_secs),
            serde_json::json!("relative"),
        ])
        .await
        .context("seek")?;
        Ok(())
    }

    pub async fn stop(&mut self) -> anyhow::Result<()> {
        let _ = self.ipc_command(&[serde_json::json!("quit")]).await;
        let _ = self.process.wait().await;
        let _ = std::fs::remove_file(&self.socket_path);
        Ok(())
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // Best-effort cleanup: kill mpv if still running.
        let _ = self.process.start_kill();
        let _ = std::fs::remove_file(&self.socket_path);
    }
}
