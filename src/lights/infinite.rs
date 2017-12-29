extern crate image;

// std
use std;
use std::f32::consts::PI;
use std::io::BufReader;
use std::sync::{Arc, RwLock};
// others
#[cfg(feature="openexr")]
use half::f16;
#[cfg(feature="openexr")]
use openexr::{FrameBufferMut, InputFile, PixelType};
// pbrt
use core::geometry::{Bounds3f, Normal3f, Point2i, Point2f, Point3f, Ray, Vector3f};
use core::geometry::{spherical_phi, spherical_theta, vec3_normalize};
use core::interaction::{Interaction, InteractionCommon};
use core::mipmap::{ImageWrap, MipMap};
use core::light::{Light, LightFlags, VisibilityTester};
use core::pbrt::{INV_2_PI, INV_PI};
use core::pbrt::{Float, Spectrum};
use core::sampling::Distribution2D;
use core::scene::Scene;
use core::transform::Transform;

// see https://stackoverflow.com/questions/36008434/how-can-i-decode-f16-to-f32-using-only-the-stable-standard-library
#[inline]
#[cfg(feature="openexr")]
fn decode_f16(half: u16) -> f32 {
    let exp: u16 = half >> 10 & 0x1f;
    let mant: u16 = half & 0x3ff;
    let val: f32 = if exp == 0 {
        (mant as f32) * (2.0f32).powi(-24)
    } else if exp != 31 {
        (mant as f32 + 1024f32) * (2.0f32).powi(exp as i32 - 25)
    } else if mant == 0 {
        ::std::f32::INFINITY
    } else {
        ::std::f32::NAN
    };
    if half & 0x8000 != 0 {
        -val
    } else {
        val
    }
}

// see infinte.h

pub struct InfiniteAreaLight {
    // private data (see infinte.h)
    pub lmap: Arc<MipMap>,
    pub world_center: RwLock<Point3f>,
    pub world_radius: RwLock<Float>,
    pub distribution: Arc<Distribution2D>,
    // inherited from class Light (see light.h)
    flags: u8,
    n_samples: i32,
    // TODO: const MediumInterface mediumInterface;
    light_to_world: Transform,
    world_to_light: Transform,
}

