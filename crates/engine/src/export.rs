//! Export: options, encoding and watermarks live in [`dac_engine_export::export`]; preparing and
//! running a batch against the session lives here.

use dac_meta::Metadata;

pub use dac_engine_export::export::*;

/// Render photo `id` at the requested size and encode it (or copy / convert its original for
/// [`ExportFormat::Original`] / [`ExportFormat::Dng`]).
pub fn export_photo(session: &mut crate::Session, id: dac_catalog::PhotoId, o: &ExportOptions, seq: usize) -> Result<Exported, String> {
    prepare_export(session, id, o, seq)?.run()
}

/// One photo's export, set up from the session ([`prepare_export`]). [`PreparedExport::run`] does
/// the heavy part (read, decode, render, encode) without the session, so it can run on another
/// thread.
pub struct PreparedExport {
    pub photo: dac_catalog::PhotoId,
    pub file_name: String,
    work: Work,
    /// The library's originals, which [`run_batch`] never writes over (shared by a batch).
    guard: std::sync::Arc<crate::originals::OriginalGuard>,
}

struct RenderWork {
    job: crate::media::RenderJob,
    meta: Option<Metadata>,
    opts: ExportOptions,
}

enum Work {
    Render(Box<RenderWork>),
    /// [`ExportFormat::Original`] (the file's bytes + an XMP sidecar) or [`ExportFormat::Dng`] (the
    /// raw data re-encoded as a lossless DNG with the edits in its XMP).
    File {
        path: String,
        read: Option<crate::merge::ByteReader>,
        packet: String,
        dng: Option<DngCompression>,
        label: String,
        size: (usize, usize),
    },
}

/// Set up the export of photo `id` at 1-based position `seq` of a batch. (For a whole batch use
/// [`prepare_batch`]: it looks at the library's originals once.)
pub fn prepare_export(session: &mut crate::Session, id: dac_catalog::PhotoId, o: &ExportOptions, seq: usize) -> Result<PreparedExport, String> {
    let guard = std::sync::Arc::new(session.original_guard());
    prepare_guarded(session, id, o, seq, guard)
}

/// [`prepare_export`] for each of `ids` in order (`{seq}` = position, from 1).
pub fn prepare_batch(session: &mut crate::Session, ids: &[dac_catalog::PhotoId], o: &ExportOptions) -> Result<Vec<PreparedExport>, String> {
    let guard = std::sync::Arc::new(session.original_guard());
    ids.iter().enumerate().map(|(i, id)| prepare_guarded(session, *id, o, i + 1, guard.clone())).collect()
}

fn prepare_guarded(
    session: &mut crate::Session,
    id: dac_catalog::PhotoId,
    o: &ExportOptions,
    seq: usize,
    guard: std::sync::Arc<crate::originals::OriginalGuard>,
) -> Result<PreparedExport, String> {
    let p = session.catalog.photo(id).ok_or("no such photo")?;
    let file_name = o.file_name_for(p, seq);
    let work = if o.format.is_rendered() {
        let (w, h) = output_size(p, o);
        let meta = export_metadata(p, o);
        let job = session.export_job(id, w, h, o.effective_space(), o.effective_depth())?;
        Work::Render(Box::new(RenderWork { job, meta, opts: o.clone() }))
    } else {
        let dac_catalog::Source::File { path } = &p.source else {
            return Err(format!("{} is a generated demo photo: it has no original file to export", p.file_name));
        };
        Work::File {
            path: path.clone(),
            read: session.media.file_bytes.clone(),
            packet: crate::sidecar::sidecar_packet(p, &session.catalog),
            dng: (o.format == ExportFormat::Dng).then_some(o.dng_compression),
            label: p.file_name.clone(),
            size: (p.width as usize, p.height as usize),
        }
    };
    Ok(PreparedExport { photo: id, file_name, work, guard })
}

impl PreparedExport {
    pub fn run(self) -> Result<Exported, String> {
        let file_name = self.file_name;
        match self.work {
            Work::Render(w) => {
                let RenderWork { job, meta, opts } = *w;
                let r = job.run().rendered?;
                let bytes = encode_rendered(&r, &opts, meta.as_ref())?;
                Ok(Exported { file_name, bytes, width: r.image.width, height: r.image.height, sidecars: Vec::new() })
            }
            Work::File { path, read, packet, dng, label, size } => {
                let bytes = match &read {
                    Some(r) => r(&path)?,
                    None => std::fs::read(&path).map_err(|e| format!("{path}: {e}"))?,
                };
                let Some(dng) = dng else {
                    return Ok(Exported { file_name, bytes, width: size.0, height: size.1, sidecars: vec![("xmp", packet.into_bytes())] });
                };
                if dac_raw::probe(&bytes).is_none() {
                    return Err(format!("{label}: DNG export needs a raw photo"));
                }
                let raw = dac_raw::decode(&bytes).map_err(|e| format!("{label}: {e}"))?;
                drop(bytes);
                let compression = match dng {
                    DngCompression::Lossless => dac_raw::DngCompression::Lj92 { tile: 256 },
                    DngCompression::Deflate => dac_raw::DngCompression::Deflate { tile: 256, half: false },
                    DngCompression::Uncompressed => dac_raw::DngCompression::Uncompressed,
                };
                let dng = dac_raw::write_dng(&raw, &dac_raw::DngWriteOptions { xmp: Some(packet), compression, ..Default::default() })
                    .map_err(|e| e.to_string())?;
                Ok(Exported { file_name, bytes: dng, width: raw.width, height: raw.height, sidecars: Vec::new() })
            }
        }
    }
}

