use std::fs;
use std::io::Write;
use std::sync::Arc;

use bytes::Bytes;
use indicatif::{ProgressBar, ProgressStyle};
use m3u8_rs::{MediaPlaylist, Playlist};
use md5::{Digest, Md5};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

mod cache;
mod video;

pub use video::VideoUtil;

fn basename(src: &str) -> &str {
    std::path::Path::new(src)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
}

fn clean_dir<P>(path: P) -> anyhow::Result<()>
where
    P: AsRef<std::path::Path>,
{
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(path).map_err(|e| e.into())
}

fn save_bytes<P>(path: P, bytes: &[u8]) -> anyhow::Result<()>
where
    P: AsRef<std::path::Path>,
{
    let mut writing = path.as_ref().to_path_buf();
    writing.as_mut_os_string().push(".writing");

    let mut file = fs::File::create(&writing)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);

    fs::rename(&writing, path)?;

    Ok(())
}

#[derive(Debug)]
struct DownloaderBuilderPart {
    target: url::Url,
    save_dir: Option<std::path::PathBuf>,
    index_name: String,

    headers: HeaderMap,
    client: Option<reqwest::Client>,

    max_download_concurrency: usize,
}

#[derive(Debug)]
pub struct DownloaderBuilder {
    inner: anyhow::Result<DownloaderBuilderPart>,
}

impl DownloaderBuilder {
    pub const INDEX_FILE_NAME: &'static str = "index.m3u8";

    pub fn new<U>(target: U) -> Self
    where
        U: reqwest::IntoUrl,
    {
        let target = target.into_url();

        if target.is_err() {
            return Self {
                inner: Err(anyhow::anyhow!("invalid url")),
            };
        }

        Self {
            inner: Ok(DownloaderBuilderPart {
                target: target.unwrap(),
                save_dir: None,
                index_name: Self::INDEX_FILE_NAME.to_string(),
                headers: HeaderMap::new(),
                client: None,
                max_download_concurrency: 10,
            }),
        }
    }

    pub fn header<K, V>(self, key: K, val: V) -> Self
    where
        HeaderName: TryFrom<K>,
        <HeaderName as TryFrom<K>>::Error: Into<anyhow::Error>,
        HeaderValue: TryFrom<V>,
        <HeaderValue as TryFrom<V>>::Error: Into<anyhow::Error>,
    {
        match self.inner {
            Ok(mut part) => match <HeaderName as TryFrom<K>>::try_from(key) {
                Ok(key) => match <HeaderValue as TryFrom<V>>::try_from(val) {
                    Ok(val) => {
                        part.headers.insert(key, val);
                        Self { inner: Ok(part) }
                    }
                    Err(e) => Self {
                        inner: Err(anyhow::anyhow!("invalid header value: {}", e.into())),
                    },
                },
                Err(e) => Self {
                    inner: Err(anyhow::anyhow!("invalid header key: {}", e.into())),
                },
            },
            Err(e) => Self { inner: Err(e) },
        }
    }

    pub fn save_dir<P>(self, dir: P) -> Self
    where
        P: AsRef<std::path::Path>,
    {
        match self.inner {
            Ok(mut part) => {
                part.save_dir = Some(dir.as_ref().to_path_buf());
                Self { inner: Ok(part) }
            }
            Err(e) => Self { inner: Err(e) },
        }
    }

    pub fn client(self, client: reqwest::Client) -> Self {
        match self.inner {
            Ok(mut part) => {
                part.client = Some(client);
                Self { inner: Ok(part) }
            }
            Err(e) => Self { inner: Err(e) },
        }
    }

    pub fn max_download_concurrency(self, max: usize) -> Self {
        match self.inner {
            Ok(mut part) => {
                part.max_download_concurrency = max;
                Self { inner: Ok(part) }
            }
            Err(e) => Self { inner: Err(e) },
        }
    }

