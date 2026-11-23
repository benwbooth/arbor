// arbor: the minimize effect (macOS's Genie, or a plain scale-down) for one window.
//
// Everything is worked out in a "canonical" frame where the dock is below: the window travels
// toward +y. arbor maps the real output into it (a reflection or rotation for docks on the top,
// left or right), so this shader only knows one direction.
//
// The fragment's canonical position is mapped back (inverse of the deformation) to a point of
// the window, which is then looked up in the window's snapshot.

precision highp float;

#if defined(DEBUG_FLAGS)
uniform float niri_tint;
#endif

varying vec2 niri_v_coords;
uniform vec2 niri_size;
uniform float niri_alpha;
uniform float niri_scale;

uniform sampler2D niri_tex;
// Snapshot size in texels.
uniform vec2 tex_size;

// Element coordinates (0..1) to canonical logical coordinates.
uniform mat3 input_to_canon;
// Canonical logical coordinates of a window point to snapshot texture coordinates.
uniform mat3 canon_to_tex;
// Window and target rectangles in canonical coordinates: (x0, y0, x1, y1).
uniform vec4 win;
uniform vec4 tgt;
// 0 = the window, 1 = in its dock slot.
uniform float progress;
// 0 = genie, 1 = scale.
uniform float mode;

float ease(float t) {
    t = clamp(t, 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

// Box-filtered lookup: the window can shrink to a few percent of its size, and the snapshot has
// no mipmaps, so a single tap would sparkle. `foot` is the size of one physical pixel in texture
// coordinates.
vec4 sample_box(vec2 uv, vec2 foot) {
    vec2 tex_px = foot * tex_size;
    // Up to 5x5 taps, spread over the pixel's footprint (when it covers more than a texel).
    vec2 n = clamp(floor(tex_px), vec2(1.0), vec2(5.0));
    vec2 stepv = foot / n;
    vec4 sum = vec4(0.0);
    float count = 0.0;
    for (int i = 0; i < 5; i++) {
        if (float(i) >= n.x) break;
        for (int j = 0; j < 5; j++) {
            if (float(j) >= n.y) break;
            vec2 o = (vec2(float(i), float(j)) + 0.5) * stepv - foot * 0.5;
            sum += texture2D(niri_tex, uv + o);
            count += 1.0;
        }
    }
    return sum / count;
}

void main() {
    vec2 p = (input_to_canon * vec3(niri_v_coords, 1.0)).xy;
    float px = 1.0 / max(niri_scale, 0.001);

    vec2 wsize = win.zw - win.xy;
    vec2 local; // 0..1 within the window
    vec2 foot;  // footprint of one physical pixel, in window units (0..1)
    float cover = 1.0;

    if (mode < 0.5) {
        // Genie. The sides bend into a funnel toward the slot (first ~45%), while the window
        // slides down through it: its bottom edge leads, its top edge follows and catches up.
        float bend = ease(progress / 0.45);
        float top = mix(win.y, tgt.y, ease((progress - 0.15) / 0.85));
        float bot = mix(win.w, tgt.w, ease((progress - 0.05) / 0.65));
        bot = max(bot, top + px);

        // The funnel's shape: wide at the window's top edge, the slot's width from the slot's
        // top edge down. A window already beside the dock still gets a short funnel.
        float c0 = min(win.y, tgt.y - 2.0 * (tgt.w - tgt.y));
        float c = ease((p.y - c0) / max(tgt.y - c0, 1.0)) * bend;
        float l = mix(win.x, tgt.x, c);
        float r = mix(win.z, tgt.z, c);
        r = max(r, l + px);

        local = vec2((p.x - l) / (r - l), (p.y - top) / (bot - top));
        foot = vec2(px / (r - l), px / (bot - top));

        cover *= clamp((p.x - l) / px + 0.5, 0.0, 1.0) * clamp((r - p.x) / px + 0.5, 0.0, 1.0);
        cover *= clamp((p.y - top) / px + 0.5, 0.0, 1.0) * clamp((bot - p.y) / px + 0.5, 0.0, 1.0);
    } else {
        // Scale: the window shrinks into the slot, keeping its shape.
        vec2 tsize = tgt.zw - tgt.xy;
        float k = min(tsize.x / max(wsize.x, 1.0), tsize.y / max(wsize.y, 1.0));
        vec2 fsize = wsize * k;
        vec2 floc = (tgt.xy + tgt.zw) * 0.5 - fsize * 0.5;
        float e = ease(progress);
        vec2 loc = mix(win.xy, floc, e);
        vec2 size = max(mix(wsize, fsize, e), vec2(px));

        local = (p - loc) / size;
        foot = vec2(px) / size;

        vec2 lo = (p - loc) / px + 0.5;
        vec2 hi = (loc + size - p) / px + 0.5;
        cover *= clamp(lo.x, 0.0, 1.0) * clamp(lo.y, 0.0, 1.0);
        cover *= clamp(hi.x, 0.0, 1.0) * clamp(hi.y, 0.0, 1.0);
    }

    if (cover <= 0.0 || local.x < -0.01 || local.x > 1.01 || local.y < -0.01 || local.y > 1.01) {
        gl_FragColor = vec4(0.0);
        return;
    }

    vec2 wpoint = win.xy + clamp(local, 0.0, 1.0) * wsize;
    vec2 uv = (canon_to_tex * vec3(wpoint, 1.0)).xy;
    // Footprint in texture coordinates (axes may be swapped by the frame; use the larger).
    vec2 duv_x = (canon_to_tex * vec3(foot.x * wsize.x, 0.0, 0.0)).xy;
    vec2 duv_y = (canon_to_tex * vec3(0.0, foot.y * wsize.y, 0.0)).xy;
    vec2 tfoot = abs(duv_x) + abs(duv_y);

    vec4 color = sample_box(uv, tfoot);

    // Fades into (out of) the slot over the last 15%, so the end doesn't pop over the icon.
    float fade = 1.0 - ease((progress - 0.85) / 0.15);
    color *= cover * fade * niri_alpha;

#if defined(DEBUG_FLAGS)
    if (niri_tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
