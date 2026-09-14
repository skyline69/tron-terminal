//! Measures wgpu initialization time per backend.

use std::time::Instant;

fn main() {
    for (name, backends) in [("vulkan", wgpu::Backends::VULKAN), ("gl", wgpu::Backends::GL)] {
        let start = Instant::now();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let instance_time = start.elapsed();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()));
        let adapter_time = start.elapsed();
        let Ok(adapter) = adapter else {
            println!("{name}: no adapter after {adapter_time:?}");
            continue;
        };
        let device = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()));
        let device_time = start.elapsed();
        println!(
            "{name}: instance {instance_time:?}, adapter {adapter_time:?}, device {device_time:?} ({}, ok: {})",
            adapter.get_info().name,
            device.is_ok()
        );
    }
}
