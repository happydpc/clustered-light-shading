#![allow(unused)]

use crate::*;

pub struct MainResources {
    pub dimensions: Vector2<i32>,
    pub sample_count: u32,

    // Main frame resources.
    pub framebuffer_name: gl::NonDefaultFramebufferName,
    pub color_texture: gl::TextureName,
    pub depth_texture: gl::TextureName,

    // Profiling
    pub depth_pass_profiler: SampleIndex,
    pub basic_pass_profiler: SampleIndex,
}

impl MainResources {
    pub fn new(
        gl: &gl::Gl,
        profiling_context: &mut ProfilingContext,
        dimensions: Vector2<i32>,
        sample_count: u32,
    ) -> Self {
        unsafe {
            let color_texture = if sample_count == 1 {
                let color_texture = gl.create_texture(gl::TEXTURE_2D);
                gl.texture_storage_2d(color_texture, 1, gl::RGBA16F, dimensions.x, dimensions.y);
                color_texture
            } else {
                let color_texture = gl.create_texture(gl::TEXTURE_2D_MULTISAMPLE);
                gl.bind_texture(gl::TEXTURE_2D_MULTISAMPLE, color_texture);
                gl.tex_image_2d_multisample(
                    gl::TEXTURE_2D_MULTISAMPLE,
                    sample_count as i32,
                    gl::RGBA16F,
                    dimensions.x,
                    dimensions.y,
                    false,
                );

                color_texture
            };

            let depth_texture = if sample_count == 1 {
                let depth_texture = gl.create_texture(gl::TEXTURE_2D);
                gl.texture_storage_2d(depth_texture, 1, gl::DEPTH24_STENCIL8, dimensions.x, dimensions.y);
                depth_texture
            } else {
                let depth_texture = gl.create_texture(gl::TEXTURE_2D_MULTISAMPLE);
                gl.bind_texture(gl::TEXTURE_2D_MULTISAMPLE, depth_texture);
                gl.tex_image_2d_multisample(
                    gl::TEXTURE_2D_MULTISAMPLE,
                    sample_count as i32,
                    gl::DEPTH24_STENCIL8,
                    dimensions.x,
                    dimensions.y,
                    false,
                );
                depth_texture
            };

            // Framebuffers.

            let framebuffer_name = create_framebuffer!(
                gl,
                (gl::DEPTH_STENCIL_ATTACHMENT, depth_texture),
                (gl::COLOR_ATTACHMENT0, color_texture),
            );

            MainResources {
                dimensions,
                sample_count,

                framebuffer_name,
                color_texture,
                depth_texture,

                depth_pass_profiler: profiling_context.add_sample("main_depth"),
                basic_pass_profiler: profiling_context.add_sample("main_basic"),
            }
        }
    }

    pub fn reset(
        &mut self,
        gl: &gl::Gl,
        _profiling_context: &mut ProfilingContext,
        dimensions: Vector2<i32>,
        sample_count: u32,
    ) {
        if self.dimensions != dimensions || self.sample_count != sample_count {
            self.dimensions = dimensions;
            self.sample_count = sample_count;

            unsafe {
                let depth_texture = if sample_count == 1 {
                    let depth_texture = gl.create_texture(gl::TEXTURE_2D);
                    gl.texture_storage_2d(depth_texture, 1, gl::DEPTH24_STENCIL8, dimensions.x, dimensions.y);
                    depth_texture
                } else {
                    let depth_texture = gl.create_texture(gl::TEXTURE_2D_MULTISAMPLE);
                    gl.bind_texture(gl::TEXTURE_2D_MULTISAMPLE, depth_texture);
                    gl.tex_image_2d_multisample(
                        gl::TEXTURE_2D_MULTISAMPLE,
                        sample_count as i32,
                        gl::DEPTH24_STENCIL8,
                        dimensions.x,
                        dimensions.y,
                        false,
                    );
                    depth_texture
                };

                let color_texture = if sample_count == 1 {
                    let color_texture = gl.create_texture(gl::TEXTURE_2D);
                    gl.texture_storage_2d(color_texture, 1, gl::RGBA16F, dimensions.x, dimensions.y);
                    color_texture
                } else {
                    let color_texture = gl.create_texture(gl::TEXTURE_2D_MULTISAMPLE);
                    gl.bind_texture(gl::TEXTURE_2D_MULTISAMPLE, color_texture);
                    gl.tex_image_2d_multisample(
                        gl::TEXTURE_2D_MULTISAMPLE,
                        sample_count as i32,
                        gl::RGBA16F,
                        dimensions.x,
                        dimensions.y,
                        false,
                    );

                    color_texture
                };

                gl.named_framebuffer_texture(self.framebuffer_name, gl::COLOR_ATTACHMENT0, color_texture, 0);
                gl.named_framebuffer_texture(
                    self.framebuffer_name,
                    gl::DEPTH_STENCIL_ATTACHMENT,
                    depth_texture,
                    0,
                );

                gl.delete_texture(self.depth_texture);
                gl.delete_texture(self.color_texture);

                self.color_texture = color_texture;
                self.depth_texture = depth_texture;
            }
        }
    }

    pub fn drop(self, gl: &gl::Gl) {
        unsafe {
            gl.delete_framebuffer(self.framebuffer_name);
            gl.delete_texture(self.color_texture);
            gl.delete_texture(self.depth_texture);
        }
    }
}

impl_frame_pool! {
    MainResourcesPool,
    MainResources,
    MainResourcesIndex,
    MainResourcesIndexIter,
    (gl: &gl::Gl, profiling_context: &mut ProfilingContext, dimensions: Vector2<i32>, sample_count: u32),
}
