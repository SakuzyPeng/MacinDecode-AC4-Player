// Object scene shader.
//
// Deliberately dumb: one matrix multiply and a passthrough. All shading —
// three-tone faces and aerial perspective — happens on the CPU in
// `scene3d::mesh`, because the scene is rebuilt every frame anyway and keeping
// the maths in Rust keeps every visual parameter reachable from a unit test.

struct Uniforms {
    view_projection: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> uniforms: Uniforms;

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) colour: vec4<f32>,
};

@vertex
fn vertex_main(
    @location(0) position: vec3<f32>,
    @location(1) colour: vec4<f32>,
) -> VertexOutput {
    var out: VertexOutput;
    out.clip_position = uniforms.view_projection * vec4<f32>(position, 1.0);
    out.colour = colour;
    return out;
}

// Preferred path: a plain Unorm target takes the theme's sRGB bytes unchanged.
@fragment
fn fragment_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.colour;
}

// Fallback for an sRGB-aware target, which encodes on write. Undo that first so
// the pixel still lands on the theme colour it was authored as.
@fragment
fn fragment_main_srgb(in: VertexOutput) -> @location(0) vec4<f32> {
    let lower = in.colour.rgb / 12.92;
    let higher = pow((in.colour.rgb + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    let linear = select(higher, lower, in.colour.rgb <= vec3<f32>(0.04045));
    return vec4<f32>(linear, in.colour.a);
}
