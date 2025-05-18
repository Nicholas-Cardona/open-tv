use crate::settings::get_default_record_path;
use crate::sql;
use crate::types::ChannelHttpHeaders;
use crate::utils::{find_macos_bin, get_bin};
use crate::{media_type, settings::get_settings, types::Channel};
use anyhow::{Context, Result, bail};
use chrono::Local;
use std::sync::LazyLock;
use std::{env::consts::OS, path::Path, process::Stdio};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};

const ARG_SAVE_POSITION_ON_QUIT: &str = "--save-position-on-quit";
const ARG_CACHE: &str = "--cache=";
const ARG_NO: &str = "no";
const ARG_RECORD: &str = "--stream-record=";
const ARG_TITLE: &str = "--title=";
// const ARG_MSG_LEVEL: &str = "--msg-level=all=info";
const ARG_MSG_LEVEL: &str = "--quiet";
const ARG_YTDLP_PATH: &str = "--script-opts=ytdl_hook-ytdl_path=";
const ARG_VOLUME: &str = "--volume=";
const ARG_HTTP_HEADERS: &str = "--http-header-fields=";
const ARG_USER_AGENT: &str = "--user-agent=";
const ARG_IGNORE_SSL: &str = "--ytdl-raw-options=no-check-certificates=True";
const ARG_PREFETCH_PLAYLIST: &str = "--prefetch-playlist=yes";
const ARG_LOOP_PLAYLIST: &str = "--loop-playlist=inf";
const ARG_HWDEC: &str = "--hwdec=auto";
const ARG_GPU_NEXT: &str = "--vo=gpu-next";
const ARG_GPU_PROFILE_HIGH_QUALITY: &str = "--profile=high-quality";
const MPV_BIN_NAME: &str = "mpv";
const YTDLP_BIN_NAME: &str = "yt-dlp";
const HTTP_ORIGIN: &str = "origin:";
const HTTP_REFERRER: &str = "referer:";
static MPV_PATH: LazyLock<String> = LazyLock::new(|| get_bin(MPV_BIN_NAME));
static YTDLP_PATH: LazyLock<String> = LazyLock::new(|| find_macos_bin(YTDLP_BIN_NAME));

pub async fn play(channel: Channel, record: bool, record_path: Option<String>) -> Result<()> {
    println!("{} playing", channel.url.as_ref().unwrap());
    let args = get_play_args(channel.clone(), record, record_path)?;
    println!("with args: {:?}", args);
    let mut cmd = Command::new(MPV_PATH.clone())
        .args(args)
        .stdout(Stdio::piped())
        .spawn()?;

    let status = cmd.wait().await?;

    if let Some(stdout) = cmd.stdout.take() {
        let mut lines = BufReader::new(stdout).lines();

        let exited_at = 0;
        let duration = 0;
        while let Some(line) = lines.next_line().await? {
            if line.contains("Exiting at:") {
                if let Some((_, time)) = line.split_once("Exiting at: ") {
                    update_percentage_watched(channel.clone(), 10);
                }
            }
        }
    }
    if !status.success() {
        let stdout = cmd.stdout.take();
        if let Some(stdout) = stdout {
            let mut error: String = "".to_string();
            let mut lines = BufReader::new(stdout).lines();
            let mut first = true;
            while let Some(line) = lines.next_line().await? {
                error += &line;
                if !first {
                    error += "\n"
                } else {
                    first = false;
                }
            }
            if error != "" {
                bail!(error);
            } else {
                bail!("Mpv encountered an unknown error");
            }
        }
    }
    Ok(())
}

fn get_play_args(
    channel: Channel,
    record: bool,
    record_path: Option<String>,
) -> Result<Vec<String>> {
    let mut args = Vec::new();
    let settings = get_settings()?;
    let headers = sql::get_channel_headers_by_id(channel.id.context("no channel id?")?)?;
    args.push(channel.url.context("no url")?);
    args.push("--term-playing-msg=Exiting at: ${time-pos}s\nDuration: ${duration}".to_string());

    if channel.media_type != media_type::LIVESTREAM {
        args.push(ARG_SAVE_POSITION_ON_QUIT.to_string());
    }
    if settings.use_stream_caching == Some(false) {
        let stream_caching_arg = format!("{ARG_CACHE}{ARG_NO}",);
        args.push(stream_caching_arg);
    }
    if settings.enable_hwdec.unwrap_or(true) {
        args.push(ARG_HWDEC.to_string());
    }
    if settings.enable_gpu.unwrap_or(false) {
        args.push(ARG_GPU_NEXT.to_string());
        args.push(ARG_GPU_PROFILE_HIGH_QUALITY.to_string());
    }
    if record {
        let path = if let Some(p) = record_path {
            p
        } else if let Some(p) = settings.recording_path.map(get_path) {
            p
        } else {
            get_path(get_default_record_path()?)
        };
        args.push(format!("{ARG_RECORD}{path}"));
    }
    if OS == "macos" && *MPV_PATH != MPV_BIN_NAME {
        args.push(format!("{}{}", ARG_YTDLP_PATH, *YTDLP_PATH));
    }
    args.push(format!("{}{}", ARG_TITLE, channel.name));
    args.push(ARG_MSG_LEVEL.to_string());
    args.push(ARG_PREFETCH_PLAYLIST.to_string());
    args.push(ARG_LOOP_PLAYLIST.to_string());
    if let Some(volume) = settings.volume {
        args.push(format!("{ARG_VOLUME}{volume}"));
    }
    set_headers(headers, &mut args);
    if let Some(mpv_params) = settings.mpv_params {
        #[cfg(not(target_os = "windows"))]
        let mut params = shell_words::split(&mpv_params)?;
        #[cfg(target_os = "windows")]
        let mut params = winsplit::split(&mpv_params);
        args.append(&mut params);
    }
    Ok(args)
}

fn update_percentage_watched(channel: Channel, percentage: i32) -> Result<()> {
    if let Some(id) = channel.id {
        if (channel.media_type == media_type::MOVIE || channel.media_type == media_type::SERIE) {
            let result = sql::update_watch_percentage(id, percentage);
            result.unwrap()
        }
    }

    return Ok(());
}

fn set_headers(headers: Option<ChannelHttpHeaders>, args: &mut Vec<String>) {
    if headers.is_none() {
        return;
    }
    let headers = headers.unwrap();
    let mut headers_vec: Vec<String> = Vec::with_capacity(2);
    if let Some(origin) = headers.http_origin {
        headers_vec.push(format!("{HTTP_ORIGIN}{origin}"));
    }
    if let Some(referrer) = headers.referrer {
        headers_vec.push(format!("{HTTP_REFERRER}{referrer}"));
    }
    if let Some(user_agent) = headers.user_agent {
        args.push(format!("{ARG_USER_AGENT}{user_agent}"));
    }
    if let Some(ignore_ssl) = headers.ignore_ssl {
        if ignore_ssl == true {
            args.push(ARG_IGNORE_SSL.to_string());
        }
    }
    if headers_vec.len() > 0 {
        let headers = headers_vec.join(",");
        args.push(format!("{ARG_HTTP_HEADERS}{headers}"));
    }
}

fn get_path(path_str: String) -> String {
    let path = Path::new(&path_str);
    let path = path.join(get_file_name());
    return path.to_string_lossy().to_string();
}

fn get_file_name() -> String {
    let current_time = Local::now();
    let formatted_time = current_time.format("%Y-%m-%d-%H-%M-%S").to_string();
    format!("{formatted_time}.mp4")
}
