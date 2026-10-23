//! wgpu instance/adapter/device setup, failing loudly when no usable GPU exists.

use anyhow::{Context, Result, bail};

/// Everything the renderer needs from the GPU stack.
pub struct Gpu {
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub info: wgpu::AdapterInfo,
    /// Whether per-frame GPU timestamps are available.
    pub timestamps: bool,
}

pub fn new_instance() -> wgpu::Instance {
    wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle())
}

fn describe(info: &wgpu::AdapterInfo) -> String {
    format!(
        "{} [{:?}, {:?}] driver={:?} ({})",
        info.name, info.backend, info.device_type, info.driver, info.driver_info
    )
}

/// Pick an adapter (compatible with `surface` when given) and open a device on it.
pub fn create(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'_>>,
    low_power: bool,
) -> Result<Gpu> {
    let available = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
    for a in &available {
        tracing::info!(adapter = %describe(&a.get_info()), "adapter available");
    }
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: if low_power {
            wgpu::PowerPreference::LowPower
        } else {
            wgpu::PowerPreference::HighPerformance
        },
        compatible_surface: surface,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .map_err(|e| {
        let list: Vec<_> = available.iter().map(|a| describe(&a.get_info())).collect();
        anyhow::anyhow!("no suitable GPU adapter ({e}); enumerated adapters: {list:?}")
    })?;

    let info = adapter.get_info();
    if info.device_type == wgpu::DeviceType::Cpu {
        bail!(
            "only a software adapter is available ({}); refusing to run the GPU spike on it",
            describe(&info)
        );
    }
    tracing::info!(adapter = %describe(&info), "adapter selected");

    let timestamps = adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY);
    if !timestamps {
        tracing::warn!(
            "adapter lacks TIMESTAMP_QUERY: GPU frame time will be unavailable, wall-clock only"
        );
    }
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("mctl-gfx device"),
        required_features: if timestamps {
            wgpu::Features::TIMESTAMP_QUERY
        } else {
            wgpu::Features::empty()
        },
        required_limits: wgpu::Limits::default(),
        ..Default::default()
    }))
    .with_context(|| format!("opening device on {}", describe(&info)))?;
    device.on_uncaptured_error(std::sync::Arc::new(|e| {
        // A validation error means a bug in the renderer; never continue past one silently.
        panic!("wgpu uncaptured error: {e}");
    }));
    Ok(Gpu {
        adapter,
        device,
        queue,
        info,
        timestamps,
    })
}
