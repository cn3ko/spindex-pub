"""Apollo restoration runner for SpinDex (SP3).

Lives in binaries/apollo/ next to the Apollo checkout, weights, and venv
(placed there by scripts/setup-apollo.ps1). Invoked by the SpinDex restore
worker as:  venv/Scripts/python.exe run_restore.py --in_wav X --out_wav Y

Prints "PROGRESS n/m" lines on stdout after each inference chunk so the
worker can show real within-track progress. 16-bit PCM 44.1 kHz wav in/out.

Apollo: JusperLee (https://github.com/JusperLee/Apollo), CC BY-SA 4.0.
"""
import argparse
import os
import sys
import wave

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(SCRIPT_DIR, "Apollo"))

import numpy as np
import torch
import look2hear.models

CKPT = os.path.join(SCRIPT_DIR, "pytorch_model.bin")
SR = 44100


def wav_load(path):
    with wave.open(path, "rb") as w:
        n_ch = w.getnchannels()
        sr = w.getframerate()
        if w.getsampwidth() != 2:
            raise ValueError(f"expected 16-bit PCM wav, got sampwidth={w.getsampwidth()}")
        raw = w.readframes(w.getnframes())
    data = np.frombuffer(raw, dtype=np.int16).astype(np.float32) / 32768.0
    data = data.reshape(-1, n_ch).T  # [ch, samples]
    return torch.from_numpy(data.copy()), sr


def wav_save(path, tensor, sr):
    audio = tensor.detach().cpu().clamp(-1.0, 1.0).numpy()
    interleaved = audio.T.reshape(-1)
    pcm16 = (interleaved * 32767.0).astype(np.int16)
    with wave.open(path, "wb") as w:
        w.setnchannels(audio.shape[0])
        w.setsampwidth(2)
        w.setframerate(sr)
        w.writeframes(pcm16.tobytes())


def chunk_positions(n_samples, chunk, stride):
    """Start offsets for overlap-add chunks covering [0, n_samples)."""
    if n_samples <= chunk:
        return [0]
    positions = []
    pos = 0
    while pos < n_samples:
        positions.append(pos)
        if pos + chunk >= n_samples:
            break
        pos += stride
    return positions


def process(model, audio, chunk_sec=10.0, overlap_sec=1.0, sr=SR):
    """Chunk + overlap-add processing to keep CPU memory bounded, with a
    PROGRESS line per chunk."""
    n_ch, n_samples = audio.shape
    chunk = int(chunk_sec * sr)
    overlap = int(overlap_sec * sr)
    stride = chunk - overlap
    positions = chunk_positions(n_samples, chunk, stride)
    total = len(positions)
    print(f"PROGRESS 0/{total}", flush=True)

    if total == 1:
        with torch.no_grad():
            out = model(audio.unsqueeze(0))[0]
        print(f"PROGRESS 1/1", flush=True)
        return out

    out = torch.zeros(n_ch, n_samples, dtype=torch.float32)
    weight = torch.zeros(n_samples, dtype=torch.float32)
    win = torch.ones(chunk, dtype=torch.float32)
    if overlap > 0:
        ramp = torch.linspace(0, 1, overlap)
        win[:overlap] = ramp
        win[-overlap:] = ramp.flip(0)

    for i, pos in enumerate(positions):
        end = min(pos + chunk, n_samples)
        seg = audio[:, pos:end]
        seg_len = seg.shape[-1]
        if seg_len < chunk:
            seg_in = torch.cat([seg, torch.zeros(n_ch, chunk - seg_len)], dim=-1)
        else:
            seg_in = seg
        with torch.no_grad():
            seg_out = model(seg_in.unsqueeze(0))[0]
        seg_out = seg_out[:, :seg_len]
        w = win[:seg_len]
        out[:, pos:end] += seg_out * w
        weight[pos:end] += w
        print(f"PROGRESS {i + 1}/{total}", flush=True)

    weight = weight.clamp(min=1e-8)
    return out / weight.unsqueeze(0)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--in_wav", required=True)
    ap.add_argument("--out_wav", required=True)
    ap.add_argument("--chunk_sec", type=float, default=10.0)
    args = ap.parse_args()

    print("Loading model...", flush=True)
    model = look2hear.models.BaseModel.from_pretrain(
        CKPT, sr=SR, win=20, feature_dim=256, layer=6
    )
    model.eval()

    audio, sr = wav_load(args.in_wav)
    if sr != SR:
        raise ValueError(f"expected {SR}Hz input, got {sr}Hz (resample with ffmpeg first)")

    out = process(model, audio, chunk_sec=args.chunk_sec)
    wav_save(args.out_wav, out, sr)
    print(f"Saved: {args.out_wav}", flush=True)


if __name__ == "__main__":
    main()