impl InfiniteAreaLight {
    #[cfg(not(feature="openexr"))]
    pub fn new(light_to_world: &Transform, l: &Spectrum, n_samples: i32, texmap: String) -> Self {
        InfiniteAreaLight::new_hdr(light_to_world, l, n_samples, texmap)
    }
    #[cfg(feature="openexr")]
    pub fn new(light_to_world: &Transform, l: &Spectrum, n_samples: i32, texmap: String) -> Self {
        // read texel data from _texmap_ and initialize _Lmap_
        if texmap != String::from("") {
            // https://cessen.github.io/openexr-rs/openexr/index.html
            let mut resolution: Point2i = Point2i::default();
            let mut names_and_fills: Vec<(&str, f64)> = Vec::new();
            // header
            let file_result = std::fs::File::open(texmap.clone());
            if file_result.is_ok() {
                let mut file = file_result.unwrap();
                let input_file_result = InputFile::new(&mut file);
                if input_file_result.is_ok() {
                    let input_file = input_file_result.unwrap();
                    // get resolution
                    let (width, height) = input_file.header().data_dimensions();
                    resolution.x = width as i32;
                    resolution.y = height as i32;
                    println!("resolution = {:?}", resolution);
                    // make sure the image properties are the same (see incremental_io.rs in github/openexr-rs)
                    for channel_name in ["R", "G", "B"].iter() {
                        let channel =
                            input_file
                                .header()
                                .get_channel(channel_name)
                                .expect(&format!("Didn't find channel {}.", channel_name));
                        assert!(channel.pixel_type == PixelType::HALF);
                        names_and_fills.push((channel_name, 0.0_f64));
                    }
                    let mut pixel_data = vec![(f16::from_f32(0.0), f16::from_f32(0.0), f16::from_f32(0.0)); (resolution.x*resolution.y) as usize];
                    {
                        // read pixels
                        let mut file = std::fs::File::open(texmap.clone()).unwrap();
                        let mut input_file = InputFile::new(&mut file).unwrap();
                        let mut fb = FrameBufferMut::new(resolution.x as u32, resolution.y as u32);
                        fb.insert_channels(&names_and_fills[..], &mut pixel_data);
                        input_file.read_pixels(&mut fb).unwrap();
                    }
                    // convert pixel data into Vec<Spectrum> (and on the way multiply by _l_)
                    let mut texels: Vec<Spectrum> = Vec::new();
                    for i in 0..(resolution.x * resolution.y) {
                        let (r, g, b) = pixel_data[i as usize];
                        texels.push(Spectrum::rgb(decode_f16(r.as_bits()),
                                                  decode_f16(g.as_bits()),
                                                  decode_f16(b.as_bits())) *
                                    *l);
                    }
                    // create _MipMap_ from converted texels (see above)
                    let do_trilinear: bool = false;
                    let max_aniso: Float = 8.0 as Float;
                    let wrap_mode: ImageWrap = ImageWrap::Repeat;
                    let lmap = Arc::new(MipMap::new(&resolution,
                                                    &texels[..],
                                                    do_trilinear,
                                                    max_aniso,
                                                    wrap_mode));

                    // initialize sampling PDFs for infinite area light

                    // compute scalar-valued image _img_ from environment map
                    let width: i32 = 2_i32 * lmap.width();
                    let height: i32 = 2_i32 * lmap.height();
                    let mut img: Vec<Float> = Vec::new();
                    let fwidth: Float = 0.5 as Float / (width as Float).min(height as Float);
                    // TODO: ParallelFor(...) {...}
                    for v in 0..height {
                        let vp: Float = (v as Float + 0.5 as Float) / height as Float;
                        let sin_theta: Float = (PI * (v as Float + 0.5 as Float) / height as Float)
                            .sin();
                        for u in 0..width {
                            let up: Float = (u as Float + 0.5 as Float) / width as Float;
                            let st: Point2f = Point2f { x: up, y: vp };
                            img.push(lmap.lookup_pnt_flt(&st, fwidth).y() * sin_theta);
                        }
                    }
                    let distribution: Arc<Distribution2D> =
                        Arc::new(Distribution2D::new(img, width, height));
                    InfiniteAreaLight {
                        lmap: lmap,
                        world_center: RwLock::new(Point3f::default()),
                        world_radius: RwLock::new(0.0),
                        distribution: distribution,
                        flags: LightFlags::Infinite as u8,
                        n_samples: std::cmp::max(1_i32, n_samples),
                        light_to_world: *light_to_world,
                        world_to_light: Transform::inverse(*light_to_world),
                    }
                } else {
                    // try to open an HDR image instead (TODO: check extension upfront)
                    InfiniteAreaLight::new_hdr(light_to_world, l, n_samples, texmap)
                }
            } else {
                // try to open an HDR image instead (TODO: check extension upfront)
                InfiniteAreaLight::new_hdr(light_to_world, l, n_samples, texmap)
            }
        } else {
            InfiniteAreaLight::default(n_samples, l)
        }
    }
    pub fn new_hdr(light_to_world: &Transform,
                   l: &Spectrum,
                   n_samples: i32,
                   texmap: String)
                   -> Self {
        // read texel data from _texmap_ and initialize _Lmap_
        if texmap != String::from("") {
            let file = std::fs::File::open(texmap.clone()).unwrap();
            let reader = BufReader::new(file);
            let img_result = image::hdr::HDRDecoder::with_strictness(reader, false);
            if img_result.is_ok() {
                if let Some(hdr) = img_result.ok() {
                    let meta = hdr.metadata();
                    let resolution: Point2i = Point2i {
                        x: meta.width as i32,
                        y: meta.height as i32,
                    };
                    println!("resolution = {:?}", resolution);
                    let img_result =
                        hdr.read_image_transform(|p| {
                                                     let rgb = p.to_hdr();
                                                     Spectrum::rgb(rgb[0], rgb[1], rgb[2]) * *l
                                                 });
                    if img_result.is_ok() {
                        let texels = img_result.ok().unwrap();
                        // create _MipMap_ from converted texels (see above)
                        let do_trilinear: bool = false;
                        let max_aniso: Float = 8.0 as Float;
                        let wrap_mode: ImageWrap = ImageWrap::Repeat;
                        let lmap = Arc::new(MipMap::new(&resolution,
                                                        &texels[..],
                                                        do_trilinear,
                                                        max_aniso,
                                                        wrap_mode));

                        // initialize sampling PDFs for infinite area light

                        // compute scalar-valued image _img_ from environment map
                        let width: i32 = 2_i32 * lmap.width();
                        let height: i32 = 2_i32 * lmap.height();
                        let mut img: Vec<Float> = Vec::new();
                        let fwidth: Float = 0.5 as Float / (width as Float).min(height as Float);
                        // TODO: ParallelFor(...) {...}
                        for v in 0..height {
                            let vp: Float = (v as Float + 0.5 as Float) / height as Float;
                            let sin_theta: Float =
                                (PI * (v as Float + 0.5 as Float) / height as Float).sin();
                            for u in 0..width {
                                let up: Float = (u as Float + 0.5 as Float) / width as Float;
                                let st: Point2f = Point2f { x: up, y: vp };
                                img.push(lmap.lookup_pnt_flt(&st, fwidth).y() * sin_theta);
                            }
                        }
                        let distribution: Arc<Distribution2D> =
                            Arc::new(Distribution2D::new(img, width, height));
                        return InfiniteAreaLight {
                                   lmap: lmap,
                                   world_center: RwLock::new(Point3f::default()),
                                   world_radius: RwLock::new(0.0),
                                   distribution: distribution,
                                   flags: LightFlags::Infinite as u8,
                                   n_samples: std::cmp::max(1_i32, n_samples),
                                   light_to_world: *light_to_world,
                                   world_to_light: Transform::inverse(*light_to_world),
                               };
                    }
                }
            }
        }
        println!("WARNING: InfiniteAreaLight::new() ... no OpenEXR support !!!");
        InfiniteAreaLight::default(n_samples, l)
    }
    fn default(n_samples: i32, l: &Spectrum) -> Self {
        let resolution: Point2i = Point2i { x: 1_i32, y: 1_i32 };
        let texels: Vec<Spectrum> = vec![*l];
        let do_trilinear: bool = false;
        let max_aniso: Float = 8.0 as Float;
        let wrap_mode: ImageWrap = ImageWrap::Repeat;
        let lmap =
            Arc::new(MipMap::new(&resolution, &texels[..], do_trilinear, max_aniso, wrap_mode));

        // initialize sampling PDFs for infinite area light

        // compute scalar-valued image _img_ from environment map
        let width: i32 = 2_i32 * lmap.width();
        let height: i32 = 2_i32 * lmap.height();
        let mut img: Vec<Float> = Vec::new();
        let fwidth: Float = 0.5 as Float / (width as Float).min(height as Float);
        // TODO: ParallelFor(...) {...}
        for v in 0..height {
            let vp: Float = (v as Float + 0.5 as Float) / height as Float;
            let sin_theta: Float = (PI * (v as Float + 0.5 as Float) / height as Float).sin();
            for u in 0..width {
                let up: Float = (u as Float + 0.5 as Float) / width as Float;
                let st: Point2f = Point2f { x: up, y: vp };
                img.push(lmap.lookup_pnt_flt(&st, fwidth).y() * sin_theta);
            }
        }
        let distribution: Arc<Distribution2D> = Arc::new(Distribution2D::new(img, width, height));
        InfiniteAreaLight {
            lmap: lmap,
            world_center: RwLock::new(Point3f::default()),
            world_radius: RwLock::new(0.0),
            distribution: distribution,
            flags: LightFlags::Infinite as u8,
            n_samples: std::cmp::max(1_i32, n_samples),
            light_to_world: Transform::default(),
            world_to_light: Transform::default(),
        }
    }
}

