#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

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

// The colour to make see-through (unpremultiplied RGB) and how opaque it stays.
uniform vec3 key;
uniform float key_alpha;

// How far `c` is from the key, as the alpha of a layer that, drawn over the key colour,
// gives `c` (GIMP's colour to alpha): 0 for the key itself, 1 for colours that can't be
// made from it.
float distance_alpha(vec3 c) {
    vec3 d = vec3(0.0);
    for (int i = 0; i < 3; i++) {
        float ci = c[i];
        float ki = key[i];
        if (ci > ki) {
            d[i] = ki < 1.0 ? (ci - ki) / (1.0 - ki) : 0.0;
        } else {
            d[i] = ki > 0.0 ? (ki - ci) / ki : 0.0;
        }
    }
    return clamp(max(d.r, max(d.g, d.b)), 0.0, 1.0);
}

void main() {
    // Premultiplied.
    vec4 color = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0);
#endif

    if (color.a > 0.0) {
        vec3 c = color.rgb / color.a;
        // The key colour keeps key_alpha; anything else is unblended from the key, so
        // text, icons and highlights stay as opaque as they were, and their antialiased
        // edges fade into the see-through background.
        float a = key_alpha + (1.0 - key_alpha) * distance_alpha(c);
        // Over the key colour this looks exactly like the original pixel.
        vec3 rgb = max(c - (1.0 - a) * key, vec3(0.0));
        color = vec4(rgb, a) * color.a;
    }

    color = color * alpha;

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}
