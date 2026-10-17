//! Video clip export (M14.8): decode the trimmed range of a clip, grade every frame, encode, mux.
//!
//! Plan option A (`plan/video-filmcraft.md` §6): a small loop over FilmCraft's media, codec and
//! container crates, with no NLE project model. The pieces:
//!
//! 1. **Demux + decode** through `lcc-video` (FilmCraft's MP4/MOV demuxer and H.264/HEVC decoders):
//!    `FrameReader::frame_at` decodes from the previous sync sample, so the first exported frame is
//!    exactly the frame displayed at `trim_in`, even mid-GOP. Not `filmcraft-codecs`: it pulls
//!    symphonia (MPL-2.0), which the licence policy rejects.
//! 2. **Grade**: the caller's closure runs once per frame on a [`GradeFrame`]
//!    (`lcc_raster::Rgb32f`, linear light; see its docs for the exact convention).
//! 3. **Encode**: RGB → Y'CbCr (BT.709 coefficients from `filmcraft-color`, limited range) as
//!    8-bit 4:2:0 for H.264 (`filmcraft-h264enc`) or 10-bit 4:2:2 for ProRes 422
//!    (`filmcraft-prores`).
//! 4. **Audio** ([`encode_audio_range`]): the first audio track `lcc_video::AudioReader` decodes
//!    (AAC, Opus, AC-3 / E-AC-3; MP4/MOV, Matroska, TS) for the trimmed range, folded to mono or
//!    stereo, encoded once per clip: AAC-LC for MP4 (encoder delay hidden with an edit list,
//!    resampled to 48 kHz when the rate is not an AAC rate), 16-bit PCM (`sowt`) for ProRes MOV.
//! 5. **Mux**: `filmcraft-isobmff`'s `Mp4Writer` (MP4 brand for H.264, QuickTime for ProRes).
//!
//! The file is written to `<out>.partial` and renamed on success; an error or a cancel removes
//! it, so a failed export never leaves a truncated movie behind.
//!
//! Sources: plan/video-filmcraft.md §6 option A (own design over FilmCraft's public crate APIs).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
mod audio;
#[cfg(not(target_arch = "wasm32"))]
pub use audio::{AudioCodecOut, AudioPacket, EncodedAudioTrack, encode_audio_range, encode_pcm_track};
#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::*;
#[cfg(not(target_arch = "wasm32"))]
mod sequence;
#[cfg(not(target_arch = "wasm32"))]
pub use sequence::SequenceWriter;
