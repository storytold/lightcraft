//! Private SAM 3 transport. Bind the worker to loopback and forward it over SSH.
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::time::Duration;

use crate::{Click, Encoded, INPUT, MASK_SIDE, Sam3};
use serde::{Deserialize, Serialize};

const CONTROL_LIMIT: usize = 16 * 1024;
const RESPONSE_LIMIT: usize = 4 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(9 * 60);
/// Environment variable (name after the app prefix, see `dac_brand::env`) with the remote worker address.
pub const ENV: &str = "SAM3_REMOTE";

#[derive(Serialize, Deserialize)]
pub enum Operation {
    Prepare,
    Clicks(Vec<[f32; 3]>),
    Text(String),
}

#[derive(Serialize, Deserialize)]
pub struct Request {
    pub width: usize,
    pub height: usize,
    pub temporary: bool,
    pub operation: Operation,
}

impl Request {
    pub fn validate(&self) -> Result<(), String> {
        if self.width > INPUT || self.height > INPUT || (self.width == 0) != (self.height == 0) {
            return Err("invalid SAM image dimensions".into());
        }
        if self.temporary && self.width == 0 {
            return Err("detail pass requires an image".into());
        }
        match &self.operation {
            Operation::Prepare => {}
            Operation::Text(text) if text.len() <= 4096 && text.split([',', ';']).any(|p| !p.trim().is_empty()) => {}
            Operation::Clicks(clicks)
                if !clicks.is_empty()
                    && clicks.len() <= 64
                    && clicks.iter().all(|c| {
                        c.iter().all(|v| v.is_finite()) && (0.0..=1.0).contains(&c[0]) && (0.0..=1.0).contains(&c[1]) && (c[2] == 0.0 || c[2] == 1.0)
                    })
                    && clicks.iter().any(|c| c[2] == 1.0) => {}
            _ => return Err("invalid SAM prompt".into()),
        }
        Ok(())
    }
}

pub fn configured() -> Option<String> {
    dac_brand::env(ENV).filter(|v| !v.trim().is_empty())
}

pub fn read_frame(reader: &mut impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut size = [0; 4];
    reader.read_exact(&mut size)?;
    let size = u32::from_be_bytes(size) as usize;
    if size > limit {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "SAM frame exceeds limit"));
    }
    let mut data = vec![0; size];
    reader.read_exact(&mut data)?;
    Ok(data)
}

fn write_frame(writer: &mut impl Write, data: &[u8]) -> io::Result<()> {
    let size = u32::try_from(data.len()).map_err(io::Error::other)?;
    writer.write_all(&size.to_be_bytes())?;
    writer.write_all(data)
}

pub struct Client {
    stream: TcpStream,
    pub key: Option<u64>,
}

impl Client {
    pub fn connect(endpoint: &str) -> Result<Self, String> {
        let address: SocketAddr = endpoint.parse().map_err(|e| format!("SAM endpoint must be an IP:port: {e}"))?;
        if !address.ip().is_loopback() {
            return Err("SAM endpoint must be loopback; use an SSH tunnel for remote access".into());
        }
        let stream = TcpStream::connect_timeout(&address, Duration::from_secs(5)).map_err(|e| e.to_string())?;
        stream.set_read_timeout(Some(TIMEOUT)).map_err(|e| e.to_string())?;
        stream.set_write_timeout(Some(Duration::from_secs(30))).map_err(|e| e.to_string())?;
        Ok(Self { stream, key: None })
    }

    pub fn request(&mut self, request: &Request, rgb: &[u8]) -> Result<Option<Vec<f32>>, String> {
        request.validate()?;
        if rgb.len() != request.width * request.height * 3 {
            return Err("SAM RGB buffer size does not match dimensions".into());
        }
        let bytes = serde_json::to_vec(request).map_err(|e| e.to_string())?;
        write_frame(&mut self.stream, &bytes).map_err(|e| e.to_string())?;
        write_frame(&mut self.stream, rgb).map_err(|e| e.to_string())?;
        let response = read_frame(&mut self.stream, RESPONSE_LIMIT).map_err(|e| e.to_string())?;
        let result: Result<Option<Vec<f32>>, String> = serde_json::from_slice(&response).map_err(|e| e.to_string())?;
        let logits = result?;
        if logits.as_ref().is_some_and(|v| v.len() != MASK_SIDE * MASK_SIDE || v.iter().any(|n| !n.is_finite())) {
            return Err("invalid SAM mask response".into());
        }
        Ok(logits)
    }
}

