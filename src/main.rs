use rascam::*;
use tracing::{error as t_error, info as t_info};
use tracing_subscriber;

use std::{thread, time};

use chrono::offset::Local;
use chrono::DateTime;
use clap::Parser;
use futures::future::FutureExt as _;
use futures::stream::StreamExt as _;
use native_dialog::FileDialog;
use std::path::Path;
use std::time::SystemTime;
use tokio::fs::File;
use tokio::io::AsyncWriteExt as _;

// static paramters for remi system
const WIDTH: u32 = 1024;
const HEIGHT: u32 = 768;
const ISO: ISO = 100;
const SENSOR_MODE: u32 = 1;
const JPEG_QUALITY: u32 = 85;
const _EXPOSTURE: u32 = 40000;
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
        encoding: MMAL_ENCODING_JPEG,
        width: WIDTH, // 96px will not require padding
        height: HEIGHT,
        iso: ISO,
        sensor_mode: SENSOR_MODE,
        quality: args.quality,
        zero_copy: true,
        use_encoder: true,
    };

    info.cameras.iter().for_each(|cam| t_info!("{}", cam));
    let mut camera = match init_camera(&info.cameras[0], &settings).await {
        Ok(camera) => camera,
        Err(e) => {
            t_error!("Fail to init camera");
            return Err(e);
        }
    };

    let datetime: DateTime<Local> = SystemTime::now().into();
    outputdir.push_str(&format!("/{}", datetime.format("%Y%m%d_%H%M%S")));
    if !Path::new(&outputdir).exists() {
        std::fs::create_dir_all(&outputdir)?;
    }

    let result = batch_capture(&mut camera, args.nframe, interval, outputdir).await;
    match result {
        Ok(_) => t_info!("Finished the capture"),
        Err(err) => {
            t_error!("error: {}", err);
            std::process::exit(1);
        }
    };
    Ok(())
}

async fn init_camera(
    info: &CameraInfo,
    settings: &CameraSettings,
) -> Result<SeriousCamera, Box<dyn std::error::Error>> {
    let mut camera = SeriousCamera::new()?;
    camera.set_camera_num(0)?;

    camera.enable_control_port(true)?;

    camera.set_camera_params(info)?;

    // critical the encoder must be created before camera formating.
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
    capture(&mut camera).await?;
    camera.set_shutter_speed(SHUTTER_SPEED)?;
    camera.set_awb_mode(AWBMode::OFF)?;
    // r_gain and g_gain were taken from live image of Remi system using picamera
    camera.set_awb_gain(0.28515625, 3.234375)?;
    thread::sleep(sleep_duration);
    capture(&mut camera).await?;

    Ok(camera)
}

async fn capture(camera: &mut SeriousCamera) -> Result<Vec<u8>, CameraError> {
    let receiver = camera.take_async()?;
    let future = receiver
        .fold(Vec::new(), |mut acc, buf| async move {
            acc.extend(buf.get_bytes());
            acc
        })
        .map(Ok);
    future.await
}

async fn batch_capture<P: AsRef<Path> + Clone>(
    camera: &mut SeriousCamera,
    n: usize,
    interval: u64,
    outputdir: P,
) -> Result<(), Box<dyn std::error::Error>> {
    // let mut to_gray = turbojpeg::Transform::default();
    // to_gray.gray = true;

    t_info!("Capture start");
    let mut ticker = tokio::time::interval(time::Duration::from_millis(interval));
    for i in 0..n {
        ticker.tick().await;
        let im = capture(camera).await?;
        let datetime: DateTime<Local> = SystemTime::now().into();
        let filename = format!("{}.jpg", datetime.format("%Y%m%d_%H%M%S_%3f"));
        t_info!("{} ({}/{})", filename, i + 1, n);
        let outputdir: &Path = outputdir.as_ref();

        // let gray = turbojpeg::transform(&to_gray, &im)?;

        let mut file = File::create(&outputdir.join(&filename)).await?;
        file.write_all(&im).await?;
    }
    Ok(())
}
