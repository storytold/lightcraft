//! `photo.sourceReport`: how the app reads a photo's original before any edit (roadmap RQ-01): sensor data or
//! the camera's embedded preview, where the starting colour and tone come from, which lens correction applies by
//! itself, and what the file carries of its own look. Read-only: the file is decoded again and nothing is cached or
//! written. It follows the same steps as opening the photo (`files::load_bytes_now`), so a comparison can say which
//! path a result came from before scoring it.

use serde_json::{Value, json};

use crate::camera_preview::{StartTrace, ToneSource};

/// The report for a file's `bytes`, developed with the camera-profile choice `camera` (`profile.camera`).
pub fn report(bytes: &[u8], camera: &str) -> Result<Value, String> {
    if lcc_raw::probe(bytes).is_none() {
        let format = lcc_codecs::sniff(bytes).map(|f| format!("{f:?}"));
        return Ok(json!({"raw": false, "format": format, "decode": {"kind": "image"}}));
    }
    let mut raw = match lcc_raw::decode(bytes).map_err(|e| crate::files::preview_reason(bytes, e)) {
        Ok(r) => r,
        // shown from the camera's embedded JPEG until the variant is supported
        Err(Ok(why)) => return Ok(json!({"raw": true, "decode": {"kind": "embeddedPreview", "reason": why}})),
        Err(Err(e)) => return Err(e),
    };
    // the photo's camera profile (`.dcp` or calibrated look) replaces the file's colour model, as on opening
    let camera_profile = crate::camera_profiles::profile_for(&raw.color, &raw.metadata, camera);
    // (a profile without a tone curve of its own keeps the tone the photo has without it, fitted with its own colour)
    let own_colour = camera_profile.as_ref().filter(|d| crate::camera_preview::keeps_photo_tone(d)).map(|_| raw.color.clone());
    if let Some(d) = &camera_profile {
        d.apply(&mut raw.color);
    }
    let camera_profile = camera_profile.map(|d| {
        let calibrated = lcc_raw::dcp::calibrated_look_id(&d);
        json!({"name": d.name, "model": d.model, "source": if calibrated.is_some() { "calibrated" } else { "dcp" }, "look": calibrated})
    });
    let info = raw.info();
    let window = lcc_raw::sensor_window(bytes, [info.active_area.width, info.active_area.height]);
    let lens = crate::lens_profiles::resolve_raw(&info, window, || lcc_meta::extract(bytes), None);
    let lens_geometry = crate::camera_preview::lens_geometry(&raw.opcodes.list3);
    raw.opcodes.list3.retain(|op| !op.is_lens_correction());
    let scale = crate::files::WbScaleOf::new(raw.format, &raw.color, raw.wb_multipliers, &raw.metadata);
    let profile = raw.color.profile.clone();
    let mut kept_trace = StartTrace::default();
    let kept = own_colour.and_then(|own| {
        let embedded = lens.as_ref().map(|(l, _)| l);
        crate::camera_preview::tone_without_profile(
            &mut raw,
            own,
            bytes,
            lens_geometry.as_ref(),
            embedded,
            lcc_develop::CameraColour::Auto,
            &mut kept_trace,
        )
    });
    let mut trace = StartTrace::default();
    let (_, t, look) = crate::camera_preview::starting_colour_traced(
        &mut raw,
        bytes,
        lens_geometry.as_ref(),
        lens.as_ref().map(|(l, _)| l),
        lcc_develop::CameraColour::Auto,
        camera_profile.is_some(),
        &mut trace,
    );
    // a look's tone curve wins over the file's own `ProfileToneCurve` (`files::load_bytes_now`)
    // then the tone a profile without a curve of its own keeps (`keptTone`: where it came from)
    let tone = match trace.tone {
        ToneSource::None if look.is_none() && kept.is_some() => json!("withoutProfile"),
        ToneSource::None if profile.tone_curve.is_some() => json!("dngProfileCurve"),
        other => json!(other),
    };
    let kept_tone = kept.is_some().then(|| match kept_trace.tone {
        ToneSource::None => json!("dngProfileCurve"),
        other => json!(other),
    });
    let lens = match &lens {
        None => json!({"source": "none"}),
        Some((l, applied)) => {
            use crate::lens_profiles::Applied;
            let (source, profile) = match applied {
                Applied::Embedded => ("embedded", None),
                Applied::Chosen(name) => ("chosen", Some(name.clone())),
                Applied::Found(name) => ("found", Some(name.clone())),
            };
            let stages = l.stages.iter().flatten().count();
            json!({"source": source, "profile": profile, "warp": l.warp.is_some() || stages > 0, "warpStages": stages, "vignette": l.vignette.is_some()})
        }
    };
    Ok(json!({
        "raw": true,
        "format": format!("{:?}", raw.format),
        "make": raw.metadata.make,
        "model": raw.metadata.model,
        "decode": {"kind": "sensor"},
        "colour": trace.colour,
        "cameraProfile": camera_profile,
        "matrixIsFallback": t.matrix_is_fallback,
        "look": look.as_ref().map(|l| json!({"hueSat": l.hue_sat.is_some()})),
        "tone": tone,
        "defaultExposure": trace.default_exposure,
        "keptTone": kept_tone,
        "whiteBalance": if scale.relative { "relative" } else { "absolute" },
        "dngProfile": {
            "toneCurve": profile.tone_curve.is_some(),
            "lookTable": profile.look_table.is_some(),
            "gainTableMap": profile.gain_table_map.is_some(),
        },
        "dynamicRangeOptimizer": lcc_raw::embedded_preview_dynamic_range_optimized(bytes),
        "lens": lens,
    }))
}
