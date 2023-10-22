use std::process::Command;

#[derive(Debug)]
pub struct VideoUtil {
    verbos: bool,
    index_dir: std::path::PathBuf,
    index_file: std::path::PathBuf,
}

impl VideoUtil {
    pub fn from_index<P>(index: P) -> anyhow::Result<Self>
    where
        P: AsRef<std::path::Path>,
    {
        let index = index.as_ref();
        let index_dir = index
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .to_path_buf();
        let index_file = index
            .file_name()
            .ok_or(anyhow::anyhow!("invalid index path"))?
            .to_os_string();
        Ok(Self {
            verbos: false,
            index_dir: index_dir,
            index_file: std::path::PathBuf::from(index_file),
        })
    }

    pub fn enable_verbose(&mut self) {
        self.verbos = true;
    }

    pub fn merge_to(&self, output: &str) -> anyhow::Result<()> {
        let output_path = std::fs::canonicalize(output)
            .unwrap_or_else(|_| std::env::current_dir().unwrap().join(output));

        let mut cmd = Command::new("ffmpeg");
        cmd.current_dir(&self.index_dir)
            .arg("-allowed_extensions")
            .arg("ALL")
            .arg("-i")
            .arg(&self.index_file)
            .arg("-codec")
            .arg("copy")
            .arg(&output_path);
        if self.verbos {
            cmd.stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit());
        }

        let output = cmd.output()?;
        if !output.status.success() {
            anyhow::bail!("ffmpeg failed: {}", String::from_utf8_lossy(&output.stderr));
        }

        Ok(())
    }

    pub fn play(&self) -> anyhow::Result<()> {
        let mut cmd = {
            if std::path::Path::new("/usr/bin/mpv").exists() {
                let mut cmd = Command::new("mpv");
                cmd.current_dir(&self.index_dir)
                    .arg(r#"--demuxer-lavf-o=allowed_extensions="ALL""#)
                    .arg(&self.index_file);
                cmd
            } else {
                let mut cmd = Command::new("ffplay");
                cmd.current_dir(&self.index_dir)
                    .arg("-allowed_extensions")
                    .arg("ALL")
                    .arg("-i")
                    .arg(&self.index_file);
                cmd
            }
        };

        if self.verbos {
            cmd.stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit());
        }
        let output = cmd.output()?;

        if !output.status.success() {
            anyhow::bail!("ffplay failed: {}", String::from_utf8_lossy(&output.stderr));
        }

        Ok(())
    }

    fn remove(&self, name: &str) -> anyhow::Result<()> {
        let mut path = std::path::PathBuf::from(name);
        if !path.is_absolute() {
            path = self.index_dir.join(name);
        }
        std::fs::remove_file(path.as_path()).map_err(|e| e.into())
    }

    pub fn clean_segment(self) -> anyhow::Result<()> {
        let m3u8_file = std::fs::read(self.index_dir.join(&self.index_file))?;
        let m3u8 = m3u8_rs::parse_media_playlist_res(&m3u8_file)
            .map_err(|e| anyhow::anyhow!("parse m3u8 failed {}", e))?;

        for seg in m3u8.segments {
            self.remove(&seg.uri)?;

            if let Some(key) = seg.key {
                if let Some(uri) = key.uri {
                    self.remove(&uri)?;
                }
            }
        }

        Ok(())
    }
}
