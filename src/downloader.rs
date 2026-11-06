//! Cancellable downloads and ownership of their temporary files.
use anyhow::{Context, Result, bail};
use rand::Rng;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{
    Arc,
    mpsc::{self, Receiver, TryRecvError},
};
use std::thread;
use std::time::Duration;

pub struct DownloadedFile {
    path: PathBuf,
    directory: PathBuf,
}
impl DownloadedFile {
    pub fn path(&self) -> &Path {
        &self.path
    }
}
impl Drop for DownloadedFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

pub struct DownloadJob {
    rx: Receiver<Result<DownloadedFile>>,
    cancelled: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl DownloadJob {
    pub fn poll(&self) -> Result<Option<DownloadedFile>> {
        match self.rx.try_recv() {
            Ok(result) => result.map(Some),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => bail!("download worker stopped unexpectedly"),
        }
    }
}
impl Drop for DownloadJob {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub fn start_download(url: String) -> DownloadJob {
    start_with(url, PathBuf::from("yt-dlp"))
}
fn start_with(url: String, program: PathBuf) -> DownloadJob {
    let (tx, rx) = mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = Arc::clone(&cancelled);
    let worker = thread::spawn(move || {
        let result = download_sync(&url, &program, &worker_cancelled);
        // An unreceived successful result is dropped here and removes its directory.
        let _ = tx.send(result);
    });
    DownloadJob {
        rx,
        cancelled,
        worker: Some(worker),
    }
}
fn download_sync(url: &str, program: &Path, cancelled: &AtomicBool) -> Result<DownloadedFile> {
    let base = std::env::temp_dir().join("qplay_downloads");
    std::fs::create_dir_all(&base)?;
    let directory = loop {
        let candidate = base.join(format!("{:016x}", rand::thread_rng().r#gen::<u64>()));
        match std::fs::create_dir(&candidate) {
            Ok(()) => break candidate,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    };
    let file = DownloadedFile {
        path: directory.join("video.mp4"),
        directory,
    };
    let mut child = Command::new(program)
        .arg("-f")
        .arg("bestvideo[ext=mp4]+bestaudio[ext=m4a]/best[ext=mp4]/best")
        .arg("-o")
        .arg(&file.path)
        .arg("--")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("Failed to execute yt-dlp. Is it installed?")?;
    loop {
        if cancelled.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            bail!("download cancelled");
        }
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                bail!("yt-dlp exited with {status}");
            }
            if !file.path.is_file() {
                bail!("yt-dlp succeeded but the output file was not created");
            }
            return Ok(file);
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn successful_download_is_removed_when_result_is_dropped() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("fake-downloader");
        std::fs::write(&program, "#!/bin/sh\nprintf media > \"$4\"\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let job = start_with("https://example.invalid".into(), program);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let file = loop {
            match job.poll().unwrap() {
                Some(file) => break file,
                None if std::time::Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10))
                }
                None => panic!("download timed out"),
            }
        };
        let path = file.path().to_owned();
        assert_eq!(std::fs::read(&path).unwrap(), b"media");
        drop(file);
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn dropping_job_terminates_and_reaps_process() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("fake-downloader");
        let marker = dir.path().join("pid");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\nprintf '%s' \"$$\" > '{}'\nexec sleep 30\n",
                marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let job = start_with("https://example.invalid".into(), program);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !marker.exists() && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        let pid = std::fs::read_to_string(marker).unwrap();
        drop(job);
        assert!(!Path::new("/proc").join(pid).exists());
    }

    #[test]
    fn missing_downloader_reports_failure_without_network() {
        let job = start_with(
            "https://example.invalid".into(),
            PathBuf::from("/nonexistent/qplay-test-downloader"),
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            match job.poll() {
                Err(error) => {
                    assert!(error.to_string().contains("yt-dlp"));
                    break;
                }
                Ok(None) if std::time::Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10))
                }
                _ => panic!("download did not fail as expected"),
            }
        }
    }
}
