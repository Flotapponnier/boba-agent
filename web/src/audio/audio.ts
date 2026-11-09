import { asset } from '../lib/game';
import { usePrefs } from '../store/prefs';

const SFX = {
  pearl: 'sfx/pearl_collected.mp3',
  done: 'sfx/game_complete.mp3',
  blocked: 'sfx/blocked_movement.mp3',
  timeout: 'sfx/time_out.mp3',
  mold: 'sfx/mold_over.mp3',
} as const;
export type SfxName = keyof typeof SFX;

const lastPlayed: Partial<Record<SfxName, number>> = {};

export function sfx(name: SfxName, gain = 1) {
  const vol = usePrefs.getState().sfxVolume * gain;
  if (vol <= 0) return;
  // Avoid machine-gun repeats when several Fx arrive together.
  const t = performance.now();
  if ((lastPlayed[name] ?? 0) > t - 120) return;
  lastPlayed[name] = t;
  try {
    const a = new Audio(asset(SFX[name]));
    a.volume = Math.min(1, vol * 0.75);
    void a.play().catch(() => {});
  } catch {
    /* audio unavailable */
  }
}

/**
 * Original chiptune loop (Web Audio, procedurally scheduled): bouncy triangle bass,
 * square lead, noise hats. Melody is our own, never copied from any game.
 */
class Music {
  private ctx: AudioContext | null = null;
  private master: GainNode | null = null;
  private noise: AudioBuffer | null = null;
  private timer: ReturnType<typeof setInterval> | undefined;
  private step = 0;
  private next = 0;
  playing = false;

  private static BPM = 138;
  private static EIGHTH = 60 / Music.BPM / 2;
  private static LEAD: (number | null)[] = [
    72, 76, 79, 76, 81, 79, 76, 74, 74, 79, 83, 79, 81, 79, 74, 71, 72, 76, 81, 76, 79, 76, 72, 69, 69, 72, 77, 72, 76, 74, 72, null,
    79, null, 79, 81, 79, 76, 72, 76, 74, null, 74, 76, 74, 71, 67, 71, 72, 76, 79, 81, 84, 81, 79, 76, 77, 76, 74, 72, 74, null, 71, null,
  ];
  private static ROOTS = [48, 43, 45, 41, 48, 43, 45, 43];

  private ensure() {
    if (this.ctx) return;
    const Ctor = window.AudioContext ?? (window as unknown as { webkitAudioContext: typeof AudioContext }).webkitAudioContext;
    this.ctx = new Ctor();
    this.master = this.ctx.createGain();
    this.master.gain.value = 0;
    this.master.connect(this.ctx.destination);
    this.noise = this.ctx.createBuffer(1, Math.floor(this.ctx.sampleRate * 0.05), this.ctx.sampleRate);
    const d = this.noise.getChannelData(0);
    for (let i = 0; i < d.length; i++) d[i] = Math.random() * 2 - 1;
  }

  private tone(type: OscillatorType, midi: number, t: number, dur: number, vol: number) {
    const ctx = this.ctx as AudioContext;
    const o = ctx.createOscillator();
    const g = ctx.createGain();
    o.type = type;
    o.frequency.value = 440 * Math.pow(2, (midi - 69) / 12);
    g.gain.setValueAtTime(0, t);
    g.gain.linearRampToValueAtTime(vol, t + 0.01);
    g.gain.exponentialRampToValueAtTime(0.0001, t + dur);
    o.connect(g).connect(this.master as GainNode);
    o.start(t);
    o.stop(t + dur + 0.02);
  }

  private hat(t: number, vol: number) {
    const ctx = this.ctx as AudioContext;
    const s = ctx.createBufferSource();
    const g = ctx.createGain();
    const f = ctx.createBiquadFilter();
    s.buffer = this.noise;
    f.type = 'highpass';
    f.frequency.value = 7000;
    g.gain.setValueAtTime(vol, t);
    g.gain.exponentialRampToValueAtTime(0.0001, t + 0.04);
    s.connect(f).connect(g).connect(this.master as GainNode);
    s.start(t);
    s.stop(t + 0.05);
  }

  private schedule = () => {
    const ctx = this.ctx as AudioContext;
    const E = Music.EIGHTH;
    while (this.next < ctx.currentTime + 0.12) {
      const i = this.step % Music.LEAD.length;
      const bar = Math.floor(i / 8);
      const beat = i % 8;
      const root = Music.ROOTS[bar] ?? 48;
      const lead = Music.LEAD[i];
      if (lead != null) this.tone('square', lead, this.next, E * 0.9, 0.06);
      this.tone('triangle', [root, root + 12, root + 7, root + 12][beat % 4] ?? root, this.next, E * 0.8, 0.14);
      this.hat(this.next, beat % 2 ? 0.05 : 0.025);
      this.next += E;
      this.step++;
    }
  };

  setVolume(v: number) {
    if (this.master && this.ctx) this.master.gain.setTargetAtTime(v * 0.6, this.ctx.currentTime, 0.1);
  }

  play(volume: number) {
    this.ensure();
    const ctx = this.ctx as AudioContext;
    void ctx.resume();
    this.setVolume(volume);
    if (this.playing) return;
    this.playing = true;
    this.next = ctx.currentTime + 0.05;
    this.timer = setInterval(this.schedule, 25);
  }

  stop() {
    if (!this.playing) return;
    this.playing = false;
    clearInterval(this.timer);
    this.setVolume(0);
    const ctx = this.ctx;
    setTimeout(() => {
      if (!this.playing) void ctx?.suspend();
    }, 300);
  }
}

export const music = new Music();
