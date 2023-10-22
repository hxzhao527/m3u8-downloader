use serde::{Deserialize, Serialize};
use std::collections::HashMap;


#[derive(Debug, Deserialize, Serialize)]
pub struct DownloadRecord {
    pub target: url::Url,
    pub headers: HashMap<String, String>,
    pub m3u8_sum: String,
}

impl DownloadRecord {
    pub fn load<P>(dir: P) -> anyhow::Result<Self>
    where
        P: AsRef<std::path::Path>,
    {
        let path = dir.as_ref().join("record.json");
        let file = std::fs::File::open(path)?;

        let recorder = serde_json::from_reader(file)?;

        Ok(recorder)
    }

    pub fn save<P>(&self, dir: P) -> anyhow::Result<()>
    where
        P: AsRef<std::path::Path>,
    {
        let path = dir.as_ref().join("record.json");
        let file = std::fs::File::create(path)?;

        serde_json::to_writer_pretty(file, self)?;

        Ok(())
    }

    pub fn new(targer: url::Url, headers: HashMap<String, String>, m3u8_sum: String) -> Self {
        Self {
            target: targer,
            headers,
            m3u8_sum,
        }
    }
}
