//! TEV combiner: WGSL generation from the render state.
//!
//! The shader bakes the stage structure (inputs, operations, destinations, texture and channel
//! selection, swap tables, alpha test); colour registers and konst colours are uniforms, so draws
//! that only change colours share a pipeline.

use std::fmt::Write;

use crate::state::RenderState;

/// Uniform block layout (std140): 4 TEV registers, 4 konst colours, alpha test refs.
pub const UNIFORM_FLOATS: usize = 4 * 4 + 4 * 4 + 4;

/// Identity of the shader `wgsl` would generate for `s`: everything it reads, hashed without allocating.
pub fn shader_key(s: &RenderState) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut mix = |v: u64| h = (h ^ v).wrapping_mul(0x100_0000_01b3);
    mix(s.alpha_compare as u64);
    mix(s.num_tev_stages as u64);
    for st in &s.tev[..s.num_tev_stages as usize] {
        mix(st.color_env as u64);
        mix(st.alpha_env as u64);
        mix(st.texmap as u64
            | (st.texcoord as u64) << 8
            | (st.tex_enable as u64) << 16
            | (st.chan as u64) << 24);
        mix(st.konst_color as u64 | (st.konst_alpha as u64) << 8);
    }
    for sw in s.swaps {
        mix(u32::from_le_bytes(sw) as u64);
    }
    h
}

pub fn uniforms(s: &RenderState) -> [f32; UNIFORM_FLOATS] {
    let mut u = [0.0f32; UNIFORM_FLOATS];
    for (i, r) in s.tev_regs.iter().enumerate() {
        for k in 0..4 {
            u[i * 4 + k] = r[k] as f32 / 255.0;
        }
    }
    for (i, r) in s.konst.iter().enumerate() {
        for k in 0..4 {
            u[16 + i * 4 + k] = r[k] as f32 / 255.0;
        }
    }
    u[32] = (s.alpha_compare & 0xFF) as f32;
    u[33] = (s.alpha_compare >> 8 & 0xFF) as f32;
    u
}

const KONST_FRACTIONS: [&str; 8] = [
    "1.0", "0.875", "0.75", "0.625", "0.5", "0.375", "0.25", "0.125",
];

/// Expression (vec3) for a konst colour selection.
fn konst_rgb(sel: u8) -> String {
    match sel {
        0..=7 => format!("vec3<f32>({})", KONST_FRACTIONS[sel as usize]),
        12..=15 => format!("u.konst[{}].rgb", sel - 12),
        16..=19 => format!("vec3<f32>(u.konst[{}].r)", sel - 16),
        20..=23 => format!("vec3<f32>(u.konst[{}].g)", sel - 20),
        24..=27 => format!("vec3<f32>(u.konst[{}].b)", sel - 24),
        28..=31 => format!("vec3<f32>(u.konst[{}].a)", sel - 28),
        _ => "vec3<f32>(1.0)".into(),
    }
}

fn konst_a(sel: u8) -> String {
    match sel {
        0..=7 => format!("f32({})", KONST_FRACTIONS[sel as usize]),
        16..=19 => format!("u.konst[{}].r", sel - 16),
        20..=23 => format!("u.konst[{}].g", sel - 20),
        24..=27 => format!("u.konst[{}].b", sel - 24),
        28..=31 => format!("u.konst[{}].a", sel - 28),
        _ => "f32(1.0)".into(),
    }
}

fn swizzle(var: &str, table: [u8; 4]) -> String {
    let ch = ["x", "y", "z", "w"];
    format!(
        "vec4<f32>({v}.{}, {v}.{}, {v}.{}, {v}.{})",
        ch[table[0] as usize & 3],
        ch[table[1] as usize & 3],
        ch[table[2] as usize & 3],
        ch[table[3] as usize & 3],
        v = var
    )
}

const REGS: [&str; 4] = ["prev", "r0", "r1", "r2"];

fn compare(cond_gt: bool, a: &str, b: &str) -> String {
    if cond_gt {
        format!("{a} > {b}")
    } else {
        format!("{a} == {b}")
    }
}