    pub async fn download(self) -> anyhow::Result<()> {
        if self.inner.is_err() {
            return Err(self.inner.unwrap_err());
        }
        let part = self.inner.unwrap();
        let client = {
            if part.client.is_some() {
                part.client.unwrap()
            } else {
                reqwest::Client::new()
            }
        };

        let downloader = Arc::new(Downloader {
            target: part.target,
            save_dir: part.save_dir.unwrap_or(std::path::PathBuf::from(".")),
            index_name: part.index_name,
            client: client,
            header: part.headers,
            max_download_concurrency: part.max_download_concurrency,
        });

        tracing::info!("downloader: {:?}", &downloader.target);
        downloader.download().await
    }
}

#[derive(Debug)]
pub struct Downloader {
    target: url::Url,
    save_dir: std::path::PathBuf,
    index_name: String,

    client: reqwest::Client,
    header: HeaderMap,
    max_download_concurrency: usize,
}

impl Downloader {
    async fn load_m3u8(&self) -> anyhow::Result<M3U8MediaPlaylist> {
        let mut uri = self.target.clone();
        let mut result = Option::<(Playlist, Bytes)>::None;

        loop {
            match result.take() {
                None => {
                    let bytes = self.get(uri.as_str()).send().await?.bytes().await?;
                    match m3u8_rs::parse_playlist_res(&bytes) {
                        Ok(any) => {
                            result = Some((any, bytes));
                        }
                        Err(e) => {
                            return Err(anyhow::anyhow!("parse m3u8 error: {}", e));
                        }
                    }
                }
                Some((Playlist::MasterPlaylist(master), _)) => {
                    tracing::info!("find master playlist, try to get real stream");
                    let one = master.variants.iter().max_by(|a, b| {
                        if a.frame_rate.is_some() && b.frame_rate.is_some() {
                            //return a.frame_rate.unwrap().cmp(&b.frame_rate.unwrap());
                        }
                        if a.resolution.is_some() && b.resolution.is_some() {
                            return a.resolution.unwrap().cmp(&b.resolution.unwrap());
                        }
                        return a.bandwidth.cmp(&b.bandwidth);
                    });

                    if let Some(stream) = one {
                        match uri.join(&stream.uri) {
                            Ok(stream_uri) => {
                                uri = stream_uri;
                                tracing::info!("master redirect to: {:?}", &uri);
                                continue;
                            }
                            Err(e) => {
                                return Err(anyhow::anyhow!("parse m3u8 error: {}", e));
                            }
                        }
                    }
                    return Err(anyhow::anyhow!("parse m3u8 error: no stream"));
                }
                Some((Playlist::MediaPlaylist(media), bytes)) => {
                    let mut hasher = Md5::new();
                    hasher.update(bytes);
                    let sum = format!("{:x}", hasher.finalize());

                    let mut media = M3U8MediaPlaylist::new(media, sum);
                    media.set_base_url(uri);
                    return Ok(media);
                }
            }
        }
    }

    fn get(&self, url: &str) -> reqwest::RequestBuilder {
        self.client.get(url).headers(self.header.clone())
    }

    async fn download_m3u8_part(&self, uri: &str) -> anyhow::Result<()> {
        let name = basename(uri);
        let save_path = self.save_dir.as_path().join(name);
        if save_path.exists() {
            return Ok(());
        }

        let bytes = self.get(uri).send().await?.bytes().await?;
        save_bytes(&save_path, &bytes)
    }

