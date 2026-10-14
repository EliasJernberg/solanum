#version 300 es
precision mediump float;

// Not part of raindrop-fx: the shade that the rain page laid over the canvas
// to keep the labels readable, the CSS
// radial-gradient(ellipse 50% 40% at 50% 50%, rgba(0, 0, 0, 0.6), rgba(0, 0, 0, 0.35)),
// drawn over the rain with source-over blending.

uniform vec2 uSize; // framebuffer size in pixels

out vec4 fragColor;

void main()
{
    vec2 p = gl_FragCoord.xy / uSize - vec2(0.5);
    float t = clamp(length(p / vec2(0.5, 0.4)), 0.0, 1.0);
    fragColor = vec4(0.0, 0.0, 0.0, mix(0.6, 0.35, t));
}
