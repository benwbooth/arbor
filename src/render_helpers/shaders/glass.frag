#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

// arbor: liquid glass. Draws the captured (optionally blurred) backdrop of one rounded-rectangle
// shape refracted through a convex lens: the backdrop bends inwards in a band along the rim
// (strongly at the edge, not at all in the middle), the colour channels split a little where it
// bends (dispersion), the colours get more vivid (saturation), a tint goes over it and a specular
// highlight lights the rim from the top-left with a weaker reflection at the bottom-right.

precision highp float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform float niri_scale;
// Shape size in logical px.
uniform vec2 geo_size;
// Corner radius in logical px (continuous corners).
uniform float radius;
// v_coords -> [0, 1] across the shape.
uniform mat3 input_to_geo;
// [0, 1] across the shape -> texture coordinates of the captured backdrop.
uniform mat3 geo_to_tex;
// Displacement at the rim, logical px (at most bezel / 2).
uniform float refraction;
// Width of the bending band, logical px.
uniform float bezel;
uniform float dispersion;
uniform float saturation;
// Premultiplied.
uniform vec4 glass_tint;
uniform float rim;

// Rounded box with continuous ("squircle") corners: a p-norm corner (n = 2.6) on a radius
// stretched to match the visible size of a circular one, like Apple's continuous corner curve.
// The same curve as hearth's shaders/glass.frag so a shell-drawn rim lines up.
float sd_glass(vec2 p, vec2 b, float r) {
    r = min(r * 1.18, min(b.x, b.y));
    vec2 q = abs(p) - b + r;
    vec2 m = max(q, 0.0);
    float c = r > 0.0 ? pow(pow(m.x, 2.6) + pow(m.y, 2.6), 1.0 / 2.6) : length(m);
    return min(max(q.x, q.y), 0.0) + c - r;
}

vec3 saturate_color(vec3 color, float sat) {
    const vec3 w = vec3(0.2126, 0.7152, 0.0722);
    return mix(vec3(dot(color, w)), color, sat);
}

vec4 sample_backdrop(vec2 p) {
    vec2 uv = (geo_to_tex * vec3(p / geo_size, 1.0)).xy;
    return texture2D(tex, clamp(uv, 0.0, 1.0));
}

void main() {
    vec3 coords_geo = input_to_geo * vec3(v_coords, 1.0);
    vec2 p = coords_geo.xy * geo_size;
    vec2 half_size = geo_size * 0.5;
    vec2 lp = p - half_size;

    float d = sd_glass(lp, half_size, radius);
    // Half a physical pixel of antialiasing.
    float aa = 0.5 / niri_scale;
    float cover = 1.0 - smoothstep(-aa, aa, d);
    if (cover <= 0.0) {
        gl_FragColor = vec4(0.0);
        return;
    }

    // Outward normal from the distance field. It flips across the crease in the middle where
    // the top and bottom (or left and right) bevels meet, and is zero right on it; an odd-sized
    // shape has a row of pixel centres exactly there, which then lit up as a bright line (and
    // float noise decided per damage rectangle whether a pixel hit zero, so partial redraws
    // left it behind in pieces). Sampling a quarter of a physical pixel off keeps every pixel
    // centre clear of the crease.
    vec2 e = vec2(0.5, 0.0);
    vec2 ln = lp + vec2(0.25 / niri_scale);
    vec2 n = vec2(sd_glass(ln + e.xy, half_size, radius) - sd_glass(ln - e.xy, half_size, radius),
                  sd_glass(ln + e.yx, half_size, radius) - sd_glass(ln - e.yx, half_size, radius));
    n = normalize(n + 1e-6);

    // Bending across the band along the rim: 1 at the rim, 0 where the band ends (and in the
    // flat middle). Quadratic, so the displacement's slope is largest at the rim and the mapping
    // never folds over (refraction <= bezel / 2 keeps the slope under 1): the backdrop is
    // magnified towards the rim like through the rounded edge of a thick lens.
    float s = 1.0 - clamp(-d / max(bezel, 0.001), 0.0, 1.0);
    float bend = s * s;
    vec2 off = -n * bend * refraction;

    vec4 color;
    if (dispersion > 0.0) {
        // Red bends least, blue most (like real glass).
        vec4 g = sample_backdrop(p + off);
        float r_ = sample_backdrop(p + off * (1.0 - dispersion)).r;
        float b_ = sample_backdrop(p + off * (1.0 + dispersion)).b;
        color = vec4(r_, g.g, b_, g.a);
    } else {
        color = sample_backdrop(p + off);
    }

    // Vibrancy.
    color.rgb = saturate_color(color.rgb, saturation);
    color.rgb = clamp(color.rgb, 0.0, color.a);

    // Tint (premultiplied, over).
    color = glass_tint + color * (1.0 - glass_tint.a);

    // Light: from the top-left of the screen; its reflection lights the bottom-right rim less.
    vec2 L = normalize(vec2(-0.55, -0.83));
    float lit = pow(max(dot(n, L), 0.0), 1.6);
    float refl = pow(max(dot(n, -L), 0.0), 2.2);
    float dir = 0.15 + 0.95 * lit + 0.5 * refl;
    // A crisp ~1.5 px specular line just inside the edge...
    float line = 1.0 - smoothstep(1.5 - aa, 1.5 + aa, -d);
    // ...and light gathered in the bending band, fading inwards.
    float glow = bend * 0.35;
    float h = clamp(rim * dir * (0.85 * line + glow), 0.0, 1.0);
    // Screen blend.
    color.rgb = color.rgb + (vec3(color.a) - color.rgb) * h;

    color *= cover * alpha;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
