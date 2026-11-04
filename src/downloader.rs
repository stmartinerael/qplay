use anyhow::{Context, Result};
use rand::Rng;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{self, Receiver};
use std::thread;

pub struct DownloadJob {
    pub url: String,
    pub rx: Receiver<Result<PathBuf>>,
}

pub fn start_download(url: String) -> DownloadJob {
    let (tx, rx) = mpsc::channel();
    let url_clone = url.clone();

    thread::spawn(move || {
        let res = download_sync(&url_clone);
        let _ = tx.send(res);
    });

    DownloadJob { url, rx }
}

fn download_sync(url: &str) -> Result<PathBuf> {
    let temp_dir = std::env::temp_dir().join("qplay_downloads");
    std::fs::create_dir_all(&temp_dir)?;

    let mut rng = rand::thread_rng();
    let id: u64 = rng.r#gen();
    let file_path = temp_dir.join(format!("{}.mp4", id));

    let status = Command::new("yt-dlp")
        .arg("-f")
        .arg("bestvideo[ext=mp4]+bestaudio[ext=m4a]/best[ext=mp4]/best")
        .arg("-o")
        .arg(file_path.to_str().unwrap())
        .arg(url)
        .status()
        .context("Failed to execute yt-dlp. Is it installed?")?;

    if !status.success() {
        anyhow::bail!("yt-dlp returned non-zero status");
    }

    if !file_path.exists() {
        anyhow::bail!("yt-dlp succeeded but file was not created");
    }

    Ok(file_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_download_job_creation() {
        let job = start_download("https://example.com/test.mp4".to_string());
        assert_eq!(job.url, "https://example.com/test.mp4");
    }
}
