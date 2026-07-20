//! WGPU device and surface management.

use pollster::FutureExt;
use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle,
    RawWindowHandle, WindowHandle,
};
use std::sync::Arc;

/// Holds the WGPU instance, device, queue, and surface configuration.
pub struct WgpuState {
    pub instance: wgpu::Instance,
    pub surface: wgpu::Surface<'static>,
    pub device: Arc<wgpu::Device>,
    pub queue: Arc<wgpu::Queue>,
    pub config: wgpu::SurfaceConfiguration,
    /// Whether the surface was successfully configured for presentation.
    configured: bool,
}

impl WgpuState {
    /// Create a new WGPU state from raw window handles.
    ///
    /// This takes raw handles instead of HasWindowHandle/HasDisplayHandle traits
    /// to work around the raw-window-handle version mismatch between baseview (0.5)
    /// and wgpu (0.6).
    pub fn new_from_raw(
        window_handle: RawWindowHandle,
        display_handle: RawDisplayHandle,
        width: u32,
        height: u32,
    ) -> Self {
        // Use Vulkan only on Linux — GL/EGL has adapter-surface compatibility
        // issues with embedded X11 windows (visual mismatch, NV NATIVE_RENDERABLE=0).
        // EGL is still pre-loaded below so its initialization doesn't log errors.
        #[cfg(target_os = "linux")]
        let backends = wgpu::Backends::VULKAN;
        #[cfg(not(target_os = "linux"))]
        let backends = wgpu::Backends::all();

        // On Linux/XWayland we need two fixes before Instance::new:
        //
        // 1. Pre-load libEGL.so.1 by absolute path via libc::dlopen.
        //    On NixOS, libEGL.so is not in any standard search path, so
        //    wgpu-hal's khronos-egl dlopen("libEGL.so.1") fails. REAPER already
        //    has libGL loaded from the libglvnd nix-store path; we find that
        //    directory from /proc/self/maps and load libEGL.so.1 ourselves.
        //    Once loaded, glibc's soname cache means dlopen("libEGL.so.1") succeeds.
        //
        // 2. Hide WAYLAND_DISPLAY: if set, wgpu-hal EGL picks the Wayland EGL
        //    platform, then fails to create a window surface from the Xlib handle
        //    ("incompatible window kind") → InvalidSurface on configure.
        //    Restored immediately after Instance::new.
        #[cfg(target_os = "linux")]
        Self::preload_egl();

        #[cfg(target_os = "linux")]
        let _wayland_guard = {
            struct WaylandGuard(Option<std::ffi::OsString>);
            impl Drop for WaylandGuard {
                fn drop(&mut self) {
                    match &self.0 {
                        Some(v) => std::env::set_var("WAYLAND_DISPLAY", v),
                        None => std::env::remove_var("WAYLAND_DISPLAY"),
                    }
                }
            }
            let prev = std::env::var_os("WAYLAND_DISPLAY");
            std::env::remove_var("WAYLAND_DISPLAY");
            eprintln!(
                "[WGPU] Hiding WAYLAND_DISPLAY for EGL X11 platform (was: {:?})",
                prev
            );
            WaylandGuard(prev)
        };

        // wgpu 29: `InstanceDescriptor` no longer implements `Default`; build it explicitly.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            // ALLOW_UNDERLYING_NONCOMPLIANT_ADAPTER: Mesa 25+ RADV marks the iGPU
            // as non-compliant (mesa bug #12799); this flag lets wgpu still use it.
            flags: wgpu::InstanceFlags::from_build_config()
                | wgpu::InstanceFlags::ALLOW_UNDERLYING_NONCOMPLIANT_ADAPTER,
            memory_budget_thresholds: Default::default(),
            backend_options: Default::default(),
            display: None,
        });

        // Restore WAYLAND_DISPLAY — EGL platform is now committed to X11.
        #[cfg(target_os = "linux")]
        drop(_wayland_guard);

        // Create the surface using RawHandle directly (not from_window)
        // This gives us more control over exactly what handles are passed
        let surface = unsafe {
            match instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(display_handle),
                raw_window_handle: window_handle,
            }) {
                Ok(s) => {
                    eprintln!("[WGPU] Surface created successfully");
                    s
                }
                Err(e) => {
                    panic!("[WGPU] Failed to create surface: {e:?}");
                }
            }
        };

        // Request an adapter
        // Use HighPerformance to prefer the discrete GPU (e.g. NVIDIA RTX) over
        // integrated graphics. On Linux/Mesa 26, the AMD RADV integrated GPU
        // (RAPHAEL_MENDOCINO) has a known bug where vkCreateSwapchainKHR fails
        // for embedded Xlib windows. The discrete GPU avoids this issue.
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
            })
            .block_on()
            .expect("Failed to find an appropriate adapter");

        // Create the device and queue
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("nice_plug_dioxus device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
                // New in wgpu 28: opt-in to unstable / experimental capabilities.
                experimental_features: wgpu::ExperimentalFeatures::default(),
            })
            .block_on()
            .expect("Failed to create device");

        let device = Arc::new(device);
        let queue = Arc::new(queue);

        // Configure the surface
        // Use NON-sRGB format because Vello outputs linear RGB values, and we copy
        // them directly to the surface. Using a non-sRGB surface means the values
        // are displayed as-is without additional gamma correction.
        //
        // Note: This means CSS colors (which are specified in sRGB) need to be
        // converted to linear RGB by the rendering pipeline (Blitz/Vello).
        let surface_caps = surface.get_capabilities(&adapter);

        // Debug: log adapter and surface capabilities (eprintln so output
        // bypasses the nih_plug logger and always appears in REAPER's stderr log)
        eprintln!("[WGPU] Adapter: {:?}", adapter.get_info());
        eprintln!(
            "[WGPU] Adapter supports surface: {}",
            adapter.is_surface_supported(&surface)
        );
        eprintln!("[WGPU] Surface formats: {:?}", surface_caps.formats);
        eprintln!("[WGPU] Surface alpha modes: {:?}", surface_caps.alpha_modes);
        eprintln!(
            "[WGPU] Surface present modes: {:?}",
            surface_caps.present_modes
        );

        if surface_caps.formats.is_empty() {
            eprintln!("[WGPU] ERROR: No surface formats available - surface may be invalid");
        }

        let format = surface_caps
            .formats
            .iter()
            .find(|f| !f.is_srgb())
            .copied()
            .unwrap_or_else(|| {
                if surface_caps.formats.is_empty() {
                    nice_plug_core::nice_error!("[WGPU] Using fallback format Bgra8Unorm");
                    wgpu::TextureFormat::Bgra8Unorm
                } else {
                    surface_caps.formats[0]
                }
            });

        // Prefer Inherit alpha mode for better compatibility, especially on XWayland
        let alpha_mode = if surface_caps
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::Inherit)
        {
            wgpu::CompositeAlphaMode::Inherit
        } else if surface_caps
            .alpha_modes
            .contains(&wgpu::CompositeAlphaMode::Opaque)
        {
            wgpu::CompositeAlphaMode::Opaque
        } else {
            surface_caps.alpha_modes[0]
        };

        // Use Fifo (vsync) for reliability
        let present_mode = if surface_caps
            .present_modes
            .contains(&wgpu::PresentMode::Fifo)
        {
            wgpu::PresentMode::Fifo
        } else {
            surface_caps.present_modes[0]
        };

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: width.max(1),
            height: height.max(1),
            present_mode,
            alpha_mode,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };

        eprintln!(
            "[WGPU] Configuring surface: {}x{}, format={:?}, alpha={:?}, present={:?}",
            config.width, config.height, config.format, config.alpha_mode, config.present_mode
        );

        // Attempt an initial configure. May fail if the window isn't fully mapped yet.
        // We'll retry each frame via try_configure() without blocking.
        let configured = Self::do_configure(&surface, &device, &config);
        if configured {
            eprintln!("[WGPU] Surface configured successfully on first attempt");
        }

        Self {
            instance,
            surface,
            device,
            queue,
            config,
            configured,
        }
    }

    /// On NixOS, libEGL.so is not in standard search paths. Pre-load it by
    /// absolute path using libc::dlopen so that wgpu-hal's subsequent
    /// dlopen("libEGL.so.1") call finds it via glibc's soname cache.
    ///
    /// Strategy: REAPER already has libGL / libGLdispatch mapped from libglvnd
    /// in /proc/self/maps. We find that directory, load libEGL.so.1 from it,
    /// and leak the handle (RTLD_NODELETE equivalent via intentional forget).
    #[cfg(target_os = "linux")]
    fn preload_egl() {
        use std::ffi::CString;

        // Find the libglvnd lib directory from already-loaded GL libraries.
        let maps = match std::fs::read_to_string("/proc/self/maps") {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[WGPU] preload_egl: cannot read /proc/self/maps: {e}");
                return;
            }
        };

        let dir = maps
            .lines()
            .filter_map(|line| line.split_whitespace().last())
            .filter(|path| {
                path.contains("libGL")
                    || path.contains("libGLdispatch")
                    || path.contains("libglvnd")
            })
            .filter_map(|path| std::path::Path::new(path).parent())
            .find(|dir| dir.join("libEGL.so.1").exists())
            .map(|p| p.to_owned());

        let Some(dir) = dir else {
            eprintln!("[WGPU] preload_egl: libglvnd dir not found in /proc/self/maps");
            return;
        };

        // Try libEGL.so.1 first (khronos-egl's primary candidate), then libEGL.so.
        for name in &["libEGL.so.1", "libEGL.so"] {
            let path = dir.join(name);
            if !path.exists() {
                continue;
            }
            let Ok(c_path) = CString::new(path.as_os_str().as_encoded_bytes()) else {
                continue;
            };
            // SAFETY: valid null-terminated path string; RTLD_GLOBAL makes symbols
            // available process-wide; RTLD_NODELETE prevents unloading.
            let handle = unsafe {
                libc::dlopen(
                    c_path.as_ptr(),
                    libc::RTLD_LAZY | libc::RTLD_GLOBAL | libc::RTLD_NODELETE,
                )
            };
            if handle.is_null() {
                let err = unsafe { std::ffi::CStr::from_ptr(libc::dlerror()) };
                eprintln!("[WGPU] preload_egl: dlopen {:?} failed: {:?}", path, err);
            } else {
                eprintln!(
                    "[WGPU] preload_egl: loaded {:?} (soname registered in glibc cache)",
                    path
                );
                // Don't dlclose — RTLD_NODELETE keeps it alive anyway.
                return;
            }
        }

        eprintln!(
            "[WGPU] preload_egl: WARNING — could not load libEGL.so.1 from {:?}",
            dir
        );
    }

    /// Whether the surface was successfully configured and is ready for rendering.
    pub fn is_configured(&self) -> bool {
        self.configured
    }

    /// Try to configure the surface once (non-blocking). Returns true on success.
    /// Call this each frame until it succeeds — the X11 event loop stays alive
    /// between frames so Vulkan can process the events it needs.
    pub fn try_configure(&mut self) -> bool {
        if self.configured {
            return true;
        }
        self.configured = Self::do_configure(&self.surface, &self.device, &self.config);
        if self.configured {
            eprintln!("[WGPU] Surface configured successfully (deferred)");
        }
        self.configured
    }

    /// Resize the surface. No-op if the surface isn't configured yet.
    pub fn resize(&mut self, width: u32, height: u32) {
        if !self.configured || width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.configured = Self::do_configure(&self.surface, &self.device, &self.config);
    }

    /// Configure the surface, capturing any validation error via error scope.
    /// Returns true on success, false on failure (no panic).
    fn do_configure(
        surface: &wgpu::Surface<'_>,
        device: &wgpu::Device,
        config: &wgpu::SurfaceConfiguration,
    ) -> bool {
        // wgpu 28: push_error_scope returns an ErrorScopeGuard you `pop()`
        // (instead of the device having a standalone pop method).
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        surface.configure(device, config);
        let error = scope.pop().block_on();
        if let Some(e) = error {
            eprintln!("[WGPU] Surface configure failed: {:?}", e);
            false
        } else {
            true
        }
    }

    /// Get the surface format.
    pub fn format(&self) -> wgpu::TextureFormat {
        self.config.format
    }
}

/// Wrapper to provide raw-window-handle 0.6 traits from raw handles.
struct RawHandleWrapper {
    window: RawWindowHandle,
    display: RawDisplayHandle,
}

impl HasWindowHandle for RawHandleWrapper {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        // SAFETY: The handles are valid for the lifetime of the window
        Ok(unsafe { WindowHandle::borrow_raw(self.window) })
    }
}

impl HasDisplayHandle for RawHandleWrapper {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        // SAFETY: The handles are valid for the lifetime of the window
        Ok(unsafe { DisplayHandle::borrow_raw(self.display) })
    }
}
