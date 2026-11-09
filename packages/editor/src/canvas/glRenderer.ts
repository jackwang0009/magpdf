import type { Rect } from '../geometry';
import { isImageBitmap } from './tileProvider';
import type { TilePixels } from './tileProvider';

/**
 * WebGL2 瓦片合成器：只负责“GL 调用”，画什么由 renderPlan 决定。
 * 每块瓦片是一张纹理、一个贴图四边形；一帧几十到上百个 draw call，远在 GPU 预算内。
 * 着色器用 GLSL ES 3.0（《工程规范》第 4 节）。
 */
export type RGBA = readonly [number, number, number, number];

const VERTEX_SHADER = `#version 300 es
in vec2 a_pos;
uniform vec4 u_rect;
uniform vec2 u_viewport;
out vec2 v_uv;
void main() {
  vec2 px = u_rect.xy + a_pos * u_rect.zw;
  vec2 clip = vec2(px.x / u_viewport.x * 2.0 - 1.0, 1.0 - px.y / u_viewport.y * 2.0);
  gl_Position = vec4(clip, 0.0, 1.0);
  v_uv = a_pos;
}
`;

const FRAGMENT_SHADER = `#version 300 es
precision mediump float;
in vec2 v_uv;
uniform sampler2D u_tex;
uniform vec4 u_tint;
out vec4 o_color;
void main() {
  o_color = texture(u_tex, v_uv) * u_tint;
}
`;

const WHITE: RGBA = [1, 1, 1, 1];

export class GlTileRenderer {
  private readonly gl: WebGL2RenderingContext;
  private program: WebGLProgram | null = null;
  private vao: WebGLVertexArrayObject | null = null;
  private uRect: WebGLUniformLocation | null = null;
  private uViewport: WebGLUniformLocation | null = null;
  private uTint: WebGLUniformLocation | null = null;
  private white: WebGLTexture | null = null;
  private width = 0;
  private height = 0;
  private lost = false;
  private readonly lostListeners = new Set<() => void>();
  /** 统计：本帧 draw call 数、纹理上传数 */
  readonly frameStats = { draws: 0, uploads: 0 };

  private constructor(
    private readonly canvas: HTMLCanvasElement,
    gl: WebGL2RenderingContext,
  ) {
    this.gl = gl;
    canvas.addEventListener('webglcontextlost', this.handleLost);
    canvas.addEventListener('webglcontextrestored', this.handleRestored);
    this.setup();
  }

  /** WebGL2 不可用（jsdom、禁用硬件加速的环境）时返回 null，调用方显示提示。 */
  static create(canvas: HTMLCanvasElement): GlTileRenderer | null {
    let gl: WebGL2RenderingContext | null;
    try {
      gl = canvas.getContext('webgl2', {
        alpha: false,
        antialias: false,
        depth: false,
        stencil: false,
        premultipliedAlpha: true,
        preserveDrawingBuffer: false,
        powerPreference: 'high-performance',
      });
    } catch {
      gl = null;
    }
    if (!gl) return null;
    return new GlTileRenderer(canvas, gl);
  }

  get isLost(): boolean {
    return this.lost;
  }

  onContextLost(listener: () => void): () => void {
    this.lostListeners.add(listener);
    return () => this.lostListeners.delete(listener);
  }

  resize(width: number, height: number): void {
    if (this.width === width && this.height === height) return;
    this.width = width;
    this.height = height;
    this.canvas.width = width;
    this.canvas.height = height;
  }

