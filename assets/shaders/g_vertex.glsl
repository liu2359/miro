precision mediump float;
in vec2 position;
in vec2 adjust;
in vec2 tex;
in vec2 underline;
in vec4 bg_color;
in vec4 fg_color;
in float has_color;
in vec2 cursor;
in vec4 cursor_color;

uniform mat4 projection;
uniform bool bg_and_line_layer;

out vec2 o_tex;
out vec4 o_fg_color;
out vec4 o_bg_color;
out float o_has_color;
out vec2 o_underline;
out vec2 o_cursor;
out vec4 o_cursor_color;

void main() {
    o_tex = tex;
    o_has_color = has_color;
    o_fg_color = fg_color;
    o_bg_color = bg_color;
    o_underline = underline;
    o_cursor = cursor;
    o_cursor_color = cursor_color;

    if (bg_and_line_layer) {
      gl_Position = projection * vec4(position, 0.0, 1.0);
    } else {
      gl_Position = projection * vec4(position + adjust, 0.0, 1.0);
    }
}