/// Fragment-side channel read for the colour arg `code`.
fn color_arg(code: u32, st: &crate::state::TevStage) -> String {
    match code {
        0 => "prev.rgb".into(),
        1 => "vec3<f32>(prev.a)".into(),
        2 => "r0.rgb".into(),
        3 => "vec3<f32>(r0.a)".into(),
        4 => "r1.rgb".into(),
        5 => "vec3<f32>(r1.a)".into(),
        6 => "r2.rgb".into(),
        7 => "vec3<f32>(r2.a)".into(),
        8 => "tex.rgb".into(),
        9 => "vec3<f32>(tex.a)".into(),
        10 => "ras.rgb".into(),
        11 => "vec3<f32>(ras.a)".into(),
        12 => "vec3<f32>(1.0)".into(),
        13 => "vec3<f32>(0.5)".into(),
        14 => konst_rgb(st.konst_color),
        _ => "vec3<f32>(0.0)".into(),
    }
}

fn alpha_arg(code: u32, st: &crate::state::TevStage) -> String {
    match code {
        0 => "prev.a".into(),
        1 => "r0.a".into(),
        2 => "r1.a".into(),
        3 => "r2.a".into(),
        4 => "tex.a".into(),
        5 => "ras.a".into(),
        6 => konst_a(st.konst_alpha),
        _ => "f32(0.0)".into(),
    }
}

fn bias(b: u32) -> &'static str {
    match b {
        1 => "0.5",
        2 => "-0.5",
        _ => "0.0",
    }
}

fn scale(sc: u32) -> &'static str {
    match sc {
        1 => "2.0",
        2 => "4.0",
        3 => "0.5",
        _ => "1.0",
    }
}

fn cmp_fn(mode: u32) -> &'static str {
    // GX compare functions: never, less, equal, lequal, greater, nequal, gequal, always
    match mode {
        0 => "false",
        1 => "(a < r)",
        2 => "(a == r)",
        3 => "(a <= r)",
        4 => "(a > r)",
        5 => "(a != r)",
        6 => "(a >= r)",
        _ => "true",
    }
}

/// Texture coordinate sets the vertex stream carries: up to the highest one a textured stage reads.
pub fn tex_inputs(s: &RenderState) -> usize {
    s.tev[..s.num_tev_stages as usize]
        .iter()
        .filter(|st| st.tex_enable)
        .map(|st| (st.texcoord as usize & 7) + 1)
        .max()
        .unwrap_or(0)
}

/// Bytes per vertex with `n` texture coordinate sets: position (4 f32), two colours (4 x u8 each), n x 3 f32.
pub const fn vertex_stride(n: usize) -> usize {
    24 + 12 * n
}

