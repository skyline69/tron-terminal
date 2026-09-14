//! Glyph texture atlas with shelf packing.

const PADDING: u32 = 1;

struct Shelf {
    y: u32,
    height: u32,
    x: u32,
}

pub struct Atlas {
    label: &'static str,
    format: wgpu::TextureFormat,
    size: u32,
    shelves: Vec<Shelf>,
    next_y: u32,
    texture: wgpu::Texture,
    pub view: wgpu::TextureView,
}

impl Atlas {
    pub fn new(device: &wgpu::Device, label: &'static str, format: wgpu::TextureFormat, size: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width: size, height: size, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self { label, format, size, shelves: Vec::new(), next_y: 0, texture, view }
    }

    pub fn size(&self) -> u32 {
        self.size
    }

    /// Replaces the texture with an empty one of `size`.
    pub fn reset(&mut self, device: &wgpu::Device, size: u32) {
        *self = Self::new(device, self.label, self.format, size);
    }

    /// Finds space for a `width` x `height` bitmap.
    pub fn allocate(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        let (w, h) = (width + PADDING, height + PADDING);
        if w > self.size || h > self.size {
            return None;
        }
        let size = self.size;
        let best = self
            .shelves
            .iter_mut()
            .filter(|s| s.height >= h && s.height <= h + h / 2 + 2 && s.x + w <= size)
            .min_by_key(|s| s.height);
        if let Some(shelf) = best {
            let position = (shelf.x, shelf.y);
            shelf.x += w;
            return Some(position);
        }
        if self.next_y + h > self.size {
            return None;
        }
        let position = (0, self.next_y);
        self.shelves.push(Shelf { y: self.next_y, height: h, x: w });
        self.next_y += h;
        Some(position)
    }

    pub fn upload(&self, queue: &wgpu::Queue, x: u32, y: u32, width: u32, height: u32, data: &[u8]) {
        let bytes_per_pixel = self.format.block_copy_size(None).unwrap_or(1);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * bytes_per_pixel),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
    }
}
