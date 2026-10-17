//! Give memory the system allocator keeps after frees back to the system
//! ([`lcc_engine::memory::set_release_hook`]). The platform call lives in
//! `lcc-sysmem`, the workspace's only crate allowed `unsafe`.

/// Install the hook.
pub fn install() {
    lcc_engine::memory::set_release_hook(release);
}

fn release() {
    let _ = lcc_sysmem::release_free_memory();
}