pub fn wgsl(s: &RenderState) -> String {
    let mut o = String::new();
    o.push_str(
        "struct Tev { regs: array<vec4<f32>, 4>, konst: array<vec4<f32>, 4>, atest: vec4<f32> };\n\
         @group(0) @binding(0) var<uniform> u: Tev;\n",
    );
    for i in 0..8 {
        let _ = writeln!(o, "@group(1) @binding({i}) var t{i}: texture_2d<f32>;");
        let _ = writeln!(o, "@group(1) @binding({}) var s{i}: sampler;", 8 + i);
    }
    // vertex inputs: position, two 8-bit colours and only the texture coordinate sets a stage reads
    let n = tex_inputs(s);
    o.push_str("struct VsIn { @location(0) pos: vec4<f32>, @location(1) c0: vec4<f32>, @location(2) c1: vec4<f32>");
    for i in 0..n {
        let _ = write!(o, ", @location({}) u{i}: vec3<f32>", 3 + i);
    }
    o.push_str(" };\n");
    o.push_str(
        "struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) c0: vec4<f32>, @location(1) c1: vec4<f32>,\n\
           @location(2) u0: vec3<f32>, @location(3) u1: vec3<f32>, @location(4) u2: vec3<f32>, @location(5) u3: vec3<f32>,\n\
           @location(6) u4: vec3<f32>, @location(7) u5: vec3<f32>, @location(8) u6: vec3<f32>, @location(9) u7: vec3<f32> };\n\
         @vertex fn vs(i: VsIn) -> VsOut {\n\
           var o: VsOut;\n\
           o.pos = i.pos; o.c0 = i.c0; o.c1 = i.c1;\n",
    );
    for k in 0..8 {
        if k < n {
            let _ = writeln!(o, "  o.u{k} = i.u{k};");
        } else {
            let _ = writeln!(o, "  o.u{k} = vec3<f32>(0.0, 0.0, 1.0);");
        }
    }
    o.push_str("  return o;\n}\n");
    o.push_str("@fragment fn fs(i: VsOut) -> @location(0) vec4<f32> {\n");
    o.push_str(
        "  var prev = u.regs[0]; var r0 = u.regs[1]; var r1 = u.regs[2]; var r2 = u.regs[3];\n",
    );
    o.push_str("  var tex = vec4<f32>(1.0); var ras = vec4<f32>(1.0);\n");
    let uvs = [
        "i.u0", "i.u1", "i.u2", "i.u3", "i.u4", "i.u5", "i.u6", "i.u7",
    ];

    for n in 0..s.num_tev_stages as usize {
        o.push_str("  {\n");
        let st = &s.tev[n];
        let (ce, ae) = (st.color_env, st.alpha_env);
        let _ = writeln!(o, "  // stage {n}");
        // texture and rasterised colour inputs
        if st.tex_enable {
            let (m, c) = (st.texmap as usize & 7, st.texcoord as usize & 7);
            let _ = writeln!(o, "  {{ let uv = {uv}; let tc = uv.xy / uv.z; var v = textureSample(t{m}, s{m}, tc); tex = {sw}; }}", uv = uvs[c], sw = swizzle("v", s.swaps[(ae >> 2 & 3) as usize]));
        } else {
            o.push_str("  tex = vec4<f32>(1.0);\n");
        }
        let ras = match st.chan {
            0 => Some("i.c0"),
            1 => Some("i.c1"),
            _ => None,
        };
        match ras {
            Some(c) => {
                let _ = writeln!(
                    o,
                    "  {{ let v = {c}; ras = {}; }}",
                    swizzle("v", s.swaps[(ae & 3) as usize])
                );
            }
            None => o.push_str("  ras = vec4<f32>(0.0);\n"),
        }

        // colour combiner
        let (d, c, b, a) = (ce & 15, ce >> 4 & 15, ce >> 8 & 15, ce >> 12 & 15);
        let (cbias, cop, cclamp, cscale, cdest) = (
            ce >> 16 & 3,
            ce >> 18 & 1,
            ce >> 19 & 1,
            ce >> 20 & 3,
            ce >> 22 & 3,
        );
        let (ia, ib, ic, id) = (
            color_arg(a, st),
            color_arg(b, st),
            color_arg(c, st),
            color_arg(d, st),
        );
        let _ = writeln!(o, "  var cres: vec3<f32>;");
        if cbias == 3 {
            let gt = cop == 0;
            let cond = match cscale {
                0 => compare(
                    gt,
                    &format!("floor(({ia}).r * 255.0 + 0.5)"),
                    &format!("floor(({ib}).r * 255.0 + 0.5)"),
                ),
                1 => {
                    let v = |x: &str| {
                        format!(
                            "(floor(({x}).g * 255.0 + 0.5) * 256.0 + floor(({x}).r * 255.0 + 0.5))"
                        )
                    };
                    compare(gt, &v(&ia), &v(&ib))
                }
                2 => {
                    let v = |x: &str| {
                        format!("(floor(({x}).b * 255.0 + 0.5) * 65536.0 + floor(({x}).g * 255.0 + 0.5) * 256.0 + floor(({x}).r * 255.0 + 0.5))")
                    };
                    compare(gt, &v(&ia), &v(&ib))
                }
                _ => String::new(),
            };
            if cscale == 3 {
                let (ga, gb) = (
                    format!("floor(({ia}) * 255.0 + 0.5)"),
                    format!("floor(({ib}) * 255.0 + 0.5)"),
                );
                let sel = if gt {
                    format!("{ga} > {gb}")
                } else {
                    format!("{ga} == {gb}")
                };
                let _ = writeln!(o, "  cres = {id} + select(vec3<f32>(0.0), {ic}, {sel});");
            } else {
                let _ = writeln!(o, "  cres = {id} + select(vec3<f32>(0.0), {ic}, {cond});");
            }
        } else {
            let sign = if cop == 1 { "-" } else { "" };
            let _ = writeln!(
                o,
                "  cres = ({id} + {sign}(mix({ia}, {ib}, {ic})) + vec3<f32>({})) * {};",
                bias(cbias),
                scale(cscale)
            );
        }
        if cclamp == 1 {
            o.push_str("  cres = clamp(cres, vec3<f32>(0.0), vec3<f32>(1.0));\n");
        } else {
            o.push_str(
                "  cres = clamp(cres, vec3<f32>(-1024.0 / 255.0), vec3<f32>(1023.0 / 255.0));\n",
            );
        }

        // alpha combiner
        let (ad, ac, ab, aa) = (ae >> 4 & 7, ae >> 7 & 7, ae >> 10 & 7, ae >> 13 & 7);
        let (abias, aop, aclamp, ascale, adest) = (
            ae >> 16 & 3,
            ae >> 18 & 1,
            ae >> 19 & 1,
            ae >> 20 & 3,
            ae >> 22 & 3,
        );
        let (xa, xb, xc, xd) = (
            alpha_arg(aa, st),
            alpha_arg(ab, st),
            alpha_arg(ac, st),
            alpha_arg(ad, st),
        );
        if abias == 3 {
            let gt = aop == 0;
            let cond = compare(
                gt,
                &format!("floor(({xa}) * 255.0 + 0.5)"),
                &format!("floor(({xb}) * 255.0 + 0.5)"),
            );
            let _ = writeln!(o, "  var ares = {xd} + select(0.0, {xc}, {cond});");
        } else {
            let sign = if aop == 1 { "-" } else { "" };
            let _ = writeln!(
                o,
                "  var ares = ({xd} + {sign}(mix({xa}, {xb}, {xc})) + {}) * {};",
                bias(abias),
                scale(ascale)
            );
        }
        if aclamp == 1 {
            o.push_str("  ares = clamp(ares, 0.0, 1.0);\n");
        } else {
            o.push_str("  ares = clamp(ares, -1024.0 / 255.0, 1023.0 / 255.0);\n");
        }
        let _ = writeln!(
            o,
            "  {}.r = cres.r; {}.g = cres.g; {}.b = cres.b;",
            REGS[cdest as usize], REGS[cdest as usize], REGS[cdest as usize]
        );
        let _ = writeln!(o, "  {}.a = ares;", REGS[adest as usize]);
        o.push_str("  }\n");
    }

    // alpha test: comp0/ref0 and comp1/ref1 combined with and/or/xor/xnor
    let (c0, c1, logic) = (
        s.alpha_compare >> 16 & 7,
        s.alpha_compare >> 19 & 7,
        s.alpha_compare >> 22 & 3,
    );
    if !(c0 == 7 && c1 == 7 && logic == 0) {
        let _ = writeln!(o, "  {{ let a = floor(prev.a * 255.0 + 0.5);");
        let _ = writeln!(o, "    var r = u.atest.x; let t0 = {};", cmp_fn(c0));
        let _ = writeln!(o, "    r = u.atest.y; let t1 = {};", cmp_fn(c1));
        let pass = match logic {
            0 => "t0 && t1",
            1 => "t0 || t1",
            2 => "t0 != t1",
            _ => "t0 == t1",
        };
        let _ = writeln!(o, "    if (!({pass})) {{ discard; }} }}");
    }
    o.push_str("  return clamp(prev, vec4<f32>(0.0), vec4<f32>(1.0));\n}\n");
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::TevStage;

    #[test]
    fn passthrough_stage_generates_valid_structure() {
        let mut s = RenderState {
            num_tev_stages: 1,
            ..RenderState::default()
        };
        // colour: d = ras.rgb (arg 10), a=b=c=zero; alpha: d = ras.a (arg 5)
        s.tev[0] = TevStage {
            color_env: 10 | 15 << 4 | 15 << 8 | 15 << 12 | 1 << 19,
            alpha_env: 5 << 4 | 7 << 7 | 7 << 10 | 7 << 13 | 1 << 19,
            chan: 0,
            ..Default::default()
        };
        s.swaps = [[0, 1, 2, 3]; 4];
        s.alpha_compare = 7 << 16 | 7 << 19;
        let src = wgsl(&s);
        assert!(src.contains("cres = (ras.rgb + (mix(vec3<f32>(0.0), vec3<f32>(0.0), vec3<f32>(0.0))) + vec3<f32>(0.0)) * 1.0;"), "{src}");
        assert!(src.contains("prev.a = ares;"));
        assert!(
            !src.contains("discard"),
            "always-pass alpha test emits no discard"
        );
    }

    #[test]
    fn uniforms_scale_registers_and_konsts() {
        let mut s = RenderState::default();
        s.tev_regs[1] = [255, 0, -255, 51];
        s.konst[2] = [255, 128, 0, 255];
        s.alpha_compare = 0x80 | 0x10 << 8;
        let u = uniforms(&s);
        assert_eq!(&u[4..8], &[1.0, 0.0, -1.0, 0.2]);
        assert_eq!(u[16 + 8], 1.0);
        assert_eq!((u[32], u[33]), (128.0, 16.0));
    }
}