/// Export `ids` in order ([`prepare_batch`] + [`run_batch`]), stopping at the first error.
pub fn export_batch(
    session: &mut crate::Session,
    ids: &[dac_catalog::PhotoId],
    o: &ExportOptions,
    to: &Destination,
    write: &mut dyn FnMut(&str, &[u8]) -> Result<(), String>,
    exists: &dyn Fn(&str) -> bool,
) -> Result<Vec<serde_json::Value>, String> {
    let items = prepare_batch(session, ids, o)?;
    run_batch(items, o, to, write, exists, true, &mut |_, _| true)
}

/// Run prepared exports in order: pick each one's path, and hand the bytes (and sidecars) to
/// `write`. `exists` tells whether a path is taken. The conflict policy applies to a file and its
/// sidecars as one: with Unique both get the same free name, with Skip the photo is skipped when
/// either is taken. A path that is a catalogued original (or its sidecar) is refused whatever the
/// policy. `progress(done, next file)` is called before each photo; returning false cancels the
/// rest. Returns one JSON object per photo: `{path, width, height, bytes, sidecars}`,
/// `{skipped: path}` or (unless `stop_on_error`) `{photo, file, error}`.
pub fn run_batch(
    items: Vec<PreparedExport>,
    o: &ExportOptions,
    to: &Destination,
    write: &mut dyn FnMut(&str, &[u8]) -> Result<(), String>,
    exists: &dyn Fn(&str) -> bool,
    stop_on_error: bool,
    progress: &mut dyn FnMut(usize, &str) -> bool,
) -> Result<Vec<serde_json::Value>, String> {
    use serde_json::json;
    let join = |a: &str, b: &str| if a.is_empty() { b.to_string() } else { format!("{}/{b}", a.trim_end_matches('/')) };
    let dir = if o.subfolder.is_empty() { to.dir.clone() } else { join(&to.dir, &o.subfolder) };
    let single = items.len() == 1;
    let mut taken = std::collections::HashSet::new();
    let mut out = Vec::new();
    for (i, item) in items.into_iter().enumerate() {
        if !progress(i, &item.file_name) {
            break;
        }
        let (photo, name, guard) = (item.photo, item.file_name.clone(), item.guard.clone());
        let e = match item.run() {
            Ok(e) => e,
            Err(err) if stop_on_error => return Err(err),
            Err(err) => {
                out.push(json!({"photo": photo.0, "file": name, "error": err}));
                continue;
            }
        };
        // the exported file and its sidecars
        let group = |main: &str| std::iter::once(main.to_string()).chain(e.sidecars.iter().map(|(x, _)| sidecar_path(main, x))).collect::<Vec<_>>();
        let path = match to.exact.as_deref().filter(|_| single) {
            Some(p) => Ok(p.to_string()),
            None => {
                let path = join(&dir, &e.file_name);
                let busy = |main: &str| group(main).iter().any(|p| taken.contains(p) || exists(p));
                if !busy(&path) {
                    Ok(path)
                } else {
                    match o.conflict {
                        Conflict::Overwrite => Ok(path),
                        Conflict::Skip => {
                            out.push(json!({"skipped": path}));
                            continue;
                        }
                        Conflict::Unique => {
                            let (stem, ext) = e.file_name.rsplit_once('.').map_or((e.file_name.as_str(), None), |(a, b)| (a, Some(b)));
                            let name = |n: usize| join(&dir, &ext.map_or(format!("{stem}-{n}"), |x| format!("{stem}-{n}.{x}")));
                            (2..1_000_000).map(name).find(|p| !busy(p)).ok_or_else(|| format!("{path}: no free file name"))
                        }
                    }
                }
            }
        };
        let file = path.clone().unwrap_or_else(|_| name.clone());
        let written = path.and_then(|path| {
            let files = group(&path);
            // never over an original, whatever the conflict policy or the exact path said
            for f in &files {
                guard.check(std::path::Path::new(f))?;
            }
            write(&path, &e.bytes)?;
            let mut sidecars = Vec::new();
            for ((_, bytes), sc) in e.sidecars.iter().zip(files.iter().skip(1)) {
                write(sc, bytes)?;
                sidecars.push(sc.clone());
            }
            Ok((path, files, sidecars))
        });
        match written {
            Ok((path, files, sidecars)) => {
                taken.extend(files);
                out.push(json!({"path": path, "width": e.width, "height": e.height, "bytes": e.bytes.len(), "sidecars": sidecars}));
            }
            Err(err) if stop_on_error => return Err(err),
            Err(err) => out.push(json!({"photo": photo.0, "file": file, "error": err})),
        }
    }
    Ok(out)
}
