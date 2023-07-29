use image::io::Reader as ImageReader;
use image::ImageFormat;
use rascam::*;
use std::io::Cursor;
use tracing::error as t_error;
use tracing::info as t_info;

use chrono::offset::Local;
use chrono::DateTime;
use clap::Parser;
use futures::future::FutureExt as _;
use futures::stream::StreamExt as _;
use native_dialog::FileDialog;
use std::path::Path;
use std::thread;
use std::time::{self, SystemTime};
use tokio::fs::File;
use tokio::io::AsyncWriteExt as _;

// static paramters for remi system
const WIDTH: u32 = 1024;
const HEIGHT: u32 = 768;
const ISO: ISO = 100;
const SENSOR_MODE: u32 = 1;
const JPEG_QUALITY: u32 = 85;
const SHUTTER_SPEED: u32 = 40000;
const DEFAULT_OUTPUT_DIR: &'static str = "/media/pi/rpi";

/// A simple capture CLI for rapid elegans motion detection (Remi) system
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Number of picture to capture
    #[arg(short, long)]
    nframe: usize,

    /// interval of between each frame (sec) (default= 2.0)
    #[arg(short, long, default_value_t = 2.0)]
    interval: f64,
    // output directory (default = ""). A filedialog will pop up if outputdir was not provided.
    #[arg(short, long, default_value = "")]
    outputdir: String,
    #[arg(short,long, default_value_t = JPEG_QUALITY)]
    quality: u32,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let interval = (args.interval * 1000f64).round() as u64;

    let mut outputdir = args.outputdir.to_string();

    if !Path::new(&outputdir).exists() {
        let path = FileDialog::new()
            .set_location(DEFAULT_OUTPUT_DIR)
            .show_open_single_dir()
            .ok();
        match path {
            Some(Some(p)) if p.to_str().is_some() => {
                outputdir = p.to_str().unwrap().to_owned();
            }
            _ => {
                t_error!("error: Output directory was not selected",);
                std::process::exit(1);
            }
        };
    }

    let info = info()?;
    if info.cameras.len() < 1 {
        t_error!("Found 0 camera. Exiting");
        // note that this doesn't run destructors
        std::process::exit(1);
    }

    t_info!("Found {} cameras.", info.cameras.len());

    let settings = CameraSettings {
        encoding: MMAL_ENCODING_PNG,
        width: WIDTH, // 96px will not require padding
        height: HEIGHT,
        iso: ISO,
        sensor_mode: SENSOR_MODE,
        quality: args.quality,
        zero_copy: true,
        use_encoder: true,
    };

    info.cameras.iter().for_each(|cam| t_info!("{}", cam));
    let mut camera = init_camera(&info.cameras[0], &settings).await?;

    let datetime: DateTime<Local> = SystemTime::now().into();
    outputdir.push_str(&format!("/{}", datetime.format("%Y%m%d_%H%M%S")));
    if !Path::new(&outputdir).exists() {
        std::fs::create_dir_all(&outputdir)?;
    }

    batch_capture(&mut camera, &settings, args.nframe, interval, &outputdir).await
}

async fn init_camera(
    info: &CameraInfo,
    settings: &CameraSettings,
) -> Result<SeriousCamera, Box<dyn std::error::Error>> {
    let mut camera = SeriousCamera::new()?;
    camera.set_camera_num(0)?;

    camera.enable_control_port(true)?;

    camera.set_camera_params(info)?;

    // [Critical] the encoder must be created before camera formating.
    camera.create_encoder()?;

    camera.set_camera_format(settings)?;
    camera.enable()?;

    camera.create_preview()?;
    camera.create_pool()?;

    camera.enable_encoder()?;
    camera.enable_preview()?;

    camera.connect_encoder()?;
    camera.connect_preview()?;

    // warm up the camera
    let sleep_duration = time::Duration::from_millis(2000);
    thread::sleep(sleep_duration);

    // warm up
    async_capture(&mut camera).await?;
    camera.set_shutter_speed(SHUTTER_SPEED)?;
    camera.set_awb_mode(AWBMode::OFF)?;
    // r_gain and g_gain were taken from live image of Remi system using picamera
    camera.set_awb_gain(0.28515625, 3.234375)?;
    thread::sleep(sleep_duration);
    async_capture(&mut camera).await?;

    Ok(camera)
}

async fn async_capture(camera: &mut SeriousCamera) -> Result<Vec<u8>, CameraError> {
    let receiver = camera.take_async()?;
    let future = receiver
        .fold(Vec::new(), |mut acc, buf| async move {
            acc.extend(buf.get_bytes());
            acc
        })
        .map(Ok);
    future.await
}

async fn batch_capture<P: AsRef<Path>>(
    camera: &mut SeriousCamera,
    settings: &CameraSettings,
    n: usize,
    interval: u64,
    // width: u32,
    // height: u32,
    outputdir: P,
) -> Result<(), Box<dyn std::error::Error>> {
    t_info!("Capture start");
    let ticker = tokio::time::interval(time::Duration::from_millis(interval));
    tokio::pin!(ticker);
    let outputdir: &Path = outputdir.as_ref();

    let format = if settings.encoding == MMAL_ENCODING_PNG {
        ImageFormat::Png
    } else {
        ImageFormat::Jpeg
    };

    let mut im: Vec<u8> = Vec::new();
    for i in 1..=n {
        ticker.tick().await;
        im.clear();
        let receiver = camera.take()?;
        loop {
            match receiver.recv()? {
                Some(buf) => {
                    im.write_all(buf.get_bytes()).await?;
                    if buf.is_complete() {
                        break;
                    }
                }
                None => break,
            };
        }

        let datetime: DateTime<Local> = SystemTime::now().into();

        match ImageReader::with_format(Cursor::new(&im), format).decode() {
            Ok(res) => {
                let gray = res.to_luma8();
                let filename = format!("{}.jpg", datetime.format("%Y%m%d_%H%M%S_%3f"));

                im.clear();
                let mut buf = Cursor::new(&mut im);
                gray.write_to(&mut buf, ImageFormat::Jpeg)?;
                // gray.save_with_format(&outputdir.join(&filename), ImageFormat::Jpeg)?;
                let mut file = File::create(&outputdir.join(&filename)).await?;
                file.write_all(&im).await?;
                t_info!("{} ({}/{})", filename, i, n);
            }
            Err(e) => {
                let filename = if settings.encoding == MMAL_ENCODING_PNG {
                    format!("{}.png", datetime.format("%Y%m%d_%H%M%S_%3f"))
                } else {
                    format!("{}.jpg", datetime.format("%Y%m%d_%H%M%S_%3f"))
                };
                let mut file = File::create(&outputdir.join(&filename)).await?;
                file.write_all(&im).await?;
                t_error!("{}", e);
                t_info!("{} ({}/{})", filename, i, n);
            }
        };
    }
    Ok(())
}
