<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { sdrState } from '../lib/state.svelte';
  import { wsClient } from '../lib/ws.svelte';
  import type { SpectrumFrame } from '../lib/protocol';

  let canvas: HTMLCanvasElement;
  let container: HTMLDivElement;
  let ctx: CanvasRenderingContext2D | null = null;
  let resizeObserver: ResizeObserver;
  let animId: number = 0;

  // Waterfall image buffer — we store rows as ImageData lines
  let waterfallData: ImageData | null = null;
  let needsRedraw = false;

  const DB_MIN = -120;
  const DB_MAX = 0;
  const MARGIN = { left: 50, right: 16, top: 0, bottom: 28 };

  // Viridis-inspired palette (256 entries)
  const palette: [number, number, number][] = buildViridis();

  function buildViridis(): [number, number, number][] {
    // Simplified viridis: purple → blue → teal → green → yellow
    const stops: [number, number, number, number][] = [
      [0.0,  68, 1, 84],
      [0.25, 59, 82, 139],
      [0.5,  33, 145, 140],
      [0.75, 94, 201, 98],
      [1.0,  253, 231, 37],
    ];
    const out: [number, number, number][] = [];
    for (let i = 0; i < 256; i++) {
      const t = i / 255;
      let si = 0;
      for (let s = 0; s < stops.length - 1; s++) {
        if (t >= stops[s][0] && t <= stops[s + 1][0]) { si = s; break; }
      }
      const seg = stops[si];
      const segN = stops[si + 1];
      const f = (t - seg[0]) / (segN[0] - seg[0]);
      out.push([
        Math.round(seg[1] + f * (segN[1] - seg[1])),
        Math.round(seg[2] + f * (segN[2] - seg[2])),
        Math.round(seg[3] + f * (segN[3] - seg[3])),
      ]);
    }
    return out;
  }

  function dbToColorIndex(db: number): number {
    const clamped = Math.max(DB_MIN, Math.min(DB_MAX, db));
    return Math.round(((clamped - DB_MIN) / (DB_MAX - DB_MIN)) * 255);
  }

  function freqToX(freq: number, plotW: number): number {
    const startFreq = sdrState.centerFreq - sdrState.sampleRate / 2;
    return MARGIN.left + ((freq - startFreq) / sdrState.sampleRate) * plotW;
  }

  function formatFreqLabel(hz: number): string {
    const mhz = hz / 1_000_000;
    if (mhz >= 1) return mhz.toFixed(mhz >= 100 ? 1 : mhz >= 10 ? 2 : 3) + 'M';
    const khz = hz / 1_000;
    return khz.toFixed(1) + 'k';
  }

  export function addSpectrumRow(frame: SpectrumFrame) {
    if (!ctx || !canvas) return;
    const w = canvas.width;
    const h = canvas.height;
    const plotW = w - MARGIN.left - MARGIN.right;
    const plotH = h - MARGIN.top - MARGIN.bottom;
    if (plotW <= 0 || plotH <= 0) return;

    // Initialize or resize waterfallData
    if (!waterfallData || waterfallData.width !== plotW || waterfallData.height !== plotH) {
      waterfallData = ctx.createImageData(plotW, plotH);
      // Fill with black
      for (let i = 3; i < waterfallData.data.length; i += 4) waterfallData.data[i] = 255;
    }

    // Scroll down: copy rows 0..plotH-2 to 1..plotH-1
    const d = waterfallData.data;
    const rowBytes = plotW * 4;
    d.copyWithin(rowBytes, 0, (plotH - 1) * rowBytes);

    // Write new row at top (row 0)
    const bins = frame.data;
    for (let x = 0; x < plotW; x++) {
      const binIdx = Math.floor((x / plotW) * bins.length);
      const ci = dbToColorIndex(bins[Math.min(binIdx, bins.length - 1)]);
      const [r, g, b] = palette[ci];
      const px = x * 4;
      d[px] = r; d[px + 1] = g; d[px + 2] = b; d[px + 3] = 255;
    }

    needsRedraw = true;
  }

  let dragStartX: number | null = null;
  let dragStartFreq: number = 0;

  function handlePointerDown(e: PointerEvent) {
    dragStartX = e.clientX;
    dragStartFreq = sdrState.centerFreq;
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
  }

  function handlePointerMove(e: PointerEvent) {
    if (dragStartX === null) return;
    const rect = canvas.getBoundingClientRect();
    const plotW = canvas.width - MARGIN.left - MARGIN.right;
    const dx = e.clientX - dragStartX;
    const freqDelta = -(dx / plotW) * sdrState.sampleRate;
    // Send tune delta relative to drag start
    const targetFreq = dragStartFreq + freqDelta;
    const delta = targetFreq - sdrState.centerFreq;
    if (Math.abs(delta) > sdrState.sampleRate * 0.005) {
      wsClient.sendCommand({ cmd: 'tune', delta: Math.round(delta) });
    }
  }

  function handlePointerUp(e: PointerEvent) {
    if (dragStartX !== null) {
      const moved = Math.abs(e.clientX - dragStartX);
      if (moved < 5) {
        // Click — tune to frequency
        const rect = canvas.getBoundingClientRect();
        const x = e.clientX - rect.left;
        const plotW = canvas.width - MARGIN.left - MARGIN.right;
        const startFreq = sdrState.centerFreq - sdrState.sampleRate / 2;
        const freq = startFreq + ((x - MARGIN.left) / plotW) * sdrState.sampleRate;
        wsClient.sendCommand({ cmd: 'set_frequency', value: Math.round(freq) });
      }
    }
    dragStartX = null;
  }

  function draw() {
    if (!ctx || !canvas) { animId = requestAnimationFrame(draw); return; }

    const w = canvas.width;
    const h = canvas.height;
    const plotW = w - MARGIN.left - MARGIN.right;
    const plotH = h - MARGIN.top - MARGIN.bottom;

    ctx.clearRect(0, 0, w, h);
    ctx.fillStyle = '#0d0d1a';
    ctx.fillRect(0, 0, w, h);

    // Draw waterfall image
    if (waterfallData && waterfallData.width === plotW && waterfallData.height === plotH) {
      ctx.putImageData(waterfallData, MARGIN.left, MARGIN.top);
    }

    // Cursor line (subtle)
    if (sdrState.cursorActive) {
      const cx = freqToX(sdrState.cursorFreq, plotW);
      ctx.strokeStyle = 'rgba(255,221,0,0.4)';
      ctx.lineWidth = 1;
      ctx.setLineDash([4, 4]);
      ctx.beginPath(); ctx.moveTo(cx, MARGIN.top); ctx.lineTo(cx, MARGIN.top + plotH); ctx.stroke();
      ctx.setLineDash([]);
    }

    // VFOs
    for (const vfo of sdrState.vfos) {
      const vx = freqToX(vfo.freq, plotW);
      ctx.strokeStyle = vfo.color + '88';
      ctx.lineWidth = 1;
      ctx.beginPath(); ctx.moveTo(vx, MARGIN.top); ctx.lineTo(vx, MARGIN.top + plotH); ctx.stroke();
    }

    // Freq labels at bottom
    const startFreq = sdrState.centerFreq - sdrState.sampleRate / 2;
    const endFreq = sdrState.centerFreq + sdrState.sampleRate / 2;
    const rawStep = sdrState.sampleRate / 8;
    const mag = Math.pow(10, Math.floor(Math.log10(rawStep)));
    const norm = rawStep / mag;
    const niceStep = (norm <= 1.5 ? 1 : norm <= 3.5 ? 2 : norm <= 7.5 ? 5 : 10) * mag;

    ctx.font = '10px JetBrains Mono, monospace';
    ctx.fillStyle = '#666';
    ctx.textAlign = 'center';
    ctx.textBaseline = 'top';
    const firstTick = Math.ceil(startFreq / niceStep) * niceStep;
    for (let f = firstTick; f <= endFreq; f += niceStep) {
      const x = freqToX(f, plotW);
      ctx.fillText(formatFreqLabel(f), x, h - MARGIN.bottom + 4);
    }

    needsRedraw = false;
    animId = requestAnimationFrame(draw);
  }

  /** Clear waterfall data (called on reconnect). */
  export function clear() {
    waterfallData = null;
    needsRedraw = true;
  }

  function resize() {
    if (!container || !canvas) return;
    const rect = container.getBoundingClientRect();
    canvas.width = Math.round(rect.width);
    canvas.height = Math.round(rect.height);
    canvas.style.width = rect.width + 'px';
    canvas.style.height = rect.height + 'px';
    ctx = canvas.getContext('2d');
    waterfallData = null; // force re-init on next frame
  }

  onMount(() => {
    ctx = canvas.getContext('2d');
    resizeObserver = new ResizeObserver(resize);
    resizeObserver.observe(container);
    resize();
    animId = requestAnimationFrame(draw);
  });

  onDestroy(() => {
    cancelAnimationFrame(animId);
    resizeObserver?.disconnect();
  });
</script>

<div class="waterfall-container" bind:this={container}>
  <canvas
    bind:this={canvas}
    onpointerdown={handlePointerDown}
    onpointermove={handlePointerMove}
    onpointerup={handlePointerUp}
  ></canvas>
</div>

<style>
  .waterfall-container {
    width: 100%;
    height: 100%;
    position: relative;
    cursor: crosshair;
  }
  canvas {
    display: block;
    position: absolute;
    top: 0;
    left: 0;
    width: 100%;
    height: 100%;
  }
</style>
