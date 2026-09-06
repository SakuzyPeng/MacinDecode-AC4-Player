//! The only file in `scene3d` that touches `wgpu`.
//!
//! Everything above it works in plain arrays so it stays unit testable in a
//! headless environment. MSAA and depth cover only the object scene, whose
//! resolved colour is composited into egui's single-sample render pass.

use std::ops::Range;

use eframe::egui_wgpu::{
    self, CallbackResources, CallbackTrait, RenderState, ScreenDescriptor, wgpu,
};

use super::camera::Matrix4;
use super::mesh::{MeshBuilder, Vertex};

const MSAA_SAMPLES: u32 = 4;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24Plus;

const INITIAL_VERTEX_CAPACITY: u64 = 4096;

/// Pipelines and buffers, stored in egui's cross-frame resource map.
pub struct SceneRenderer {
    solid: wgpu::RenderPipeline,
    line: wgpu::RenderPipeline,
    decal: wgpu::RenderPipeline,
    uniforms: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    vertices: wgpu::Buffer,
    vertex_capacity: u64,
    composite: wgpu::RenderPipeline,
    composite_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    target_format: wgpu::TextureFormat,
    target: Option<SceneTarget>,
}

struct SceneTarget {
    size: [u32; 2],
    multisampled: wgpu::TextureView,
    depth: wgpu::TextureView,
    resolved: wgpu::TextureView,
    composite: wgpu::BindGroup,
}

impl SceneRenderer {
    /// Build the pipelines once and park them where the paint callback can find
    /// them. Returns `false` when eframe is not running on the wgpu backend, in
    /// which case the scene simply does not draw.
    pub fn install(render_state: &RenderState) -> bool {
        render_state
            .renderer
            .write()
            .callback_resources
            .insert(Self::new(&render_state.device, render_state.target_format));
        true
    }

    fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("scene3d_shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene3d_uniforms_layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scene3d_uniforms"),
            size: std::mem::size_of::<Matrix4>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene3d_uniforms_bind_group"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniforms.as_entire_binding(),
            }],
        });

        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scene3d_pipeline_layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });

        let build = |label: &str, cull: Option<wgpu::Face>, bias: wgpu::DepthBiasState| {
            build_pipeline(device, &layout, &module, target_format, label, cull, bias)
        };

        let composite_layout = composite_bind_group_layout(device);
        let composite = composite_pipeline(device, &composite_layout, target_format);
        Self {
            solid: build(
                "scene3d_solid",
                Some(wgpu::Face::Back),
                wgpu::DepthBiasState::default(),
            ),
            // Culling must be off for both of these. A camera-facing line quad's
            // winding flips with the view direction, and a floor decal only ever
            // faces one way.
            line: build("scene3d_line", None, wgpu::DepthBiasState::default()),
            decal: build(
                "scene3d_decal",
                None,
                // Floor geometry is all coplanar at y = -1, so without a bias it
                // z-fights. The slope term matters as much as the constant: a
                // free camera can graze the floor plane, where a constant-only
                // bias stops being enough.
                wgpu::DepthBiasState {
                    constant: -2,
                    slope_scale: -1.0,
                    clamp: 0.0,
                },
            ),
            uniforms,
            bind_group,
            vertices: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("scene3d_vertices"),
                size: INITIAL_VERTEX_CAPACITY * std::mem::size_of::<Vertex>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            vertex_capacity: INITIAL_VERTEX_CAPACITY,
            composite,
            composite_layout,
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("scene3d_composite_sampler"),
                ..Default::default()
            }),
            target_format,
            target: None,
        }
    }

    fn reserve(&mut self, device: &wgpu::Device, vertices: u64) {
        if vertices <= self.vertex_capacity {
            return;
        }
        let capacity = vertices.next_power_of_two();
        self.vertices = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scene3d_vertices"),
            size: capacity * std::mem::size_of::<Vertex>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.vertex_capacity = capacity;
    }

    fn resize_target(&mut self, device: &wgpu::Device, size: [u32; 2]) {
        if self
            .target
            .as_ref()
            .is_some_and(|target| target.size == size)
        {
            return;
        }
        let texture = |label, format, sample_count, usage| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: size[0],
                        height: size[1],
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let resolved = texture(
            "scene3d_resolved",
            self.target_format,
            1,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let composite = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene3d_composite"),
            layout: &self.composite_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&resolved),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        self.target = Some(SceneTarget {
            size,
            multisampled: texture(
                "scene3d_msaa",
                self.target_format,
                MSAA_SAMPLES,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TRANSIENT_ATTACHMENT,
            ),
            depth: texture(
                "scene3d_depth",
                DEPTH_FORMAT,
                MSAA_SAMPLES,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TRANSIENT_ATTACHMENT,
            ),
            resolved,
            composite,
        });
    }
}

