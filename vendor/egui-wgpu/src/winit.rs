#![expect(clippy::missing_errors_doc)]
#![expect(clippy::undocumented_unsafe_blocks)]
#![expect(clippy::unwrap_used)] // TODO(emilk): avoid unwraps
#![expect(unsafe_code)]

use crate::{RenderState, SurfaceConfig, SurfaceErrorAction, WgpuConfiguration, renderer};
use crate::{
    RendererOptions,
    capture::{CaptureReceiver, CaptureSender, CaptureState, capture_channel},
};
use core::num::NonZeroU32;
use egui::{Context, Event, UserData, ViewportId, ViewportIdMap, ViewportIdSet};
use std::sync::Arc;

struct SurfaceState {
    surface: wgpu::Surface<'static>,
    alpha_mode: wgpu::CompositeAlphaMode,
    width: u32,
    height: u32,
    resizing: bool,
    needs_reconfigure: bool,
    needs_recreate: bool,
    // Patch (unconfigured surface): whether the last configure succeeded, and whether the
    // current run of failed configures has been logged.
    configured: bool,
    configure_failing: bool,
}

/// Patch (unconfigured surface): how long a surface that failed to configure waits before
/// the next try (each try is a repaint).
const CONFIGURE_RETRY: std::time::Duration = std::time::Duration::from_millis(100);

/// Everything you need to paint egui with [`wgpu`] on [`winit`].
///
/// Alternatively you can use [`crate::Renderer`] directly.
///
/// NOTE: all egui viewports share the same painter.
pub struct Painter {
    context: Context,
    config: WgpuConfiguration,
    options: RendererOptions,
    support_transparent_backbuffer: bool,
    screen_capture_state: Option<CaptureState>,

    instance: wgpu::Instance,
    render_state: Option<RenderState>,

    // Per viewport/window:
    depth_texture_view: ViewportIdMap<wgpu::TextureView>,
    msaa_texture_view: ViewportIdMap<wgpu::TextureView>,
    surfaces: ViewportIdMap<SurfaceState>,
    capture_tx: CaptureSender,
    capture_rx: CaptureReceiver,
}

impl Painter {
    /// Manages [`wgpu`] state, including surface state, required to render egui.
    ///
    /// Only the [`wgpu::Instance`] is initialized here. Device selection and the initialization
    /// of render + surface state is deferred until the painter is given its first window target
    /// via [`set_window()`](Self::set_window). (Ensuring that a device that's compatible with the
    /// native window is chosen)
    ///
    /// Before calling [`paint_and_update_textures()`](Self::paint_and_update_textures) a
    /// [`wgpu::Surface`] must be initialized (and corresponding render state) by calling
    /// [`set_window()`](Self::set_window) once you have
    /// a [`winit::window::Window`] with a valid `.raw_window_handle()`
    /// associated.
    pub async fn new(
        context: Context,
        config: WgpuConfiguration,
        support_transparent_backbuffer: bool,
        options: RendererOptions,
    ) -> Self {
        let (capture_tx, capture_rx) = capture_channel();
        let instance = config.wgpu_setup.new_instance().await;

        Self {
            context,
            config,
            options,
            support_transparent_backbuffer,
            screen_capture_state: None,

            instance,
            render_state: None,

            depth_texture_view: Default::default(),
            surfaces: Default::default(),
            msaa_texture_view: Default::default(),

            capture_tx,
            capture_rx,
        }
    }

    /// Get the [`RenderState`].
    ///
    /// Will return [`None`] if the render state has not been initialized yet.
    pub fn render_state(&self) -> Option<RenderState> {
        self.render_state.clone()
    }

    fn configure_surface(
        surface_state: &mut SurfaceState,
        render_state: &RenderState,
        config: &SurfaceConfig,
    ) {
        profiling::function_scope!();

        let SurfaceConfig {
            present_mode,
            desired_maximum_frame_latency,
        } = *config;

        // Transaction presentation can hold a drawable during AppKit live resize. Keep the
        // configured low-latency path normally, but use three Metal drawables while resizing.
        #[cfg(all(target_os = "macos", feature = "macos-window-resize-jitter-fix"))]
        let desired_maximum_frame_latency = if surface_state.resizing {
            Some(desired_maximum_frame_latency.unwrap_or(2).max(2))
        } else {
            desired_maximum_frame_latency
        };

        let width = surface_state.width;
        let height = surface_state.height;

        let mut surf_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: render_state.target_format,
            present_mode,
            alpha_mode: surface_state.alpha_mode,
            view_formats: vec![render_state.target_format],
            ..surface_state
                .surface
                .get_default_config(&render_state.adapter, width, height)
                .expect("The surface isn't supported by this adapter")
        };
        // Patch (HDR display output): the extended-range colour space, where this surface
        // offers it (another monitor's window may not: then the backend's default).
        if render_state.target_color_space != wgpu::SurfaceColorSpace::Auto {
            let offered = surface_state
                .surface
                .get_capabilities(&render_state.adapter)
                .color_spaces(render_state.target_format);
            if render_state
                .target_color_space
                .to_color_spaces()
                .is_some_and(|c| offered.contains(c))
            {
                surf_config.color_space = render_state.target_color_space;
            } else {
                log::warn!(
                    "HDR display: this window's surface doesn't offer {:?}",
                    render_state.target_color_space
                );
            }
        }

