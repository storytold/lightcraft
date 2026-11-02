//! Give memory the system allocator keeps after frees back to the system
//! ([`lightcraft_engine::memory::set_release_hook`]). The platform call lives in
//! `lightcraft-sysmem`, the workspace's only crate allowed `unsafe`.

/// Install the hooks.
pub fn install() {
    lightcraft_engine::memory::set_release_hook(release);
    // (Windows tells its memory only through a system call: the export gate and the budget follow it)
    lightcraft_engine::memory::set_ram_probe(lightcraft_sysmem::total_physical_memory);
}

fn release() {
    let _ = lightcraft_sysmem::release_free_memory();
}
