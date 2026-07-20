//! Offscreen WGPU rendering with softbuffer blitting.
//!
//! This module provides GPU rendering via wgpu to an offscreen texture,
//! then blits the result to the window using softbuffer. This approach
//! works on Linux/XWayland where wgpu surface creation fails.

use pollster::FutureExt;
use std::num::NonZeroU32;
use std::sync::Arc;

/// Holds the WGPU instance, device, queue for offscreen rendering.
pub struct WgpuOffscreenState {
    pub instance: wgpu::Instance,
    pub device: Arc<wgpu::Device>,
    pub queue: Arc<wgpu::Queue>,
    pub render_texture: wgpu::Texture,
    pub render_texture_view: wgpu::TextureView,
    pub staging_buffer: wgpu::Buffer,
    pub format: wgpu::TextureFormat,
    pub width: u32,
    pub height: u32,
}

impl WgpuOffscreenState {
    /// Create a new offscreen WGPU state.
    pub fn new(width: u32, height: u32) -> Self {
        let width = width.max(1);
        let height = height.max(1);

        // Create the instance - no surface needed!
        // wgpu 29: `InstanceDescriptor` no longer implements `Default` and `Instance::new`
        // takes it by value.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            flags: wgpu::InstanceFlags::default(),
            memory_budget_thresholds: Default::default(),
            backend_options: Default::default(),
            display: None,
        });

        // Request an adapter (no surface compatibility needed)
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                force_fallback_adapter: false,
                compatible_surface: None, // No surface!
            })
            .block_on()
            .expect("Failed to find an appropriate adapter");

        nice_plug_core::nice_log!("[WGPU Offscreen] Adapter: {:?}", adapter.get_info());

        // Create the device and queue
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("nice_plug_dioxus offscreen device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::Off,
                experimental_features: wgpu::ExperimentalFeatures::default(),
            })
            .block_on()
            .expect("Failed to create device");

        let device = Arc::new(device);
        let queue = Arc::new(queue);

        // Use BGRA8 format for compatibility with softbuffer (which expects XRGB/ARGB)
        let format = wgpu::TextureFormat::Bgra8Unorm;

        // Create the offscreen render texture
        let render_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen render texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        let render_texture_view =
            render_texture.create_view(&wgpu::TextureViewDescriptor::default());

        // Create staging buffer for reading back pixels
        // Must account for row alignment padding (COPY_BYTES_PER_ROW_ALIGNMENT = 256)
        let bytes_per_pixel = 4u32;
        let unpadded_bytes_per_row = width * bytes_per_pixel;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = (unpadded_bytes_per_row + align - 1) / align * align;
        let buffer_size = (padded_bytes_per_row * height) as u64;
        let staging_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("staging buffer"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        nice_plug_core::nice_log!(
            "[WGPU Offscreen] Created {}x{} offscreen renderer",
            width,
            height
        );

        Self {
            instance,
            device,
            queue,
            render_texture,
            render_texture_view,
            staging_buffer,
            format,
            width,
            height,
        }
    }

    /// Resize the offscreen render target.
    pub fn resize(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);

        if width == self.width && height == self.height {
            return;
        }

        // Recreate the render texture
        self.render_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen render texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        self.render_texture_view = self
            .render_texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // Recreate staging buffer with row alignment padding
        let bytes_per_pixel = 4u32;
        let unpadded_bytes_per_row = width * bytes_per_pixel;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = (unpadded_bytes_per_row + align - 1) / align * align;
        let buffer_size = (padded_bytes_per_row * height) as u64;
        self.staging_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("staging buffer"),
            size: buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        self.width = width;
        self.height = height;

        nice_plug_core::nice_log!("[WGPU Offscreen] Resized to {}x{}", width, height);
    }

    /// Get the surface format.
    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// Copy the render texture to the staging buffer and read it back.
    /// Returns the pixel data as BGRA8.
    pub fn read_pixels(&self) -> Vec<u8> {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("readback encoder"),
            });

        // Calculate bytes per row with alignment
        let bytes_per_pixel = 4u32;
        let unpadded_bytes_per_row = self.width * bytes_per_pixel;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = (unpadded_bytes_per_row + align - 1) / align * align;

        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.render_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.staging_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );

        self.queue.submit(std::iter::once(encoder.finish()));

        // Map the buffer and read the data
        let buffer_slice = self.staging_buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
            tx.send(result).unwrap();
        });
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv().unwrap().unwrap();

        let data = buffer_slice.get_mapped_range();

        // Remove padding if necessary
        let result = if padded_bytes_per_row != unpadded_bytes_per_row {
            let mut unpadded = Vec::with_capacity((self.width * self.height * 4) as usize);
            for y in 0..self.height {
                let start = (y * padded_bytes_per_row) as usize;
                let end = start + unpadded_bytes_per_row as usize;
                unpadded.extend_from_slice(&data[start..end]);
            }
            unpadded
        } else {
            data.to_vec()
        };

        drop(data);
        self.staging_buffer.unmap();

        result
    }

    /// Convert BGRA8 pixel data to the format expected by softbuffer (0xAARRGGBB or 0x00RRGGBB).
    pub fn bgra_to_softbuffer(bgra: &[u8], width: u32, height: u32) -> Vec<u32> {
        let mut result = Vec::with_capacity((width * height) as usize);
        for chunk in bgra.chunks_exact(4) {
            let b = chunk[0] as u32;
            let g = chunk[1] as u32;
            let r = chunk[2] as u32;
            let a = chunk[3] as u32;
            // softbuffer expects 0xAARRGGBB or 0x00RRGGBB
            result.push((a << 24) | (r << 16) | (g << 8) | b);
        }
        result
    }
}