        // Patch (present mode): the app's choice for this adapter and colour space, and FIFO
        // when the surface doesn't offer the mode (Mailbox / Immediate are optional in Vulkan).
        let offered_modes = surface_state
            .surface
            .get_capabilities(&render_state.adapter)
            .present_modes;
        if let Some(hook) = &render_state.choose_present_mode
            && let Some(mode) = hook(
                &render_state.adapter.get_info(),
                surf_config.color_space,
                &offered_modes,
            )
        {
            surf_config.present_mode = mode;
        }
        if !matches!(
            surf_config.present_mode,
            wgpu::PresentMode::AutoVsync | wgpu::PresentMode::AutoNoVsync
        ) && !offered_modes.contains(&surf_config.present_mode)
        {
            log::warn!(
                "window surface: {:?} isn't offered; using FIFO",
                surf_config.present_mode
            );
            surf_config.present_mode = wgpu::PresentMode::AutoVsync;
        }

        if let Some(desired_maximum_frame_latency) = desired_maximum_frame_latency {
            surf_config.desired_maximum_frame_latency = desired_maximum_frame_latency;
        }

        // Patch (unconfigured surface): a failed configure (seen at launch on Metal: "Failed to
        // wait for GPU to come idle") is caught here instead of reaching the device's
        // uncaptured-error handler, and the surface is marked unconfigured, so no frame is
        // acquired from it until a configure succeeds (see `acquire_frame`). A configure on a
        // lost device reports no error at all, so it still counts as a success here.
        let out_of_memory = render_state
            .device
            .push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let validation = render_state
            .device
            .push_error_scope(wgpu::ErrorFilter::Validation);
        surface_state
            .surface
            .configure(&render_state.device, &surf_config);
        // Inner scope first: both are always popped.
        let validation = pop_now(validation);
        let out_of_memory = pop_now(out_of_memory);
        if let Some(error) = validation.or(out_of_memory) {
            if !surface_state.configure_failing {
                log::warn!("window surface: configure failed, retrying: {error}");
            }
            surface_state.configured = false;
            surface_state.configure_failing = true;
            return;
        }
        if surface_state.configure_failing {
            log::info!("window surface: configured after failing");
        }
        surface_state.configured = true;
        surface_state.configure_failing = false;
        // Patch (HDR display output): the display may have changed (moved window, OS HDR
        // switch, brightness): re-query its HDR facts.
        *render_state.display_hdr_info.write() = Some(
            surface_state
                .surface
                .display_hdr_info(&render_state.adapter),
        );
        if let Some(hook) = &render_state.on_surface_configured {
            hook(&render_state.device, &surface_state.surface);
        }
    }

    /// Drop the existing [`wgpu::Surface`] for `viewport_id` and create a fresh one for the
    /// given window via [`wgpu::Instance::create_surface`], then configure it.
    ///
    /// Used to recover from [`wgpu::CurrentSurfaceTexture::Lost`], where reconfiguring the
    /// existing surface object cannot recover.
    fn recreate_surface(
        &mut self,
        viewport_id: ViewportId,
        window: &Arc<winit::window::Window>,
    ) -> Result<(), crate::WgpuError> {
        profiling::function_scope!();

        let Some(old_state) = self.surfaces.remove(&viewport_id) else {
            return Ok(());
        };

        let surface = self.instance.create_surface(Arc::clone(window))?;
        self.install_surface(
            surface,
            viewport_id,
            old_state.width,
            old_state.height,
            old_state.resizing,
        );
        Ok(())
    }

    /// Updates (or clears) the [`winit::window::Window`] associated with the [`Painter`]
    ///
    /// This creates a [`wgpu::Surface`] for the given Window (as well as initializing render
    /// state if needed) that is used for egui rendering.
    ///
    /// This must be called before trying to render via
    /// [`paint_and_update_textures`](Self::paint_and_update_textures)
    ///
    /// # Portability
    ///
    /// _In particular it's important to note that on Android a it's only possible to create
    /// a window surface between `Resumed` and `Paused` lifecycle events, and Winit will panic on
    /// attempts to query the raw window handle while paused._
    ///
    /// On Android [`set_window`](Self::set_window) should be called with `Some(window)` for each
    /// `Resumed` event and `None` for each `Paused` event. Currently, on all other platforms
    /// [`set_window`](Self::set_window) may be called with `Some(window)` as soon as you have a
    /// valid [`winit::window::Window`].
    ///
    /// # Errors
    /// If the provided wgpu configuration does not match an available device.
    pub async fn set_window(
        &mut self,
        viewport_id: ViewportId,
        window: Option<Arc<winit::window::Window>>,
    ) -> Result<(), crate::WgpuError> {
        profiling::scope!("Painter::set_window"); // profile_function gives bad names for async functions

        if let Some(window) = window {
            let size = window.inner_size();
            if !self.surfaces.contains_key(&viewport_id) {
                let surface = self.instance.create_surface(window)?;
                self.add_surface(surface, viewport_id, size).await?;
            }
        } else {
            log::warn!("No window - clearing all surfaces");
            self.surfaces.clear();
        }
        Ok(())
    }

    /// Updates (or clears) the [`winit::window::Window`] associated with the [`Painter`] without taking ownership of the window.
    ///
    /// Like [`set_window`](Self::set_window) except:
    ///
    /// # Safety
    /// The user is responsible for ensuring that the window is alive for as long as it is set.
    pub async unsafe fn set_window_unsafe(
        &mut self,
        viewport_id: ViewportId,
        window: Option<&winit::window::Window>,
    ) -> Result<(), crate::WgpuError> {
        profiling::scope!("Painter::set_window_unsafe"); // profile_function gives bad names for async functions

        if let Some(window) = window {
            let size = window.inner_size();
            if !self.surfaces.contains_key(&viewport_id) {
                let surface = unsafe {
                    self.instance
                        .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::from_window(&window)?)?
                };
                self.add_surface(surface, viewport_id, size).await?;
            }
        } else {
            log::warn!("No window - clearing all surfaces");
            self.surfaces.clear();
        }
        Ok(())
    }

    async fn add_surface(
        &mut self,
        surface: wgpu::Surface<'static>,
        viewport_id: ViewportId,
        size: winit::dpi::PhysicalSize<u32>,
    ) -> Result<(), crate::WgpuError> {
        if self.render_state.is_none() {
            let render_state =
                RenderState::create(&self.config, &self.instance, Some(&surface), self.options)
                    .await?;
            self.render_state = Some(render_state);
        }
        self.install_surface(surface, viewport_id, size.width, size.height, false);
        Ok(())
    }

    /// Inserts a freshly created surface into [`Self::surfaces`] and configures it.
    ///
    /// Render state must already be initialised before calling this.
    // NOTE: The same assumption is already required by `resize_and_generate_depth_texture_view_and_msaa_view`.
    fn install_surface(
        &mut self,
        surface: wgpu::Surface<'static>,
        viewport_id: ViewportId,
        width: u32,
        height: u32,
        resizing: bool,
    ) {
        let alpha_mode = {
            // Panic: We use the same failure mode as `resize_and_generate_depth_texture_view_and_msaa_view`
            let render_state = self
                .render_state
                .as_ref()
                .expect("install_surface called before render_state initialization");
            if self.support_transparent_backbuffer {
                let supported_alpha_modes =
                    surface.get_capabilities(&render_state.adapter).alpha_modes;
                // Prefer pre multiplied over post multiplied!
                if supported_alpha_modes.contains(&wgpu::CompositeAlphaMode::PreMultiplied) {
                    wgpu::CompositeAlphaMode::PreMultiplied
                } else if supported_alpha_modes.contains(&wgpu::CompositeAlphaMode::PostMultiplied)
                {
                    wgpu::CompositeAlphaMode::PostMultiplied
                } else {
                    log::warn!(
                        "Transparent window was requested, but the active wgpu surface does not support a `CompositeAlphaMode` with transparency."
                    );
                    wgpu::CompositeAlphaMode::Auto
                }
            } else {
                wgpu::CompositeAlphaMode::Auto
            }
        };
        self.surfaces.insert(
            viewport_id,
            SurfaceState {
                surface,
                width,
                height,
                alpha_mode,
                resizing,
                needs_reconfigure: false,
                needs_recreate: false,
                configured: false,
                configure_failing: false,
            },
        );
        let Some(width) = NonZeroU32::new(width) else {
            log::debug!("The window width was zero; skipping generate textures");
            return;
        };
        let Some(height) = NonZeroU32::new(height) else {
            log::debug!("The window height was zero; skipping generate textures");
            return;
        };
        self.resize_and_generate_depth_texture_view_and_msaa_view(viewport_id, width, height);
    }

    /// Returns the maximum texture dimension supported if known
    ///
    /// This API will only return a known dimension after `set_window()` has been called
    /// at least once, since the underlying device and render state are initialized lazily
    /// once we have a window (that may determine the choice of adapter/device).
    pub fn max_texture_side(&self) -> Option<usize> {
        self.render_state
            .as_ref()
            .map(|rs| rs.device.limits().max_texture_dimension_2d as usize)
    }

    fn resize_and_generate_depth_texture_view_and_msaa_view(
        &mut self,
        viewport_id: ViewportId,
        width_in_pixels: NonZeroU32,
        height_in_pixels: NonZeroU32,
    ) {
        profiling::function_scope!();

        let width = width_in_pixels.get();
        let height = height_in_pixels.get();

        let render_state = self.render_state.as_ref().unwrap();
        let surface_state = self.surfaces.get_mut(&viewport_id).unwrap();

        surface_state.width = width;
        surface_state.height = height;

        Self::configure_surface(surface_state, render_state, &self.config.surface);

        if let Some(depth_format) = self.options.depth_stencil_format {
            self.depth_texture_view.insert(
                viewport_id,
                render_state
                    .device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some("egui_depth_texture"),
                        size: wgpu::Extent3d {
                            width,
                            height,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: self.options.msaa_samples.max(1),
                        dimension: wgpu::TextureDimension::D2,
                        format: depth_format,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                            | wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[depth_format],
                    })
                    .create_view(&wgpu::TextureViewDescriptor::default()),
            );
        }

        if let Some(render_state) = (self.options.msaa_samples > 1)
            .then_some(self.render_state.as_ref())
            .flatten()
        {
            let texture_format = render_state.target_format;
            self.msaa_texture_view.insert(
                viewport_id,
                render_state
                    .device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some("egui_msaa_texture"),
                        size: wgpu::Extent3d {
                            width,
                            height,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: self.options.msaa_samples.max(1),
                        dimension: wgpu::TextureDimension::D2,
                        format: texture_format,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                        view_formats: &[texture_format],
                    })
                    .create_view(&wgpu::TextureViewDescriptor::default()),
            );
        }
    }

    /// Handles changes of the resizing state.
    ///
    /// Should be called prior to the first [`Painter::on_window_resized`] call and after the last in
    /// the chain. Used to apply platform-specific logic, e.g. OSX Metal window resize jitter fix.
    pub fn on_window_resize_state_change(&mut self, viewport_id: ViewportId, resizing: bool) {
        profiling::function_scope!();

        let Some(state) = self.surfaces.get_mut(&viewport_id) else {
            return;
        };
        if state.resizing == resizing {
            if resizing {
                log::debug!(
                    "Painter::on_window_resize_state_change() redundant call while resizing"
                );
            } else {
                log::debug!(
                    "Painter::on_window_resize_state_change() redundant call after resizing"
                );
            }
            return;
        }

        // Set before reconfiguring so macOS live resize uses the temporary latency bump above.
        state.resizing = resizing;

        // Resizing is a bit tricky on macOS.
        // It requires enabling ["present_with_transaction"](https://developer.apple.com/documentation/quartzcore/cametallayer/presentswithtransaction)
        // flag to avoid jittering during the resize. Even though resize jittering on macOS
        // is common across rendering backends, the solution for wgpu/metal is known.
        //
        // See https://github.com/emilk/egui/issues/903
        #[cfg(all(target_os = "macos", feature = "macos-window-resize-jitter-fix"))]
        {
            // Patch (unconfigured surface): the Metal surface is released before the configure,
            // which now takes the surface state mutably.
            if let Some(render_state) = self.render_state.as_ref() {
                // SAFETY: `as_hal::<Metal>()` returns `None` unless this surface is backed by
                // wgpu's Metal backend.
                let metal = unsafe {
                    state
                        .surface
                        .as_hal::<wgpu::hal::api::Metal>()
                        .map(|hal_surface| {
                            hal_surface
                                .render_layer()
                                .lock()
                                .setPresentsWithTransaction(resizing);
                        })
                        .is_some()
                };
                if metal {
                    Self::configure_surface(state, render_state, &self.config.surface);
                }
            }
        }
    }

    pub fn on_window_resized(
        &mut self,
        viewport_id: ViewportId,
        width_in_pixels: NonZeroU32,
        height_in_pixels: NonZeroU32,
    ) {
        profiling::function_scope!();

        if self.surfaces.contains_key(&viewport_id) {
            self.resize_and_generate_depth_texture_view_and_msaa_view(
                viewport_id,
                width_in_pixels,
                height_in_pixels,
            );
        } else {
            log::warn!(
                "Ignoring window resize notification with no surface created via Painter::set_window()"
            );
        }
    }

    /// Patch (unconfigured surface): the surface's next texture and the seconds spent waiting
    /// for it, after configuring the surface when asked to or when its last configure failed;
    /// `None` while it still can't be configured. wgpu panics when a surface that never got
    /// configured is asked for a texture, and the panic can't unwind out of winit's callbacks,
    /// so the app aborted (a launch with the GPU busy).
    fn acquire_frame(
        surface_state: &mut SurfaceState,
        render_state: &RenderState,
        config: &SurfaceConfig,
    ) -> Option<(wgpu::CurrentSurfaceTexture, f32)> {
        if surface_state.needs_reconfigure || !surface_state.configured {
            Self::configure_surface(surface_state, render_state, config);
            surface_state.needs_reconfigure = false;
        }
        if !surface_state.configured {
            return None;
        }

        profiling::scope!("get_current_texture");
        // This is what vsync-waiting happens on my Mac.
        let start = web_time::Instant::now();
        let output_frame = surface_state.surface.get_current_texture();
        Some((output_frame, start.elapsed().as_secs_f32()))
    }

    /// Returns two things:
    ///
    /// The approximate number of seconds spent on vsync-waiting (if any),
    /// and the captures captured screenshot if it was requested.
    ///
    /// If `capture_data` isn't empty, a screenshot will be captured.
    #[expect(clippy::too_many_arguments)]
    pub fn paint_and_update_textures(
        &mut self,
        viewport_id: ViewportId,
        pixels_per_point: f32,
        clear_color: [f32; 4],
        clipped_primitives: &[epaint::ClippedPrimitive],
        textures_delta: &mut epaint::textures::TexturesDelta,
        capture_data: Vec<UserData>,
        window: &Arc<winit::window::Window>,
    ) -> f32 {
        profiling::function_scope!();

        /// Guard to ensure that commands are always submitted to the renderer queue
        /// so that calls to [`write_buffer()`](https://docs.rs/wgpu/latest/wgpu/struct.Queue.html#method.write_buffer)
        /// are completed even if we take a codepath which doesn't submit commands and avoids
        /// internal buffers growing indefinitely.
        ///
        /// This may happen, for example, if no output frame is resolved.
        /// See <https://github.com/emilk/egui/pull/7928> for full context.
        struct RendererQueueGuard<'q> {
            queue: &'q wgpu::Queue,
            commands_submitted: bool,
        }

        impl Drop for RendererQueueGuard<'_> {
            fn drop(&mut self) {
                // Only submit an empty command buffer array if no commands were
                // explicitly submitted.
                if !self.commands_submitted {
                    self.queue.submit([]);
                }
            }
        }

        let capture = !capture_data.is_empty();
        let mut vsync_sec = 0.0;

        // If the previous frame produced `CurrentSurfaceTexture::Lost`, the action match
        // below set `needs_recreate`. Recreate the surface now, before re-borrowing
        // `self.render_state` / `self.surfaces` for the rest of the paint.
        if self
            .surfaces
            .get(&viewport_id)
            .is_some_and(|s| s.needs_recreate)
            && let Err(err) = self.recreate_surface(viewport_id, window)
        {
            log::error!("Failed to recreate surface for {viewport_id:?}: {err}");
            return vsync_sec;
        }

        // Apply any runtime changes requested via `RenderState::surface_config`.
        // We diff against the already-applied values in `self.config.surface`
        // and, if anything differs, mark every surface as needing reconfiguration so
        // the existing `needs_reconfigure` pathway below picks them up.
        if let Some(render_state) = self.render_state.as_ref()
            && render_state.surface_config != self.config.surface
        {
            self.config.surface = render_state.surface_config;
            #[expect(clippy::iter_over_hash_type)]
            for surface in self.surfaces.values_mut() {
                surface.needs_reconfigure = true;
            }
        }

        let Some(render_state) = self.render_state.as_mut() else {
            return vsync_sec;
        };

        let mut render_queue_guard = RendererQueueGuard {
            queue: &render_state.queue,
            commands_submitted: false,
        };

        let Some(surface_state) = self.surfaces.get_mut(&viewport_id) else {
            return vsync_sec;
        };

        let mut encoder =
            render_state
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("encoder"),
                });

        // Upload all resources for the GPU.
        let screen_descriptor = renderer::ScreenDescriptor {
            size_in_pixels: [surface_state.width, surface_state.height],
            pixels_per_point,
        };

        let user_cmd_bufs = {
            let mut renderer = render_state.renderer.write();
            #[expect(clippy::iter_over_hash_type)] // Order doesn't matter here
            for (id, image_deltas) in textures_delta.set.drain() {
                for image_delta in image_deltas {
                    renderer.update_texture(
                        &render_state.device,
                        &render_state.queue,
                        id,
                        &image_delta,
                    );
                }
            }

            renderer.update_buffers(
                &render_state.device,
                &render_state.queue,
                &mut encoder,
                clipped_primitives,
                &screen_descriptor,
            )
        };

        // Patch (unconfigured surface): skip the frame while the surface can't be configured,
        // and try again shortly.
        let Some((output_frame, waited)) =
            Self::acquire_frame(surface_state, render_state, &self.config.surface)
        else {
            self.context
                .request_repaint_after_for(CONFIGURE_RETRY, viewport_id);
            return vsync_sec;
        };
        vsync_sec += waited;

        // Patch (device loss): tell the app a frame got its texture (see `on_frame_acquired`).
        if matches!(
            output_frame,
            wgpu::CurrentSurfaceTexture::Success(_) | wgpu::CurrentSurfaceTexture::Suboptimal(_)
        ) && let Some(acquired) = &self.config.on_frame_acquired
        {
            acquired();
        }
        let output_frame = match output_frame {
            wgpu::CurrentSurfaceTexture::Success(frame) => frame,
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                surface_state.needs_reconfigure = true;
                frame
            }
            other => {
                match (*self.config.on_surface_status)(&other) {
                    SurfaceErrorAction::Reconfigure => {
                        Self::configure_surface(surface_state, render_state, &self.config.surface);
                        self.context.request_repaint_of(viewport_id);
                    }
                    SurfaceErrorAction::RecreateSurface => {
                        // Because of ownership, I could not find an easy way to do a full recovery here,
                        // as that would involve dropping the old surface and creating a new one.
                        // For now, we defer the recreation to the beginning of the next frame (which
                        // we ensure to arrive via `request_repaint_of`). A cleaner solution would be
                        // to untangle the ownership of `RenderState`.
                        surface_state.needs_recreate = true;
                        self.context.request_repaint_of(viewport_id);
                    }
                    SurfaceErrorAction::SkipFrame => {}
                }
                return vsync_sec;
            }
        };

        let mut capture_buffer = None;
        {
            let renderer = render_state.renderer.read();

            let target_texture = if capture {
                let capture_state = self.screen_capture_state.get_or_insert_with(|| {
                    CaptureState::new(&render_state.device, &output_frame.texture)
                });
                capture_state.update(&render_state.device, &output_frame.texture);

                &capture_state.texture
            } else {
                &output_frame.texture
            };
            let target_view = target_texture.create_view(&wgpu::TextureViewDescriptor::default());

            let (view, resolve_target) = (self.options.msaa_samples > 1)
                .then_some(self.msaa_texture_view.get(&viewport_id))
                .flatten()
                .map_or((&target_view, None), |texture_view| {
                    (texture_view, Some(&target_view))
                });

            let render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui_render"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    resolve_target,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: clear_color[0] as f64,
                            g: clear_color[1] as f64,
                            b: clear_color[2] as f64,
                            a: clear_color[3] as f64,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: self.depth_texture_view.get(&viewport_id).map(|view| {
                    wgpu::RenderPassDepthStencilAttachment {
                        view,
                        depth_ops: self
                            .options
                            .depth_stencil_format
                            .is_some_and(|depth_stencil_format| {
                                depth_stencil_format.has_depth_aspect()
                            })
                            .then_some(wgpu::Operations {
                                load: wgpu::LoadOp::Clear(1.0),
                                // It is very unlikely that the depth buffer is needed after egui finished rendering
                                // so no need to store it. (this can improve performance on tiling GPUs like mobile chips or Apple Silicon)
                                store: wgpu::StoreOp::Discard,
                            }),
                        stencil_ops: self
                            .options
                            .depth_stencil_format
                            .is_some_and(|depth_stencil_format| {
                                depth_stencil_format.has_stencil_aspect()
                            })
                            .then_some(wgpu::Operations {
                                load: wgpu::LoadOp::Clear(0),
                                store: wgpu::StoreOp::Discard,
                            }),
                    }
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            // Forgetting the pass' lifetime means that we are no longer compile-time protected from
            // runtime errors caused by accessing the parent encoder before the render pass is dropped.
            // Since we don't pass it on to the renderer, we should be perfectly safe against this mistake here!
            renderer.render(
                &mut render_pass.forget_lifetime(),
                clipped_primitives,
                &screen_descriptor,
            );

            if capture && let Some(capture_state) = &mut self.screen_capture_state {
                capture_buffer = Some(capture_state.copy_textures(
                    &render_state.device,
                    &output_frame,
                    &mut encoder,
                ));
            }
        }

        let encoded = {
            profiling::scope!("CommandEncoder::finish");
            encoder.finish()
        };

        // Submit the commands: both the main buffer and user-defined ones.
        {
            profiling::scope!("Queue::submit");
            // wgpu doesn't document where vsync can happen. Maybe here?
            let start = web_time::Instant::now();
            render_state
                .queue
                .submit(core::iter::chain(user_cmd_bufs, [encoded]));
            vsync_sec += start.elapsed().as_secs_f32();
        };

        // Ensure that the queue guard does not do unnecessary work when dropped
        render_queue_guard.commands_submitted = true;

        // Free textures marked for destruction **after** queue submit since they might still be used in the current frame.
        // Calling `wgpu::Texture::destroy` on a texture that is still in use would invalidate the command buffer(s) it is used in.
        // However, once we called `wgpu::Queue::submit`, it is up for wgpu to determine how long the underlying gpu resource has to live.
        {
            let mut renderer = render_state.renderer.write();
            #[expect(clippy::iter_over_hash_type)] // Order doesn't matter here
            for id in textures_delta.free.drain() {
                renderer.free_texture(&id);
            }
        }

        let (output_scale, pq_output) = {
            let r = render_state.renderer.read();
            (r.output_scale(), r.pq_output())
        };
        if let Some(capture_buffer) = capture_buffer
            && let Some(screen_capture_state) = &mut self.screen_capture_state
        {
            screen_capture_state.read_screen_rgba(
                self.context.clone(),
                capture_buffer,
                capture_data,
                self.capture_tx.clone(),
                viewport_id,
                output_scale,
                pq_output,
            );
        }

        window.pre_present_notify();

        {
            profiling::scope!("present");
            // wgpu doesn't document where vsync can happen. Maybe here?
            let start = web_time::Instant::now();
            render_state.queue.present(output_frame);
            vsync_sec += start.elapsed().as_secs_f32();
        }

        vsync_sec
    }

    /// Call this at the beginning of each frame to receive the requested screenshots.
    pub fn handle_screenshots(&self, events: &mut Vec<Event>) {
        for (viewport_id, user_data, screenshot) in self.capture_rx.try_iter() {
            let screenshot = Arc::new(screenshot);
            for data in user_data {
                events.push(Event::Screenshot {
                    viewport_id,
                    user_data: data,
                    image: Arc::clone(&screenshot),
                });
            }
        }
    }

    pub fn gc_viewports(&mut self, active_viewports: &ViewportIdSet) {
        self.surfaces.retain(|id, _| active_viewports.contains(id));
        self.depth_texture_view
            .retain(|id, _| active_viewports.contains(id));
        self.msaa_texture_view
            .retain(|id, _| active_viewports.contains(id));
    }

    #[expect(clippy::needless_pass_by_ref_mut, clippy::unused_self)]
    pub fn destroy(&mut self) {
        // TODO(emilk): something here?
    }
}