    /// download m3u8
    pub async fn download(self: Arc<Self>) -> anyhow::Result<()> {
        // load m3u8 full bytes
        // merge m3u8 need three parts
        // 1. m3u8 file self
        // 2. key in m3u8
        // 3. segment in m3u8

        let media = self.load_m3u8().await?;
        std::fs::create_dir_all(&self.save_dir)?;

        match cache::DownloadRecord::load(&self.save_dir) {
            Ok(record) if record.m3u8_sum == media.content_sum() => {}
            _ => {
                tracing::warn!(
                    "cache not match or not exist, clean dir: {:?}",
                    &self.save_dir
                );
                clean_dir(&self.save_dir)?;
                let record = cache::DownloadRecord::new(
                    self.target.clone(),
                    self.header
                        .clone()
                        .into_iter()
                        .map(|(k, v)| (k.unwrap().to_string(), v.to_str().unwrap().to_string()))
                        .collect(),
                    media.content_sum().to_string(),
                );
                record.save(&self.save_dir)?;
                tracing::info!("save record: {:?}", &record);
            }
        }

        if let Some(key) = media.key() {
            self.download_m3u8_part(&key).await?;
            tracing::info!("key downloaded");
        }

        let sem = Arc::new(Semaphore::new(self.max_download_concurrency));

        let sgs = media.segments();
        let pb = ProgressBar::new(sgs.len() as u64);
        pb.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} [{wide_bar:.cyan/blue}] {pos}/{len} ({eta})",
            )
            .unwrap()
            .progress_chars("#>-"),
        );

        let mut set = JoinSet::new();

        for segment in sgs.into_iter() {
            set.spawn({
                let self2 = self.clone();
                let sem = sem.clone();
                let pb = pb.clone();
                async move {
                    let permit = sem.acquire().await;
                    if permit.is_err() {
                        return Ok(());
                    }
                    match self2.download_m3u8_part(&segment).await {
                        Ok(_) => {
                            pb.inc(1);
                            Ok(())
                        }
                        Err(e) => {
                            return Err(e);
                        }
                    }
                }
            });
        }

        while let Some(res) = set.join_next().await {
            let out = res?;
            if let Err(e) = out {
                pb.abandon();
                sem.close();
                return Err(e);
            }
        }

        media.write_to_file(&self.save_dir.join(&self.index_name))?;
        pb.finish_with_message("downloaded");
        tracing::info!("segments downloaded");

        Ok(())
    }
}

#[derive(Debug)]
pub struct M3U8MediaPlaylist {
    base_url: Option<url::Url>,

    media: MediaPlaylist,

    content_sum: String,
}

impl M3U8MediaPlaylist {
    pub fn new(media: MediaPlaylist, sum: String) -> Self {
        Self {
            base_url: None,
            media: media,
            content_sum: sum,
        }
    }

    pub fn content_sum(&self) -> &str {
        &self.content_sum
    }

    pub fn set_base_url(&mut self, base_url: url::Url) {
        self.base_url = Some(base_url);
    }

    pub fn segments(&self) -> Vec<String> {
        self.media
            .segments
            .iter()
            .map(|s| self.format_url(&s.uri))
            .collect()
    }

    fn format_url(&self, uri: &str) -> String {
        if uri.starts_with("http") {
            return uri.to_string();
        }
        if let Some(ref base_url) = self.base_url {
            return base_url.join(uri).unwrap().to_string();
        }
        uri.to_string()
    }

    pub fn key(&self) -> Option<String> {
        if let Some(sg) = self.media.segments.first() {
            if let Some(ref key) = sg.key {
                if let Some(ref uri) = key.uri {
                    return Some(self.format_url(uri));
                }
            }
        }
        None
    }

    /// 需要对文件中的路径做处理
    pub fn write_to_file<P>(mut self, path: P) -> anyhow::Result<()>
    where
        P: AsRef<std::path::Path>,
    {
        let mut file = std::fs::File::create(&path)?;

        self.media.segments.iter_mut().for_each(|item| {
            if let Some(ref mut key) = item.key {
                if let Some(ref mut uri) = key.uri {
                    *uri = basename(uri).to_string();
                }
            }
            item.uri = basename(&item.uri).to_string();
        });

        self.media.write_to(&mut file)?;

        file.sync_all()?;
        Ok(())
    }
}
