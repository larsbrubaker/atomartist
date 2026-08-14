//! wgpu device + surface setup for the native shell.
//!
//! Split out of `main.rs` to keep the entry point under the project's
//! 800-line file cap. The surface format selection prefers a
//! non-sRGB swap chain because the renderer outputs colors in
//! perceptual space already; `COPY_SRC` is required so the screenshot
//! capture path can copy the live framebuffer into a staging texture.

use std::sync::Arc;

use winit::window::Window;

pub(crate) struct Gpu {
    pub(crate) device: Arc<wgpu::Device>,
    pub(crate) queue: Arc<wgpu::Queue>,
    pub(crate) surface: wgpu::Surface<'static>,
    pub(crate) surface_format: wgpu::TextureFormat,
    pub(crate) config: wgpu::SurfaceConfiguration,
}

impl Gpu {
    pub(crate) fn new(window: Arc<Window>) -> Self {
        let size = window.inner_size();
        let mut instance_desc = wgpu::InstanceDescriptor::new_without_display_handle();
        instance_desc.backends = wgpu::Backends::PRIMARY;
        let instance = wgpu::Instance::new(instance_desc);
        let surface = instance
            .create_surface(window.clone())
            .expect("create surface");
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .expect("request adapter");

        // Enable 32-bit-float blending when the adapter offers it: the
        // dual-peel chain stores depth in a blendable float target, and
        // half-float can't separate perspective-compressed transparent
        // layers (they collapse into one bias band and blend in draw
        // order — painter's algorithm). Requested only when available so
        // the app still starts on adapters without it (falling back to
        // half-float depth).
        let float32_blend = adapter.features() & wgpu::Features::FLOAT32_BLENDABLE;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("atomartist-native-wgpu"),
            required_features: float32_blend,
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::Performance,
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        }))
        .expect("request device");

        // Mirror the WASM shell: route uncaptured wgpu errors to stderr
        // rather than letting them vanish. Without a handler a failed
        // texture allocation or rejected shader module just produces an
        // empty frame, which reads as a renderer bug rather than a
        // resource problem.
        device.on_uncaptured_error(std::sync::Arc::new(|e: wgpu::Error| {
            eprintln!("wgpu uncaptured error: {e}");
        }));

        let caps = surface.get_capabilities(&adapter);
        let surface_format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            // COPY_SRC required for the screenshot capture path (which
            // copies the surface into an internal capture texture).
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            format: surface_format,
            width: size.width.max(1),
            height: size.height.max(1),
            // NOT AutoVsync (FIFO): on Windows a reactive (on-demand)
            // redraw loop paired with FIFO makes `get_current_texture()`
            // block on the DWM present queue for many vblank intervals —
            // the ~90 ms/frame stall seen even on a trivial scene, while
            // the WASM build (browser compositor, non-blocking) stays
            // smooth. AutoNoVsync resolves to Immediate/Mailbox so each
            // frame presents without pacing to the refresh rate. A CAD
            // viewport tolerates the occasional tear for the latency win.
            present_mode: wgpu::PresentMode::AutoNoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);

        Self {
            device: Arc::new(device),
            queue: Arc::new(queue),
            surface,
            surface_format,
            config,
        }
    }

    pub(crate) fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 { return; }
        self.config.width = w;
        self.config.height = h;
        self.surface.configure(&self.device, &self.config);
    }
}