/// Patch (unconfigured surface): the error an error scope caught, read without blocking. Native
/// wgpu hands the scope's result back at once; where it doesn't, the configure counts as a
/// success, as every configure did before.
fn pop_now(scope: wgpu::ErrorScopeGuard) -> Option<wgpu::Error> {
    let mut popped = std::pin::pin!(scope.pop());
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    match popped.as_mut().poll(&mut cx) {
        std::task::Poll::Ready(error) => error,
        std::task::Poll::Pending => None,
    }
}

// Patch (unconfigured surface): a real Metal surface, on a `CAMetalLayer` with no window.
// Vendored crates aren't workspace members: run with `cargo test -p egui-wgpu` from the root.
#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::{Painter, SurfaceState};
    use crate::{RenderState, RendererOptions, WgpuConfiguration};
    use core::ffi::{c_char, c_void};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> *mut c_void;
        fn sel_registerName(name: *const c_char) -> *mut c_void;
        fn objc_msgSend();
    }

    /// `[receiver selector]` for a selector that takes no arguments and returns an object.
    unsafe fn send(receiver: *mut c_void, selector: &core::ffi::CStr) -> *mut c_void {
        let send: unsafe extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            unsafe { std::mem::transmute(objc_msgSend as unsafe extern "C" fn()) };
        unsafe { send(receiver, sel_registerName(selector.as_ptr())) }
    }

    /// A `CAMetalLayer` (QuartzCore is linked through wgpu's Metal backend), released on drop.
    struct MetalLayer(*mut c_void);

    impl MetalLayer {
        fn new() -> Self {
            let class = unsafe { objc_getClass(c"CAMetalLayer".as_ptr()) };
            assert!(!class.is_null(), "CAMetalLayer should be loaded");
            let layer = unsafe { send(class, c"new") };
            assert!(!layer.is_null(), "a CAMetalLayer should be created");
            Self(layer)
        }
    }

    impl Drop for MetalLayer {
        fn drop(&mut self) {
            unsafe { send(self.0, c"release") };
        }
    }

    /// Native wgpu's futures are ready at once.
    fn ready<F: Future>(future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        match future.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(output) => output,
            std::task::Poll::Pending => panic!("a native wgpu future should be ready at once"),
        }
    }

    /// A launch with the GPU busy: the window surface's first configure failed, the next frame
    /// asked it for a texture, and wgpu panicked ("Surface is not configured for presentation"),
    /// aborting the app. A surface whose configure fails is configured again before each frame,
    /// is never asked for a texture meanwhile, and draws again once a configure succeeds.
    #[test]
    fn a_surface_that_failed_to_configure_is_retried_and_never_asked_for_a_frame() {
        let configured = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&configured);
        let config = WgpuConfiguration {
            on_surface_configured: Some(Arc::new(move |_, _| {
                count.fetch_add(1, Ordering::Relaxed);
            })),
            ..Default::default()
        };
        let instance = ready(config.wgpu_setup.new_instance());
        let layer = MetalLayer::new();
        let surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::CoreAnimationLayer(layer.0))
        }
        .expect("a surface on the layer");
        let render_state = match ready(RenderState::create(
            &config,
            &instance,
            Some(&surface),
            RendererOptions::default(),
        )) {
            Ok(render_state) => render_state,
            Err(e) => {
                eprintln!("skipped: no Metal adapter ({e})");
                return;
            }
        };
        // As the app does: an error outside a scope is counted, not wgpu's default panic, so a
        // surface left unconfigured reaches the frame path as it did in the app.
        let uncaptured = Arc::new(AtomicUsize::new(0));
        let errors = Arc::clone(&uncaptured);
        render_state.device.on_uncaptured_error(Arc::new(move |_| {
            errors.fetch_add(1, Ordering::Relaxed);
        }));
        // A size over the device's limit makes the configure fail, as the busy GPU did.
        let too_wide = render_state.device.limits().max_texture_dimension_2d + 1;
        let mut state = SurfaceState {
            surface,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            width: too_wide,
            height: 64,
            resizing: false,
            needs_reconfigure: false,
            needs_recreate: false,
            configured: false,
            configure_failing: false,
        };

        Painter::configure_surface(&mut state, &render_state, &config.surface);
        assert!(
            !state.configured,
            "a configure over the size limit should fail"
        );
        for _ in 0..2 {
            assert!(
                Painter::acquire_frame(&mut state, &render_state, &config.surface).is_none(),
                "a surface that can't be configured should give no frame, not be asked for one"
            );
        }
        assert_eq!(
            configured.load(Ordering::Relaxed),
            0,
            "the configured hook should run only after a configure that worked"
        );
        assert_eq!(
            uncaptured.load(Ordering::Relaxed),
            0,
            "a failed configure should be caught by the painter, not reach the device's uncaptured-error handler"
        );

        state.width = 64;
        let frame = Painter::acquire_frame(&mut state, &render_state, &config.surface);
        assert!(
            state.configured,
            "the surface should be configured again on the next frame"
        );
        assert!(
            frame.is_some(),
            "a configured surface should be asked for its frame"
        );
        assert_eq!(
            configured.load(Ordering::Relaxed),
            1,
            "the configured hook should run once the configure works"
        );
        drop(frame);
        drop(state);
        drop(layer);
    }
}
