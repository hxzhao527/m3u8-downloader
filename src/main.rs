use clap::Parser;

use m3u8_download::DownloaderBuilder;

#[derive(Debug, Parser)]
pub struct Args {
    url: String,

    /// http headers, example: -H "User-Agent: curl/7.54.0"
    #[arg(short = 'H', long="header", value_parser = parse_key_val::<String, String>)]
    headers: Vec<(String, String)>,

    /// output dir
    #[arg(short = 'D', long = "dir")]
    dir: String,

    /// merge output
    #[arg(short = 'm', long = "merge")]
    merge_output: Option<String>,

    /// play m3u8
    #[arg(short = 'p', long = "play", default_value_t = false)]
    play: bool,

    /// verbose
    #[arg(short = 'v', long = "verbose", default_value_t = false)]
    verbose: bool,
}

fn parse_key_val<T, U>(s: &str) -> anyhow::Result<(T, U)>
where
    T: std::str::FromStr,
    <T as std::str::FromStr>::Err: Into<anyhow::Error>,
    U: std::str::FromStr,
    <U as std::str::FromStr>::Err: Into<anyhow::Error>,
{
    let pos = s
        .find(": ")
        .ok_or_else(|| anyhow::anyhow!("invalid KEY=value: no `=` found in `{s}`"))?;
    Ok((
        s[..pos].parse::<T>().map_err(|e| e.into())?,
        s[pos + 2..].parse::<U>().map_err(|e| e.into())?,
    ))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let downloader: DownloaderBuilder = DownloaderBuilder::new(args.url).save_dir(&args.dir);
    let index_path = std::path::Path::new(&args.dir).join(DownloaderBuilder::INDEX_FILE_NAME);

    let downloader = args
        .headers
        .iter()
        .fold(downloader, |downloader, (k, v)| downloader.header(k, v));

    downloader.download().await?;

    let util = m3u8_download::VideoUtil::from_index(index_path)?;

    if args.play {
        util.play()?;
    }

    if let Some(merge_output) = args.merge_output {
        util.merge_to(&merge_output)?;
        util.clean_segment()?;
    }

    Ok(())
}
