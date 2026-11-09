import { api } from '../api/client';
import type { SpeechProvider } from '../api/types';

/** Minimal typing of the Web Speech API (not in lib.dom for every TS version). */
interface SR extends EventTarget {
  lang: string;
  continuous: boolean;
  interimResults: boolean;
  start(): void;
  stop(): void;
  abort(): void;
  onresult: ((e: { resultIndex: number; results: ArrayLike<ArrayLike<{ transcript: string }> & { isFinal: boolean }> }) => void) | null;
  onerror: ((e: { error: string }) => void) | null;
  onend: (() => void) | null;
}
type SRCtor = new () => SR;

const srCtor = (): SRCtor | null => {
  const w = window as unknown as { SpeechRecognition?: SRCtor; webkitSpeechRecognition?: SRCtor };
  return w.SpeechRecognition ?? w.webkitSpeechRecognition ?? null;
};

export function voiceSupport(p: SpeechProvider | undefined): { ok: boolean; why?: string } {
  const provider = p?.type ?? 'browser';
  if (provider === 'disabled') return { ok: false, why: 'Voice is turned off for this workspace.' };
  if (provider === 'browser' && !srCtor()) return { ok: false, why: "This browser has no speech recognition. Type instead, or ask an admin to set up Whisper." };
  if (provider === 'whisper' && !(navigator.mediaDevices && 'MediaRecorder' in window)) return { ok: false, why: 'This browser cannot record audio.' };
  return { ok: true };
}

export interface VoiceSession {
  /** Stop listening; resolves with the final transcript. */
  stop(): Promise<string>;
  cancel(): void;
}

export interface VoiceHandlers {
  onInterim(text: string): void;
  /** 0..1 levels for the waveform (32 bars). */
  onLevels(levels: number[]): void;
  onError(msg: string): void;
}

/** Starts listening with the workspace's speech provider. Always draws a waveform when the mic is reachable. */
export async function startVoice(p: SpeechProvider | undefined, h: VoiceHandlers): Promise<VoiceSession> {
  const provider = p?.type ?? 'browser';
  let stream: MediaStream | null = null;
  let ctx: AudioContext | null = null;
  let raf = 0;
  try {
    stream = await navigator.mediaDevices.getUserMedia({ audio: true });
    ctx = new AudioContext();
    const an = ctx.createAnalyser();
    an.fftSize = 128;
    ctx.createMediaStreamSource(stream).connect(an);
    const buf = new Uint8Array(an.frequencyBinCount);
    const loop = () => {
      an.getByteFrequencyData(buf);
      const bars = Array.from({ length: 32 }, (_, i) => (buf[Math.min(buf.length - 1, 2 + i)] ?? 0) / 255);
      h.onLevels(bars);
      raf = requestAnimationFrame(loop);
    };
    loop();
  } catch {
    if (provider === 'whisper') {
      h.onError('Microphone access was denied.');
      throw new Error('mic denied');
    }
    // Browser STT may still work without our own stream; fake a gentle waveform.
    const t0 = performance.now();
    const loop = () => {
      const t = (performance.now() - t0) / 300;
      h.onLevels(Array.from({ length: 32 }, (_, i) => 0.15 + 0.12 * Math.abs(Math.sin(t + i / 3))));
      raf = requestAnimationFrame(loop);
    };
    loop();
  }
  const cleanup = () => {
    cancelAnimationFrame(raf);
    stream?.getTracks().forEach((t) => t.stop());
    void ctx?.close().catch(() => {});
  };

  if (provider === 'whisper' && stream) {
    const rec = new MediaRecorder(stream);
    const chunks: Blob[] = [];
    rec.ondataavailable = (e) => e.data.size && chunks.push(e.data);
    rec.start();
    h.onInterim('Listening… release to send');
    return {
      stop: () =>
        new Promise((resolve) => {
          rec.onstop = async () => {
            cleanup();
            try {
              const t = await api.transcribe(new Blob(chunks, { type: rec.mimeType || 'audio/webm' }));
              resolve(t.text.trim());
            } catch (e) {
              h.onError(e instanceof Error ? e.message : 'Transcription failed');
              resolve('');
            }
          };
          rec.stop();
        }),
      cancel: () => {
        rec.onstop = null;
        try {
          rec.stop();
        } catch {
          /* already stopped */
        }
        cleanup();
      },
    };
  }

  const Ctor = srCtor();
  if (!Ctor) {
    cleanup();
    throw new Error('no speech recognition');
  }
  const sr = new Ctor();
  sr.lang = navigator.language || 'en-US';
  sr.continuous = true;
  sr.interimResults = true;
  let final = '';
  let interim = '';
  let ended: (() => void) | null = null;
  sr.onresult = (e) => {
    interim = '';
    for (let i = e.resultIndex; i < e.results.length; i++) {
      const r = e.results[i];
      if (!r) continue;
      const txt = r[0]?.transcript ?? '';
      if (r.isFinal) final += txt;
      else interim += txt;
    }
    h.onInterim((final + interim).trim());
  };
  sr.onerror = (e) => {
    if (e.error !== 'aborted' && e.error !== 'no-speech') h.onError(e.error === 'not-allowed' ? 'Microphone access was denied.' : `Speech error: ${e.error}`);
  };
  sr.onend = () => ended?.();
  sr.start();
  return {
    stop: () =>
      new Promise((resolve) => {
        const done = () => {
          cleanup();
          resolve((final + ' ' + interim).replace(/\s+/g, ' ').trim());
        };
        ended = done;
        try {
          sr.stop();
        } catch {
          done();
        }
        setTimeout(done, 1200); // some browsers never fire onend
      }),
    cancel: () => {
      sr.onend = null;
      try {
        sr.abort();
      } catch {
        /* ignore */
      }
      cleanup();
    },
  };
}

export function speak(text: string) {
  try {
    if (!text || !('speechSynthesis' in window)) return;
    window.speechSynthesis.cancel();
    const u = new SpeechSynthesisUtterance(text);
    u.rate = 1.05;
    u.pitch = 1.2;
    window.speechSynthesis.speak(u);
  } catch {
    /* speech synthesis unavailable */
  }
}
