//! Give memory the system allocator keeps after frees back to the system
//! ([`dac_engine::memory::set_release_hook`]). The platform call lives in
//! `dac-sysmem`, the workspace's only crate allowed `unsafe`.

/// Install the hook.
pub fn install() {
    dac_engine::memory::set_release_hook(release);
}

fn release() {
    let _ = dac_sysmem::release_free_memory();
}