impl Light for InfiniteAreaLight {
    fn sample_li(&self,
                 iref: &InteractionCommon,
                 u: Point2f,
                 wi: &mut Vector3f,
                 pdf: &mut Float,
                 vis: &mut VisibilityTester)
                 -> Spectrum {
        // TODO: ProfilePhase _(Prof::LightSample);
        // find $(u,v)$ sample coordinates in infinite light texture
        let mut map_pdf: Float = 0.0 as Float;
        let uv: Point2f = self.distribution.sample_continuous(&u, &mut map_pdf);
        if map_pdf == 0 as Float {
            return Spectrum::default();
        }
        // convert infinite light sample point to direction
        let theta: Float = uv[1] * PI;
        let phi: Float = uv[0] * 2.0 as Float * PI;
        let cos_theta: Float = theta.cos();
        let sin_theta: Float = theta.sin();
        let sin_phi: Float = phi.sin();
        let cos_phi: Float = phi.cos();
        let vec: Vector3f = Vector3f {
            x: sin_theta * cos_phi,
            y: sin_theta * sin_phi,
            z: cos_theta,
        };
        *wi = self.light_to_world.transform_vector(vec);
        // compute PDF for sampled infinite light direction
        *pdf = map_pdf / (2.0 as Float * PI * PI * sin_theta);
        if sin_theta == 0.0 as Float {
            *pdf = 0.0 as Float;
        }
        // return radiance value for infinite light direction
        let world_radius: Float = *self.world_radius.read().unwrap();
        *vis = VisibilityTester {
            p0: InteractionCommon {
                p: iref.p,
                time: iref.time,
                p_error: iref.p_error,
                wo: iref.wo,
                n: iref.n,
            },
            p1: InteractionCommon {
                p: iref.p + *wi * (2.0 as Float * world_radius),
                time: iref.time,
                p_error: Vector3f::default(),
                wo: Vector3f::default(),
                n: Normal3f::default(),
            },
        };
        // TODO: SpectrumType::Illuminant
        self.lmap.lookup_pnt_flt(&uv, 0.0 as Float)
    }
    /// Like directional lights, the total power from the infinite
    /// area light is related to the surface area of the scene. Like
    /// many other lights the power computed here is approximate.
    fn power(&self) -> Spectrum {
        let p: Point2f = Point2f { x: 0.5, y: 0.5 };
        let world_radius: Float = *self.world_radius.read().unwrap();
        // TODO: SpectrumType::Illuminant
        self.lmap.lookup_pnt_flt(&p, 0.5 as Float) * Spectrum::new(PI * world_radius * world_radius)
    }
    /// Like **DistanceLights**, **InfiniteAreaLights** also need the
    /// scene bounds; here again, the **preprocess()** method finds
    /// the scene bounds after all of the scene geometry has been
    /// created.
    fn preprocess(&self, scene: &Scene) {
        let mut world_center_ref = self.world_center.write().unwrap();
        let mut world_radius_ref = self.world_radius.write().unwrap();
        Bounds3f::bounding_sphere(&scene.world_bound(),
                                  &mut world_center_ref,
                                  &mut world_radius_ref);
    }
    /// Because infinte area lights need to be able to contribute
    /// radiance to rays that don't hit any geometry in the scene,
    /// we'll add a method to the base **Light** class that returns
    /// emitted radiance due to that light along a ray that escapes
    /// the scene bounds. It's the responsibility of the integrators
    /// to call this method for these rays.
    fn le(&self, ray: &mut Ray) -> Spectrum {
        let w: Vector3f = vec3_normalize(self.world_to_light.transform_vector(ray.d));
        let st: Point2f = Point2f {
            x: spherical_phi(&w) * INV_2_PI,
            y: spherical_theta(&w) * INV_PI,
        };
        // TODO: SpectrumType::Illuminant
        self.lmap.lookup_pnt_flt(&st, 0.0 as Float)
    }
    fn pdf_li(&self, _iref: &Interaction, w: Vector3f) -> Float {
        // TODO: ProfilePhase _(Prof::LightPdf);
        let wi: Vector3f = self.world_to_light.transform_vector(w);
        let theta: Float = spherical_theta(&wi);
        let phi: Float = spherical_phi(&wi);
        let sin_theta: Float = theta.sin();
        if sin_theta == 0 as Float {
            return 0 as Float;
        }
        let p: Point2f = Point2f {
            x: phi * INV_2_PI,
            y: theta * INV_PI,
        };
        self.distribution.pdf(&p) / (2.0 as Float * PI * PI * sin_theta)
    }
    fn get_flags(&self) -> u8 {
        self.flags
    }
    fn get_n_samples(&self) -> i32 {
        self.n_samples
    }
}