  /** 把瓦片像素上传成纹理；调用方负责在淘汰时 deleteTexture。 */
  uploadTexture(pixels: TilePixels, width: number, height: number): WebGLTexture | null {
    if (this.lost) return null;
    const gl = this.gl;
    const texture = gl.createTexture();
    if (!texture) return null;
    gl.bindTexture(gl.TEXTURE_2D, texture);
    gl.pixelStorei(gl.UNPACK_COLORSPACE_CONVERSION_WEBGL, gl.NONE);
    gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, false);
    gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, false);
    if (isImageBitmap(pixels)) {
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, gl.RGBA, gl.UNSIGNED_BYTE, pixels);
    } else {
      gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);
      const data =
        pixels.data instanceof Uint8Array
          ? pixels.data
          : new Uint8Array(pixels.data.buffer, pixels.data.byteOffset, pixels.data.byteLength);
      gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, width, height, 0, gl.RGBA, gl.UNSIGNED_BYTE, data);
    }
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    this.frameStats.uploads++;
    return texture;
  }

  deleteTexture(texture: unknown): void {
    if (this.lost || !texture) return;
    this.gl.deleteTexture(texture as WebGLTexture);
  }

  beginFrame(clear: RGBA): void {
    const gl = this.gl;
    this.frameStats.draws = 0;
    this.frameStats.uploads = 0;
    if (this.lost || !this.program) return;
    gl.viewport(0, 0, this.width, this.height);
    gl.clearColor(clear[0], clear[1], clear[2], clear[3]);
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.useProgram(this.program);
    gl.bindVertexArray(this.vao);
    gl.uniform2f(this.uViewport, Math.max(1, this.width), Math.max(1, this.height));
    gl.activeTexture(gl.TEXTURE0);
  }

  /** 画一个四边形（设备像素矩形）。texture 为 null 时画纯色（tint）。 */
  drawQuad(rect: Rect, texture: WebGLTexture | null, tint: RGBA = WHITE): void {
    if (this.lost || !this.program) return;
    const gl = this.gl;
    gl.bindTexture(gl.TEXTURE_2D, texture ?? this.white);
    gl.uniform4f(this.uRect, rect.x, rect.y, rect.width, rect.height);
    gl.uniform4f(this.uTint, tint[0], tint[1], tint[2], tint[3]);
    gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
    this.frameStats.draws++;
  }

  endFrame(): void {
    // 不需要 flush：浏览器在合成时提交
  }

  dispose(): void {
    this.canvas.removeEventListener('webglcontextlost', this.handleLost);
    this.canvas.removeEventListener('webglcontextrestored', this.handleRestored);
    this.teardown();
    const ext = this.gl.getExtension('WEBGL_lose_context');
    ext?.loseContext();
  }

  private setup(): void {
    const gl = this.gl;
    const program = gl.createProgram();
    const vs = compile(gl, gl.VERTEX_SHADER, VERTEX_SHADER);
    const fs = compile(gl, gl.FRAGMENT_SHADER, FRAGMENT_SHADER);
    if (!program || !vs || !fs) return;
    gl.attachShader(program, vs);
    gl.attachShader(program, fs);
    gl.linkProgram(program);
    gl.deleteShader(vs);
    gl.deleteShader(fs);
    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
      console.error('着色器链接失败', gl.getProgramInfoLog(program));
      gl.deleteProgram(program);
      return;
    }
    this.program = program;
    this.uRect = gl.getUniformLocation(program, 'u_rect');
    this.uViewport = gl.getUniformLocation(program, 'u_viewport');
    this.uTint = gl.getUniformLocation(program, 'u_tint');
    gl.useProgram(program);
    gl.uniform1i(gl.getUniformLocation(program, 'u_tex'), 0);

    this.vao = gl.createVertexArray();
    gl.bindVertexArray(this.vao);
    const buffer = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([0, 0, 1, 0, 0, 1, 1, 1]), gl.STATIC_DRAW);
    const aPos = gl.getAttribLocation(program, 'a_pos');
    gl.enableVertexAttribArray(aPos);
    gl.vertexAttribPointer(aPos, 2, gl.FLOAT, false, 0, 0);

    this.white = gl.createTexture();
    gl.bindTexture(gl.TEXTURE_2D, this.white);
    gl.texImage2D(
      gl.TEXTURE_2D,
      0,
      gl.RGBA,
      1,
      1,
      0,
      gl.RGBA,
      gl.UNSIGNED_BYTE,
      new Uint8Array([255, 255, 255, 255]),
    );
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST);

    gl.disable(gl.DEPTH_TEST);
    gl.enable(gl.BLEND);
    gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
  }

  private teardown(): void {
    const gl = this.gl;
    if (this.program) gl.deleteProgram(this.program);
    if (this.vao) gl.deleteVertexArray(this.vao);
    if (this.white) gl.deleteTexture(this.white);
    this.program = null;
    this.vao = null;
    this.white = null;
  }

  private readonly handleLost = (event: Event) => {
    event.preventDefault();
    this.lost = true;
    this.program = null;
    this.vao = null;
    this.white = null;
    for (const listener of this.lostListeners) listener();
  };

  private readonly handleRestored = () => {
    this.lost = false;
    this.setup();
  };
}

function compile(gl: WebGL2RenderingContext, type: number, source: string): WebGLShader | null {
  const shader = gl.createShader(type);
  if (!shader) return null;
  gl.shaderSource(shader, source);
  gl.compileShader(shader);
  if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
    console.error('着色器编译失败', gl.getShaderInfoLog(shader));
    gl.deleteShader(shader);
    return null;
  }
  return shader;
}

/** 解析 `#rrggbb` 或 `rgba(r, g, b, a)` 为 0–1 的分量，供 clearColor/tint 使用。 */
export function parseCssColor(color: string): RGBA {
  const hex = /^#([0-9a-f]{6})$/i.exec(color.trim());
  if (hex) {
    const n = parseInt(hex[1]!, 16);
    return [((n >> 16) & 255) / 255, ((n >> 8) & 255) / 255, (n & 255) / 255, 1];
  }
  const rgba = /^rgba?\(([^)]+)\)$/i.exec(color.trim());
  if (rgba) {
    const parts = rgba[1]!.split(',').map((p) => parseFloat(p.trim()));
    const [r = 0, g = 0, b = 0, a = 1] = parts;
    return [r / 255, g / 255, b / 255, a];
  }
  throw new Error(`无法解析颜色：${color}`);
}

/** 预乘 alpha（混合模式用 ONE, ONE_MINUS_SRC_ALPHA） */
export function premultiply(color: RGBA): RGBA {
  return [color[0] * color[3], color[1] * color[3], color[2] * color[3], color[3]];
}
