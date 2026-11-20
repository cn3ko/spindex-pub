"""Generates golden JSON for the Rust mel-spectrogram implementation.

Usage:  py -3.12 scripts/gen-goldens.py
Needs:  pip install torch torchaudio numpy
Output: src-tauri/tests/golden/mel_fbank_rows.json, mel_sine_440.json
"""
import json
from pathlib import Path

import numpy as np
import torch
import torchaudio

OUT = Path(__file__).resolve().parent.parent / "src-tauri" / "tests" / "golden"
OUT.mkdir(parents=True, exist_ok=True)

SR, N_FFT, HOP, N_MELS, F_MIN, F_MAX = 22050, 1024, 441, 128, 30, 11000

# Exactly the beat_this LogMelSpect configuration.
mel = torchaudio.transforms.MelSpectrogram(
    sample_rate=SR, n_fft=N_FFT, hop_length=HOP, f_min=F_MIN, f_max=F_MAX,
    n_mels=N_MELS, mel_scale="slaney", normalized="frame_length", power=1,
)

# Golden 1: three full filterbank rows (0, 64, 127), each 513 floats.
fb = mel.mel_scale.fb.numpy()          # (513, 128)
rows = {str(i): fb[:, i].tolist() for i in (0, 64, 127)}
(OUT / "mel_fbank_rows.json").write_text(json.dumps(rows))

# Golden 2: log-mel of a 1 s 440 Hz sine, beat_this scaling (log1p(1000 x)).
t = np.arange(SR) / SR
sine = (np.sin(2 * np.pi * 440.0 * t) * 0.5).astype(np.float32)
spec = torch.log1p(1000 * mel(torch.from_numpy(sine))).T  # (T, 128)
(OUT / "mel_sine_440.json").write_text(json.dumps({
    "frames": spec.shape[0], "mels": spec.shape[1],
    "data": spec.numpy().flatten().tolist(),
}))
print(f"wrote goldens to {OUT}: sine spec {tuple(spec.shape)}")