fn composite_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("scene3d_composite_layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    })
}

fn composite_pipeline(
    device: &wgpu::Device,
    bind_group_layout: &wgpu::BindGroupLayout,
    target_format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("scene3d_composite_shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("composite.wgsl").into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("scene3d_composite_pipeline_layout"),
        bind_group_layouts: &[Some(bind_group_layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("scene3d_composite_pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vertex_main"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fragment_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// The three scene pipelines share the scene target's format and sample count;
/// they differ only in face culling and depth bias.
#[allow(
    clippy::too_many_arguments,
    reason = "a pipeline descriptor's inputs do not group into meaningful structs"
)]
fn build_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    module: &wgpu::ShaderModule,
    target_format: wgpu::TextureFormat,
    label: &str,
    cull: Option<wgpu::Face>,
    bias: wgpu::DepthBiasState,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vertex_main"),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Vertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Unorm8x4],
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: cull,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias,
        }),
        multisample: wgpu::MultisampleState {
            count: MSAA_SAMPLES,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(if target_format.is_srgb() {
                "fragment_main_srgb"
            } else {
                "fragment_main"
            }),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                // Every colour in this scene is opaque: the flat paper
                // theme expresses fading by lerping toward STAGE, not
                // with alpha. That keeps the depth buffer sufficient and
                // leaves no transparency ordering to solve.
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// One frame's geometry, handed to egui as a paint callback.
pub struct SceneCallback {
    rect: eframe::egui::Rect,
    view_projection: Matrix4,
    vertices: Vec<Vertex>,
    solid: Range<u32>,
    line: Range<u32>,
    decal: Range<u32>,
}

impl SceneCallback {
    /// Flatten the three streams into one buffer, remembering where each starts.
    ///
    /// The ranges are computed here rather than in `prepare` because `paint`
    /// only gets `&self`, and they are just the stream lengths anyway.
    #[must_use]
    pub fn new(mesh: &MeshBuilder, view_projection: Matrix4) -> Self {
        let solid = to_range(0, mesh.solid.len());
        let line = to_range(solid.end, mesh.line.len());
        let decal = to_range(line.end, mesh.decal.len());

        let mut vertices =
            Vec::with_capacity(mesh.solid.len() + mesh.line.len() + mesh.decal.len());
        vertices.extend_from_slice(&mesh.solid);
        vertices.extend_from_slice(&mesh.line);
        vertices.extend_from_slice(&mesh.decal);

        Self {
            rect: eframe::egui::Rect::NOTHING,
            view_projection,
            vertices,
            solid,
            line,
            decal,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.vertices.is_empty()
    }

    /// Wrap into an egui paint callback covering `rect`.
    pub fn into_shape(mut self, rect: eframe::egui::Rect) -> eframe::egui::Shape {
        self.rect = rect;
        eframe::egui::Shape::Callback(egui_wgpu::Callback::new_paint_callback(rect, self))
    }
}

fn target_size(rect: eframe::egui::Rect, screen: &ScreenDescriptor, limit: u32) -> [u32; 2] {
    // Match egui's rounding/clipping of the callback viewport, including
    // fractional display scaling, so the composite maps one texel per pixel.
    let viewport = eframe::egui::epaint::ViewportInPixels::from_points(
        &rect,
        screen.pixels_per_point,
        screen.size_in_pixels,
    );
    [viewport.width_px, viewport.height_px].map(|pixels| {
        u32::try_from(pixels.max(1))
            .expect("positive viewport dimension")
            .min(limit)
    })
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "the scene is bounded well below u32 vertices; see MAX_VIEW_OBJECTS"
)]
fn to_range(start: u32, len: usize) -> Range<u32> {
    start..start + len as u32
}

impl CallbackTrait for SceneCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen_descriptor: &ScreenDescriptor,
        encoder: &mut wgpu::CommandEncoder,
        resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(renderer) = resources.get_mut::<SceneRenderer>() {
            renderer.reserve(device, self.vertices.len() as u64);
            renderer.resize_target(
                device,
                target_size(
                    self.rect,
                    screen_descriptor,
                    device.limits().max_texture_dimension_2d,
                ),
            );
            queue.write_buffer(
                &renderer.uniforms,
                0,
                bytemuck::cast_slice(&self.view_projection),
            );
            if !self.vertices.is_empty() {
                queue.write_buffer(&renderer.vertices, 0, bytemuck::cast_slice(&self.vertices));
            }
            let target = renderer.target.as_ref().expect("scene target was prepared");
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene3d_offscreen_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.multisampled,
                    depth_slice: None,
                    resolve_target: Some(&target.resolved),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Discard,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &target.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
            pass.set_bind_group(0, &renderer.bind_group, &[]);
            pass.set_vertex_buffer(0, renderer.vertices.slice(..));
            for (pipeline, range) in [
                (&renderer.solid, &self.solid),
                (&renderer.line, &self.line),
                (&renderer.decal, &self.decal),
            ] {
                if !range.is_empty() {
                    pass.set_pipeline(pipeline);
                    pass.draw(range.clone(), 0..1);
                }
            }
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: eframe::egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &CallbackResources,
    ) {
        let Some(renderer) = resources.get::<SceneRenderer>() else {
            return;
        };
        if let Some(target) = &renderer.target {
            render_pass.set_pipeline(&renderer.composite);
            render_pass.set_bind_group(0, &target.composite, &[]);
            render_pass.draw(0..3, 0..1);
        }
    }
}

#[cfg(test)]
mod rendering_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene3d::mesh::{Layer, Rgb, ViewContext};

    fn view() -> ViewContext {
        ViewContext {
            direction: [0.577, 0.577, 0.577],
            degeneracy: 0.0,
            world_units_per_point: 0.01,
            ink: Rgb::from_color32(eframe::egui::Color32::from_rgb(51, 42, 31)),
            stage: Rgb::from_color32(eframe::egui::Color32::from_rgb(248, 243, 234)),
        }
    }

    #[test]
    fn scene_targets_follow_the_physical_viewport_and_clip_to_the_screen() {
        use eframe::egui::{Rect, pos2};
        let screen = ScreenDescriptor {
            size_in_pixels: [1920, 1080],
            pixels_per_point: 1.5,
        };
        assert_eq!(
            target_size(
                Rect::from_min_max(pos2(100.25, 200.25), pos2(500.75, 400.75)),
                &screen,
                8192
            ),
            [601, 301]
        );
        assert_eq!(
            target_size(
                Rect::from_min_max(pos2(1200.0, 700.0), pos2(1300.0, 750.0)),
                &screen,
                8192
            ),
            [120, 30]
        );
        assert_eq!(target_size(Rect::ZERO, &screen, 8192), [1, 1]);
        assert_eq!(
            target_size(
                Rect::from_min_max(pos2(0.0, 0.0), pos2(900.0, 500.0)),
                &screen,
                1024
            ),
            [1024, 750]
        );
    }

    #[test]
    fn the_three_streams_are_concatenated_into_disjoint_draw_ranges() {
        let mut mesh = MeshBuilder::default();
        let context = view();
        mesh.add_box(
            [0.0; 3],
            [0.1; 3],
            Rgb::from_color32(eframe::egui::Color32::from_rgb(206, 122, 59)),
            &context,
        );
        mesh.add_line(
            Layer::Line,
            [0.0; 3],
            [1.0, 0.0, 0.0],
            Rgb::from_color32(eframe::egui::Color32::from_rgb(154, 139, 118)),
            1.0,
            &context,
        );
        mesh.add_floor_mark(
            0.0,
            0.0,
            0.1,
            -1.0,
            Rgb::from_color32(eframe::egui::Color32::from_rgb(154, 139, 118)),
            &context,
        );

        let callback = SceneCallback::new(&mesh, [0.0; 16]);
        assert_eq!(callback.solid, 0..36);
        assert_eq!(callback.line, 36..42);
        assert_eq!(callback.decal, 42..48);
        assert_eq!(callback.vertices.len(), 48);
    }

    #[test]
    fn an_empty_scene_produces_no_draws() {
        let callback = SceneCallback::new(&MeshBuilder::default(), [0.0; 16]);
        assert!(callback.is_empty());
        assert!(callback.solid.is_empty() && callback.line.is_empty() && callback.decal.is_empty());
    }
}
