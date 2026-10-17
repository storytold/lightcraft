//! The HDR headroom of the Wayland outputs, read from the compositor's colour-management
//! protocol (`wp_color_manager_v1` → each output's image description → `target_luminance` max
//! over `luminances` reference white). Pure Rust (`wayland-client`), on a connection of its own
//! before the window opens, so it never touches the window's surface. KWin reports it for HDR
//! outputs and, with EDR allowed, for SDR panels whose backlight has room (brightness below
//! 100 %: eDP-1 at 70 % reports reference 200, target max 281 → 1.4×).

use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};
use wayland_protocols::wp::color_management::v1::client::{
    wp_color_management_output_v1::WpColorManagementOutputV1,
    wp_color_manager_v1::WpColorManagerV1,
    wp_image_description_info_v1::{self, WpImageDescriptionInfoV1},
    wp_image_description_v1::{self, WpImageDescriptionV1},
};

/// One output's luminances (cd/m²), as its image description reports them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutputLuminance {
    pub reference: u32,
    pub max: u32,
    pub target_max: Option<u32>,
}

/// Headroom over SDR white: the target maximum (what the output can show now), else the
/// encoding maximum, over reference white. `None` without a usable reference.
pub fn headroom_of(l: &OutputLuminance) -> Option<f32> {
    let max = l.target_max.unwrap_or(l.max);
    (l.reference > 0 && max > 0).then(|| max as f32 / l.reference as f32).filter(|h| h.is_finite())
}

/// The largest headroom among `outputs` (the window may go to any of them), when above 1.
pub fn best_headroom(outputs: &[OutputLuminance]) -> Option<f32> {
    outputs.iter().filter_map(headroom_of).filter(|h| *h > 1.0).fold(None, |a, h| Some(a.map_or(h, |a: f32| a.max(h))))
}

#[derive(Default)]
struct State {
    outputs: Vec<wl_output::WlOutput>,
    manager: Option<WpColorManagerV1>,
    /// per image description (by output index): its luminances
    found: Vec<OutputLuminance>,
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(state: &mut Self, reg: &wl_registry::WlRegistry, ev: wl_registry::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_registry::Event::Global { name, interface, version } = ev {
            match interface.as_str() {
                "wl_output" => state.outputs.push(reg.bind(name, version.min(4), qh, ())),
                "wp_color_manager_v1" => state.manager = Some(reg.bind(name, version.min(2), qh, ())),
                _ => {}
            }
        }
    }
}

delegate_noop!(State: ignore wl_output::WlOutput);
delegate_noop!(State: ignore WpColorManagerV1);
delegate_noop!(State: ignore WpColorManagementOutputV1);

impl Dispatch<WpImageDescriptionV1, usize> for State {
    fn event(_: &mut Self, desc: &WpImageDescriptionV1, ev: wp_image_description_v1::Event, idx: &usize, _: &Connection, qh: &QueueHandle<Self>) {
        if matches!(ev, wp_image_description_v1::Event::Ready { .. } | wp_image_description_v1::Event::Ready2 { .. }) {
            desc.get_information(qh, *idx);
        }
    }
}

impl Dispatch<WpImageDescriptionInfoV1, usize> for State {
    fn event(
        state: &mut Self,
        _: &WpImageDescriptionInfoV1,
        ev: wp_image_description_info_v1::Event,
        idx: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(l) = state.found.get_mut(*idx) else { return };
        match ev {
            wp_image_description_info_v1::Event::Luminances { max_lum, reference_lum, .. } => {
                l.max = max_lum;
                l.reference = reference_lum;
            }
            wp_image_description_info_v1::Event::TargetLuminance { max_lum, .. } => l.target_max = Some(max_lum),
            _ => {}
        }
    }
}

/// Every output's luminances. Errors: no Wayland session, or the compositor has no
/// colour management (version 2 or later, which allows reading an output's description).
pub fn read() -> Result<Vec<OutputLuminance>, String> {
    let conn = Connection::connect_to_env().map_err(|e| e.to_string())?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let _registry = conn.display().get_registry(&qh, ());
    let mut state = State::default();
    queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
    let manager = state.manager.clone().ok_or("the compositor has no colour management")?;
    if manager.version() < 2 {
        return Err("the compositor's colour management can't describe outputs".into());
    }
    state.found = vec![OutputLuminance::default(); state.outputs.len()];
    let outputs = state.outputs.clone();
    let mut held = Vec::new();
    for (i, o) in outputs.iter().enumerate() {
        let cm = manager.get_output(o, &qh, ());
        held.push((cm.get_image_description(&qh, i), cm));
    }
    // ready → get_information → info events: three round trips
    for _ in 0..3 {
        queue.roundtrip(&mut state).map_err(|e| e.to_string())?;
    }
    for (d, cm) in held {
        d.destroy();
        cm.destroy();
    }
    manager.destroy();
    let _ = conn.flush();
    Ok(state.found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headroom_is_target_peak_over_reference_white() {
        // KWin, eDP-1 at 70 % with EDR: reference 200, target max 281
        let edr = OutputLuminance { reference: 200, max: 200, target_max: Some(281) };
        assert!((headroom_of(&edr).unwrap() - 1.405).abs() < 1e-3);
        // no target: the encoding maximum
        assert_eq!(headroom_of(&OutputLuminance { reference: 203, max: 406, target_max: None }), Some(2.0));
        // nonsense: none
        assert_eq!(headroom_of(&OutputLuminance { reference: 0, max: 1000, target_max: None }), None);
        assert_eq!(headroom_of(&OutputLuminance { reference: 200, max: 0, target_max: Some(0) }), None);
    }

    #[test]
    fn the_best_output_wins_and_sdr_outputs_count_for_nothing() {
        let sdr = OutputLuminance { reference: 200, max: 200, target_max: Some(200) };
        let hdr = OutputLuminance { reference: 203, max: 10_000, target_max: Some(812) };
        assert_eq!(best_headroom(&[sdr]), None);
        assert_eq!(best_headroom(&[]), None);
        assert!((best_headroom(&[sdr, hdr]).unwrap() - 4.0).abs() < 1e-3);
    }
}
