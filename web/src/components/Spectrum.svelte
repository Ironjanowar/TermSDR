<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { sdrState } from '../lib/state.svelte';
  import { wsClient } from '../lib/ws.svelte';
  import type { SpectrumFrame } from '../lib/protocol';

  let canvas: HTMLCanvasElement;
  let container: HTMLDivElement;
  let ctx: CanvasRenderingContext2D | null = null;
  let animId: number = 0;
  let latestFrame: SpectrumFrame | null = null;
  let resizeObserver: ResizeObserver;

  // Callbacks for waterfall integration
  export let onFrame: ((frame: SpectrumFrame) => void) | undefined = undefined;

  const DB_MIN = -120;
  const DB_MAX = 0;
  const MARGIN = { left: 50, right: 16, top: 10, bottom: 28 };

  function handleSpectrum(frame: SpectrumFrame) {
    latestFrame = frame;
    onFrame?.(frame);
  }

  function freqToX(freq: number, plotW: number): number {
    const startFreq = sdrState.centerFreq - sdrState.sampleRate / 2;
    return MARGIN.left + ((freq - startFreq) / sdrState.sampleRate) * plotW;
  }

  function xToFreq(x: number, plotW: number): number {
    const startFreq = sdrState.centerFreq - sdrState.sampleRate / 2;
    return startFreq + ((x - MARGIN.left) / plotW) * sdrState.sampleRate;
  }

  function formatFreqLabel(hz: number): string {
    const mhz = hz / 1_000_000;
    if (mhz >= 1) return mhz.toFixed(mhz >= 100 ? 1 : mhz >= 10 ? 2 : 3) + 'M';
    const khz = hz / 1_000;
    return khz.toFixed(1) + 'k';
  }

  function draw() {
    if (!ctx || !canvas) { animId = requestAnimationFrame(draw); return; }

    const w = canvas.width;
    const h = canvas.height;
    const plotW = w - MARGIN.left - MARGIN.right;
    const plotH = h - MARGIN.top - MARGIN.bottom;

    ctx.clearRect(0, 0, w, h);

    // Background
    ctx.fillStyle = '#0d0d1a';
    ctx.fillRect(0, 0, w, h);

    // Grid
    ctx.strokeStyle = '#1a1a2e';
    ctx.lineWidth = 1;
    ctx.font = '10px JetBrains Mono, monospace';

    // dB grid lines & labels
    const dbStep = 20;
    ctx.fillStyle = '#666';
    ctx.textAlign = 'right';
    ctx.textBaseline = 'middle';
    for (let db = DB_MIN; db <= DB_MAX; db += dbStep) {
      const y = MARGIN.top + plotH * (1 - (db - DB_MIN) / (DB_MAX - DB_MIN));
      ctx.beginPath();
      ctx.moveTo(MARGIN.left, y);
      ctx.lineTo(w - MARGIN.right, y);
      ctx.stroke();
      ctx.fillText(`${db}`, MARGIN.left - 4, y);
    }

    // Frequency grid lines & labels
    const startFreq = sdrState.centerFreq - sdrState.sampleRate / 2;
    const endFreq = sdrState.centerFreq + sdrState.sampleRate / 2;
    const freqRange = sdrState.sampleRate;
    // Choose a nice step
    const rawStep = freqRange / 8;
    const mag = Math.pow(10, Math.floor(Math.log10(rawStep)));
    const norm = rawStep / mag;
    const niceStep = (norm <= 1.5 ? 1 : norm <= 3.5 ? 2 : norm <= 7.5 ? 5 : 10) * mag;

    ctx.textAlign = 'center';
    ctx.textBaseline = 'top';
    const firstTick = Math.ceil(startFreq / niceStep) * niceStep;
    for (let f = firstTick; f <= endFreq; f += niceStep) {
      const x = freqToX(f, plotW);
      ctx.beginPath();
      ctx.strokeStyle = '#1a1a2e';
      ctx.moveTo(x, MARGIN.top);
      ctx.lineTo(x, h - MARGIN.bottom);
      ctx.stroke();
      ctx.fillStyle = '#666';
      ctx.fillText(formatFreqLabel(f), x, h - MARGIN.bottom + 4);
    }

    // Spectrum trace
    if (latestFrame && latestFrame.data.length > 0) {
      const data = latestFrame.data;
      const binCount = data.length;

      ctx.beginPath();
      for (let i = 0; i < binCount; i++) {
        const x = MARGIN.left + (i / (binCount - 1)) * plotW;
        const db = Math.max(DB_MIN, Math.min(DB_MAX, data[i]));
        const y = MARGIN.top + plotH * (1 - (db - DB_MIN) / (DB_MAX - DB_MIN));
        if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
      }
      ctx.strokeStyle = '#00ff88';
      ctx.lineWidth = 1.5;
      ctx.stroke();

      // Gradient fill
      const grad = ctx.createLinearGradient(0, MARGIN.top, 0, MARGIN.top + plotH);
      grad.addColorStop(0, 'rgba(0,255,136,0.3)');
      grad.addColorStop(1, 'rgba(0,255,136,0)');
      ctx.lineTo(MARGIN.left + plotW, MARGIN.top + plotH);
      ctx.lineTo(MARGIN.left, MARGIN.top + plotH);
      ctx.closePath();
      ctx.fillStyle = grad;
      ctx.fill();
    }

    // Channel BW around cursor
    if (sdrState.cursorActive) {
      const bwHalf = sdrState.channelBw / 2;
      const cxL = freqToX(sdrState.cursorFreq - bwHalf, plotW);
      const cxR = freqToX(sdrState.cursorFreq + bwHalf, plotW);
      ctx.fillStyle = 'rgba(255,255,0,0.05)';
      ctx.fillRect(cxL, MARGIN.top, cxR - cxL, plotH);
      ctx.setLineDash([4, 4]);
      ctx.strokeStyle = 'rgba(255,255,0,0.4)';
      ctx.lineWidth = 1;
      ctx.beginPath(); ctx.moveTo(cxL, MARGIN.top); ctx.lineTo(cxL, MARGIN.top + plotH); ctx.stroke();
      ctx.beginPath(); ctx.moveTo(cxR, MARGIN.top); ctx.lineTo(cxR, MARGIN.top + plotH); ctx.stroke();
      ctx.setLineDash([]);

      // Cursor line
      const cx = freqToX(sdrState.cursorFreq, plotW);
      ctx.setLineDash([6, 3]);
      ctx.strokeStyle = '#ffdd00';
      ctx.lineWidth = 1.5;
      ctx.beginPath(); ctx.moveTo(cx, MARGIN.top); ctx.lineTo(cx, MARGIN.top + plotH); ctx.stroke();
      ctx.setLineDash([]);

      // Cursor label
      ctx.fillStyle = '#ffdd00';
      ctx.font = '11px JetBrains Mono, monospace';
      ctx.textAlign = 'center';
      ctx.textBaseline = 'bottom';
      ctx.fillText(formatFreqLabel(sdrState.cursorFreq), cx, MARGIN.top - 1);
    }

    // VFOs
    for (const vfo of sdrState.vfos) {
      const vx = freqToX(vfo.freq, plotW);
      ctx.strokeStyle = vfo.color;
      ctx.lineWidth = 1.5;
      ctx.beginPath(); ctx.moveTo(vx, MARGIN.top); ctx.lineTo(vx, MARGIN.top + plotH); ctx.stroke();

      // VFO channel BW
      const bwH = vfo.channel_bw / 2;
      const vL = freqToX(vfo.freq - bwH, plotW);
      const vR = freqToX(vfo.freq + bwH, plotW);
      ctx.setLineDash([3, 3]);
      ctx.strokeStyle = vfo.color + '88';
      ctx.lineWidth = 1;
      ctx.beginPath(); ctx.moveTo(vL, MARGIN.top); ctx.lineTo(vL, MARGIN.top + plotH); ctx.stroke();
      ctx.beginPath(); ctx.moveTo(vR, MARGIN.top); ctx.lineTo(vR, MARGIN.top + plotH); ctx.stroke();
      ctx.setLineDash([]);
    }

    animId = requestAnimationFrame(draw);
  }

  function handleClick(e: MouseEvent) {
    const rect = canvas.getBoundingClientRect();
    const x = e.clientX - rect.left;
    const plotW = canvas.width - MARGIN.left - MARGIN.right;
    const freq = xToFreq(x, plotW);
    wsClient.sendCommand({ cmd: 'set_frequency', value: Math.round(freq) });
  }

  function handleWheel(e: WheelEvent) {
    e.preventDefault();
    wsClient.sendCommand({ cmd: 'zoom', direction: e.deltaY < 0 ? 'in' : 'out' });
  }

  function resize() {
    if (!container || !canvas) return;
    const rect = container.getBoundingClientRect();
    // Use 1:1 pixel mapping — canvas dimensions = CSS dimensions.
    // Avoids DPR scaling bugs and simplifies all draw coordinates.
    canvas.width = Math.round(rect.width);
    canvas.height = Math.round(rect.height);
    canvas.style.width = rect.width + 'px';
    canvas.style.height = rect.height + 'px';
    ctx = canvas.getContext('2d');
  }

  onMount(() => {
    ctx = canvas.getContext('2d');
    resizeObserver = new ResizeObserver(resize);
    resizeObserver.observe(container);
    resize();
    wsClient.onSpectrum = handleSpectrum;
    animId = requestAnimationFrame(draw);
  });

  onDestroy(() => {
    cancelAnimationFrame(animId);
    resizeObserver?.disconnect();
    if (wsClient.onSpectrum === handleSpectrum) wsClient.onSpectrum = null;
  });
</script>

<div class="spectrum-container" bind:this={container}>
  <canvas
    bind:this={canvas}
    onclick={handleClick}
    onwheel={handleWheel}
  ></canvas>
</div>

<style>
  .spectrum-container {
    width: 100%;
    height: 100%;
    position: relative;
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