pub fn text_logits(model: &mut Sam3, enc: &mut Encoded, text: &str) -> Result<Option<Vec<f32>>, String> {
    let mut merged: Option<Vec<f32>> = None;
    for phrase in text.split([',', ';']).map(str::trim).filter(|p| !p.is_empty()) {
        if let Some(p) = model.segment_text(enc, phrase, 0.5).map_err(|e| e.to_string())? {
            if let Some(m) = &mut merged {
                for (a, b) in m.iter_mut().zip(p.logits) {
                    *a = a.max(b);
                }
            } else {
                merged = Some(p.logits);
            }
        }
    }
    Ok(merged)
}

fn predict(model: &mut Sam3, enc: &mut Encoded, operation: Operation) -> Result<Option<Vec<f32>>, String> {
    match operation {
        Operation::Prepare => {
            model.prepare_clicks(enc).map_err(|e| e.to_string())?;
            Ok(None)
        }
        Operation::Text(text) => text_logits(model, enc, &text),
        Operation::Clicks(points) => {
            let clicks: Vec<_> = points.into_iter().map(|p| Click { x: p[0], y: p[1], positive: p[2] == 1.0 }).collect();
            Ok(Some(model.segment_clicks(enc, &clicks).map_err(|e| e.to_string())?.logits))
        }
    }
}

fn serve_client(stream: &mut TcpStream, dir: &Path, device: candle_core::Device) -> Result<(), String> {
    stream.set_read_timeout(Some(Duration::from_secs(10 * 60))).map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(Duration::from_secs(30))).map_err(|e| e.to_string())?;
    let mut model = None;
    let mut cache = None;
    loop {
        let frame = read_frame(stream, CONTROL_LIMIT).map_err(|e| e.to_string())?;
        let request: Request = serde_json::from_slice(&frame).map_err(|e| e.to_string())?;
        request.validate()?;
        let rgb = read_frame(stream, request.width * request.height * 3).map_err(|e| e.to_string())?;
        if rgb.len() != request.width * request.height * 3 {
            return Err("invalid RGB payload".into());
        }
        let started = std::time::Instant::now();
        let result = (|| {
            if model.is_none() {
                model = Some(Sam3::load_on(dir, device.clone()).map_err(|e| e.to_string())?);
            }
            let m = model.as_mut().ok_or("SAM model unavailable")?;
            let mut temporary = None;
            if request.width > 0 {
                let encoded = m.encode(&rgb, request.width, request.height).map_err(|e| e.to_string())?;
                if request.temporary {
                    temporary = Some(encoded);
                } else {
                    cache = Some(encoded);
                }
            }
            let enc = if request.temporary { temporary.as_mut() } else { cache.as_mut() }.ok_or("SAM image cache empty")?;
            predict(m, enc, request.operation)
        })();
        eprintln!(
            "SAM3 Metal request elapsed={:.3}s image={}x{} detail={} success={}",
            started.elapsed().as_secs_f64(),
            request.width,
            request.height,
            request.temporary,
            result.is_ok()
        );
        let response = serde_json::to_vec(&result).map_err(|e| e.to_string())?;
        write_frame(stream, &response).map_err(|e| e.to_string())?;
        if result.is_err() {
            return Err("SAM inference failed; session released".into());
        }
    }
}

pub fn serve(address: SocketAddr, dir: &Path, device: candle_core::Device) -> Result<(), String> {
    if !address.ip().is_loopback() || !device.is_metal() {
        return Err("SAM worker requires a loopback address and a Metal device".into());
    }
    if !crate::is_model_dir(dir) {
        return Err("SAM model files are missing".into());
    }
    let listener = TcpListener::bind(address).map_err(|e| e.to_string())?;
    eprintln!("SAM3 worker listening on {address}; device=Metal; idle unload=600s");
    for stream in listener.incoming() {
        let mut stream = stream.map_err(|e| e.to_string())?;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| serve_client(&mut stream, dir, device.clone())));
        match result {
            Ok(Err(error)) => eprintln!("SAM3 session closed: {error}"),
            Err(_) => eprintln!("SAM3 session panicked; model released"),
            Ok(Ok(())) => {}
        }
    }
    Ok(())
}
